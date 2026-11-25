use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use iroh::endpoint::{Connection, PathEvent, QuicTransportConfig, VarInt, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use iroh_tickets::Ticket;
use iroh_tickets::endpoint::EndpointTicket;

use super::helper::HelperHub;
use super::plaza::Plaza;
use super::protocol::StallHandler;
use super::talk::Talk;
use super::traffic::Traffic;
use crate::storage::load_or_create_secret_key;

pub const ALPN: &[u8] = b"popstall/http/1";
const RELAY_WAIT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const RELAY_NOTICE_AFTER: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct RouteBook {
    live: Mutex<HashMap<String, bool>>,
    /// Someone reached us on a direct path while we were the stall host.
    inbound_direct: AtomicBool,
    /// Human label for the selected path of live connections, e.g. relay host or 直连.
    path_labels: Mutex<HashMap<String, String>>,
}

impl RouteBook {
    pub fn set(&self, id: &str, relay_only: bool) {
        if let Ok(mut live) = self.live.lock() {
            live.insert(id.to_string(), relay_only);
        }
    }

    pub fn clear(&self, id: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(id);
        }
        if let Ok(mut labels) = self.path_labels.lock() {
            labels.remove(id);
        }
    }

    pub fn set_path_label(&self, id: &str, label: String) {
        if let Ok(mut labels) = self.path_labels.lock() {
            labels.insert(id.to_string(), label);
        }
    }

    pub fn path_summary(&self) -> Option<String> {
        self.path_labels
            .lock()
            .ok()
            .and_then(|labels| labels.values().next().cloned())
    }

    pub fn relay_only(&self) -> bool {
        self.live
            .lock()
            .map(|live| live.values().any(|flag| *flag))
            .unwrap_or(false)
    }

    pub fn note_inbound_direct(&self) {
        self.inbound_direct.store(true, Ordering::Relaxed);
    }

    pub fn inbound_direct(&self) -> bool {
        self.inbound_direct.load(Ordering::Relaxed)
    }
}

pub struct ShareMaterial {
    pub code: String,
    pub relay_ready: bool,
    pub endpoint_id: String,
}

pub struct Node {
    pub endpoint: Endpoint,
    /// Kept alive so the accept loop continues after `start` returns.
    #[allow(dead_code)]
    pub router: Router,
    handler: StallHandler,
    traffic: Arc<Traffic>,
    routes: Arc<RouteBook>,
    plaza: Option<Plaza>,
    talk: Option<Talk>,
    helper: Option<HelperHub>,
}

impl Node {
    pub async fn start(data_dir: &Path) -> anyhow::Result<Self> {
        let secret_key = load_or_create_secret_key(data_dir)?;
        let traffic = Arc::new(Traffic::default());
        let routes = Arc::new(RouteBook::default());
        let handler = StallHandler::new(traffic.clone(), routes.clone());
        let endpoint = bind_endpoint(secret_key).await?;
        let mut router = Router::builder(endpoint.clone()).accept(ALPN, handler.clone());
        let (plaza, talk, helper) = if cfg!(test) {
            (None, None, None)
        } else {
            let mdns = super::plaza::attach_mdns(&endpoint);
            let gossip = iroh_gossip::Gossip::builder().spawn(endpoint.clone());
            router = router.accept(iroh_gossip::ALPN, gossip.clone());
            let plaza = super::plaza::start(endpoint.clone(), gossip.clone(), mdns);
            let talk = super::talk::start(endpoint.clone(), gossip.clone(), plaza.clone());
            router = router.accept(super::talk::ALPN, talk.handler());
            let helper = super::helper::start(
                endpoint.clone(),
                gossip,
                plaza.clone(),
                routes.clone(),
                handler.clone(),
            );
            router = router
                .accept(super::helper::ALPN, helper.handler())
                .accept(super::helper::INVITE_ALPN, helper.invite_handler());
            (Some(plaza), Some(talk), Some(helper))
        };
        let router = router.spawn();
        let locals: Vec<String> = endpoint
            .bound_sockets()
            .into_iter()
            .map(|socket| socket.to_string())
            .collect();
        tracing::info!(endpoint_id = %endpoint.id(), ?locals, "本机 UDP 已绑定");
        Ok(Self {
            endpoint,
            router,
            handler,
            traffic,
            routes,
            plaza,
            talk,
            helper,
        })
    }

    pub async fn talk_lines(&self) -> Vec<super::talk::TalkLineDto> {
        match &self.talk {
            Some(talk) => talk.lines().await,
            None => Vec::new(),
        }
    }

    pub async fn talk_say(&self, text: &str, name: &str) -> Result<(), String> {
        let Some(talk) = &self.talk else {
            return Err("夜谈还没有接上".to_string());
        };
        talk.say(text, name).await
    }

    pub async fn helper_status(&self) -> super::helper::HelperStatus {
        match &self.helper {
            Some(helper) => helper.status().await,
            None => super::helper::HelperStatus {
                enabled: false,
                limit_bps: super::helper::DEFAULT_LIMIT_BPS,
                active: 0,
                offers: 0,
                via_helper: false,
                capable: false,
                reach_note: "节点还没接上".to_string(),
            },
        }
    }

    pub async fn helper_set(&self, enabled: bool, limit_bps: u32) -> Result<(), String> {
        let Some(helper) = &self.helper else {
            return Err("互助中继还没有接上".to_string());
        };
        helper.set(enabled, limit_bps).await
    }

    pub async fn helper_offers(
        &self,
        exclude: Option<&str>,
    ) -> Vec<super::helper::HelperOffer> {
        let Some(helper) = &self.helper else {
            return Vec::new();
        };
        let exclude = exclude.and_then(|raw| raw.parse().ok());
        helper.probed_offers(exclude).await
    }

    pub async fn dial_helper_to(
        &self,
        helper_id: &str,
        target: EndpointId,
    ) -> anyhow::Result<Connection> {
        let Some(helper) = &self.helper else {
            anyhow::bail!("互助中继还没有接上");
        };
        let helper_id: EndpointId = helper_id
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("帮手地址无效"))?;
        helper
            .dial_as_helper(helper_id, target, self.routes.clone())
            .await
    }

    pub fn path_summary(&self) -> Option<String> {
        self.routes.path_summary()
    }

    pub fn clear_via_helper(&self) {
        if let Some(helper) = &self.helper {
            helper.set_via_helper(false);
        }
    }

    pub fn traffic(&self) -> Arc<Traffic> {
        self.traffic.clone()
    }

    pub async fn set_host(&self, target: super::HostTarget) {
        self.handler.set_host(target).await;
        if let Some(helper) = &self.helper {
            helper.set_hosting(true);
        }
    }

    pub async fn clear_host(&self) {
        self.handler.clear_host().await;
        if let Some(helper) = &self.helper {
            helper.set_hosting(false);
        }
    }

    pub fn announce_stall(&self, title: &str, label: &str, blurb: &str, locked: bool) {
        if let Some(plaza) = &self.plaza {
            plaza.announce(title, label, blurb, locked);
        }
    }

    pub fn relay_only(&self) -> bool {
        self.routes.relay_only()
    }

    pub fn retract_stall(&self) {
        if let Some(plaza) = &self.plaza {
            plaza.retract();
        }
    }

    pub async fn plaza_snapshot(&self) -> super::plaza::PlazaSnapshot {
        match &self.plaza {
            Some(plaza) => plaza.snapshot(self.endpoint.id()).await,
            None => super::plaza::PlazaSnapshot::default(),
        }
    }

    pub async fn share_material(&self) -> ShareMaterial {
        if !relay_ready(&self.endpoint) {
            let ready = wait_for_relay(&self.endpoint, RELAY_WAIT).await;
            if !ready {
                tracing::warn!("relay address was not ready; the share code will be refreshed once it is");
            }
        }
        self.share_snapshot()
    }

    pub fn share_snapshot(&self) -> ShareMaterial {
        let addr = self.endpoint.addr();
        let ready = addr.relay_urls().next().is_some();
        let relays: Vec<String> = addr.relay_urls().map(|url| url.to_string()).collect();
        let direct: Vec<String> = addr.ip_addrs().map(|socket| socket.to_string()).collect();
        tracing::info!(?relays, ?direct, "本机对外地址");
        ShareMaterial {
            code: EndpointTicket::new(addr).encode_string(),
            relay_ready: ready,
            endpoint_id: self.endpoint.id().to_string(),
        }
    }

    pub async fn connect(&self, share_code: &str) -> anyhow::Result<iroh::endpoint::Connection> {
        let ticket = EndpointTicket::decode_string(share_code.trim())
            .map_err(|err| anyhow::anyhow!("分享码无法解析: {err:?}"))?;
        let addr = ticket.endpoint_addr().clone();
        self.ensure_not_self(addr.id)?;
        let target = addr.id;
        let dial = if addr.relay_urls().next().is_some() {
            addr
        } else {
            tracing::info!("share code has no relay; resolving the endpoint id");
            EndpointAddr::from(target)
        };
        let direct = self.dial(dial).await?;
        Ok(self.maybe_via_helper(target, direct).await)
    }

    pub async fn connect_endpoint(&self, endpoint_id: &str) -> anyhow::Result<iroh::endpoint::Connection> {
        let id: EndpointId = endpoint_id
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("这个摊位的地址无效"))?;
        self.ensure_not_self(id)?;
        let direct = self.dial(EndpointAddr::from(id)).await?;
        Ok(self.maybe_via_helper(id, direct).await)
    }

    async fn maybe_via_helper(&self, target: EndpointId, direct: Connection) -> Connection {
        let Some(helper) = &self.helper else {
            return direct;
        };
        let (connection, via) = super::helper::connect_preferring_helper(
            &self.endpoint,
            &helper.book(),
            target,
            direct,
            self.routes.clone(),
        )
        .await;
        helper.set_via_helper(via);
        connection
    }

    pub fn endpoint_id_of(share_code: &str) -> anyhow::Result<String> {
        let ticket = EndpointTicket::decode_string(share_code.trim())
            .map_err(|err| anyhow::anyhow!("分享码无法解析: {err:?}"))?;
        Ok(ticket.endpoint_addr().id.to_string())
    }

    fn ensure_not_self(&self, id: EndpointId) -> anyhow::Result<()> {
        if id == self.endpoint.id() {
            anyhow::bail!("这是你自己的摊位，用本机预览看");
        }
        Ok(())
    }

    async fn dial(&self, addr: EndpointAddr) -> anyhow::Result<Connection> {
        let relays = addr.relay_urls().count();
        let direct = addr.ip_addrs().count();
        tracing::info!(remote = %addr.id, relays, direct, "开始拨号");
        match tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, ALPN)).await {
            Ok(Ok(connection)) => {
                watch_paths(connection.clone(), "guest", self.routes.clone());
                Ok(connection)
            }
            Ok(Err(err)) => Err(anyhow::anyhow!("P2P 连接失败: {err:?}")),
            Err(_) => Err(anyhow::anyhow!(
                "P2P 连接超时。对方需要正在开张；跨网第一次握手可能要半分钟"
            )),
        }
    }
}

pub(crate) fn watch_paths(connection: Connection, role: &'static str, routes: Arc<RouteBook>) {
    let remote = connection.remote_id();
    let key = remote.to_string();
    tracing::info!(%remote, role, paths = %describe_paths(&connection), "连接已建立");
    if role == "host" && selected_is_relay(&connection) == Some(false) {
        routes.note_inbound_direct();
    }
    {
        let paths = connection.paths();
        for path in &paths {
            if path.is_selected() {
                routes.set_path_label(&key, path_label(path.remote_addr(), role));
                break;
            }
        }
        if role == "helper" {
            routes.set_path_label(&key, "互助中继（直连帮手）".to_string());
        }
    }
    let mut relay_since = (selected_is_relay(&connection) == Some(true)).then(Instant::now);
    tokio::spawn(async move {
        let mut events = connection.path_events();
        loop {
            let wait = notice_wait(relay_since);
            tokio::select! {
                biased;
                event = events.next() => {
                    match event {
                        Some(PathEvent::Opened { remote_addr, .. }) => {
                            log_path(remote, role, &remote_addr, "出现一条路径");
                        }
                        Some(PathEvent::Selected { remote_addr, .. }) => {
                            log_path(remote, role, &remote_addr, "数据改走这条路径");
                            routes.set_path_label(&key, path_label(&remote_addr, role));
                            if remote_addr.is_relay() {
                                relay_since = Some(Instant::now());
                            } else {
                                relay_since = None;
                                routes.set(&key, false);
                                if role == "host" {
                                    routes.note_inbound_direct();
                                }
                            }
                        }
                        Some(PathEvent::Closed { remote_addr, last_stats, .. }) => {
                            tracing::info!(
                                %remote,
                                role,
                                via = %remote_addr,
                                kind = path_kind(&remote_addr),
                                rtt_ms = last_stats.rtt.as_millis() as u64,
                                sent_bytes = last_stats.udp_tx.bytes,
                                recv_bytes = last_stats.udp_rx.bytes,
                                cwnd = last_stats.cwnd,
                                "路径关闭"
                            );
                        }
                        Some(PathEvent::Lagged { missed, .. }) => {
                            tracing::warn!(%remote, role, missed, "路径事件来不及记，漏掉了几条");
                        }
                        Some(_) => {}
                        None => {
                            routes.clear(&key);
                            tracing::info!(%remote, role, "连接关闭");
                            break;
                        }
                    }
                }
                _ = wait => {
                    if relay_since.is_some() {
                        routes.set(&key, true);
                        relay_since = None;
                    }
                }
            }
        }
    });
}

fn notice_wait(since: Option<Instant>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        match since {
            Some(since) => {
                let rest = RELAY_NOTICE_AFTER.saturating_sub(since.elapsed());
                tokio::time::sleep(rest).await;
            }
            None => std::future::pending().await,
        }
    })
}

fn selected_is_relay(connection: &Connection) -> Option<bool> {
    let paths = connection.paths();
    let mut selected = None;
    for path in &paths {
        if path.is_selected() {
            selected = Some(path.is_relay());
        }
    }
    selected
}

fn log_path(remote: EndpointId, role: &str, addr: &TransportAddr, message: &str) {
    if addr.is_relay() {
        tracing::info!(%remote, role, via = %addr, "{message}：中继");
    } else {
        tracing::info!(%remote, role, via = %addr, "{message}：直连");
    }
}

fn path_kind(addr: &TransportAddr) -> &'static str {
    if addr.is_relay() {
        "relay"
    } else if addr.is_ip() {
        "direct"
    } else {
        "other"
    }
}

fn path_label(addr: &TransportAddr, role: &str) -> String {
    if role == "helper" {
        return "互助中继（直连帮手）".to_string();
    }
    if addr.is_relay() {
        let raw = addr.to_string();
        let host = raw
            .strip_prefix("relay:")
            .unwrap_or(&raw)
            .trim_end_matches('/')
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        format!("公共中继 {host}")
    } else if addr.is_ip() {
        "直连".to_string()
    } else {
        "其他路径".to_string()
    }
}

fn describe_paths(connection: &Connection) -> String {
    let paths = connection.paths();
    let mut parts = Vec::new();
    for path in &paths {
        let selected = if path.is_selected() { "*" } else { "" };
        let kind = if path.is_relay() { "中继" } else { "直连" };
        parts.push(format!(
            "{selected}{kind} {} {}ms",
            path.remote_addr(),
            path.rtt().as_millis()
        ));
    }
    if parts.is_empty() {
        "还没有路径".to_string()
    } else {
        parts.join(" | ")
    }
}

fn bulk_transport() -> QuicTransportConfig {
    // Office-to-home traffic often stays on a distant relay. Cubic keeps a tiny
    // window there and the transfer idles between bursts. BBR holds a window
    // sized to the path, and the larger flow-control windows let that through.
    let mut congestion = noq_proto::congestion::Bbr3Config::default();
    congestion.initial_window(256 * 1024);
    QuicTransportConfig::builder()
        .stream_receive_window(VarInt::from_u32(8 * 1024 * 1024))
        .receive_window(VarInt::from_u32(32 * 1024 * 1024))
        .send_window(32 * 1024 * 1024)
        .congestion_controller_factory(Arc::new(congestion))
        .build()
}

fn endpoint_builder(secret_key: iroh::SecretKey) -> iroh::endpoint::Builder {
    Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .transport_config(bulk_transport())
}

async fn bind_endpoint(secret_key: iroh::SecretKey) -> anyhow::Result<Endpoint> {
    if cfg!(test) {
        return endpoint_builder(secret_key)
            .bind()
            .await
            .map_err(|err| anyhow::anyhow!("启动 P2P 节点失败: {err:?}"));
    }
    match endpoint_builder(secret_key.clone())
        .address_lookup(iroh_mainline_address_lookup::DhtAddressLookup::builder())
        .bind()
        .await
    {
        Ok(endpoint) => Ok(endpoint),
        Err(err) => {
            tracing::warn!(?err, "DHT address lookup did not start");
            endpoint_builder(secret_key)
                .bind()
                .await
                .map_err(|err| anyhow::anyhow!("启动 P2P 节点失败: {err:?}"))
        }
    }
}

fn relay_ready(endpoint: &Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
}

async fn wait_for_relay(endpoint: &Endpoint, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if relay_ready(endpoint) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use tokio::io::AsyncReadExt;

    use super::super::HostTarget;
    use super::super::frame::{RequestFrame, encode_request};
    use super::super::io::read_response_head;
    use super::{ALPN, Node};
    use crate::server::StaticHost;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loopback_fetches_index_over_iroh() {
        let dir = std::env::temp_dir().join(format!(
            "popstall-loop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("host-key")).unwrap();
        std::fs::create_dir_all(dir.join("guest-key")).unwrap();
        std::fs::create_dir_all(dir.join("site")).unwrap();
        std::fs::write(dir.join("site/index.html"), b"loopback-ok").unwrap();

        let host = Node::start(&dir.join("host-key")).await.unwrap();
        let site = StaticHost::bind(dir.join("site")).await.unwrap();
        host.set_host(HostTarget {
            router: site.router.clone(),
            password: None,
        })
        .await;

        let mut addr = iroh::EndpointAddr::new(host.endpoint.id());
        for socket in host.endpoint.bound_sockets() {
            let dialable = match socket.ip() {
                IpAddr::V4(ip) if ip.is_unspecified() => {
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), socket.port())
                }
                IpAddr::V6(ip) if ip.is_unspecified() => continue,
                _ => socket,
            };
            addr = addr.with_ip_addr(dialable);
        }

        let guest = Node::start(&dir.join("guest-key")).await.unwrap();
        let connection =
            tokio::time::timeout(Duration::from_secs(20), guest.endpoint.connect(addr, ALPN))
                .await
                .expect("connect timed out")
                .expect("connect failed");
        let (mut send, mut recv) = connection.open_bi().await.expect("open stream");
        let frame = encode_request(&RequestFrame::get("/")).unwrap();
        send.write_all(&frame).await.unwrap();
        send.finish().unwrap();

        let head = read_response_head(&mut recv).await.unwrap();
        assert_eq!(head.status, 200, "{}", head.content_type);
        let mut body = Vec::new();
        AsyncReadExt::read_to_end(&mut recv, &mut body)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("loopback-ok"),
            "{}",
            String::from_utf8_lossy(&body)
        );

        site.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
