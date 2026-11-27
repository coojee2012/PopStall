use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use iroh::endpoint::{Connection, PathEvent, QuicTransportConfig, VarInt, presets};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};
use iroh_tickets::Ticket;
use iroh_tickets::endpoint::EndpointTicket;

use super::helper::HelperHub;
use super::plaza::Plaza;
use super::protocol::StallHandler;
use super::retry::{RetryError, retry_with_backoff, with_deadline};
use super::secure::{PathTrust, SessionId};
use super::talk::Talk;
use super::traffic::Traffic;
use super::visit::VisitHub;
use crate::storage::load_or_create_secret_key;

pub const ALPN: &[u8] = b"popstall/http/1";
const RELAY_WAIT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const HELPER_DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const HELPER_UPGRADE_TIMEOUT: Duration = Duration::from_secs(20);
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(20);
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

#[derive(Clone)]
pub struct GuestLink {
    pub connection: Connection,
    pub trust: PathTrust,
    pub secret: SecretKey,
}

pub struct Node {
    pub endpoint: Endpoint,
    secret: SecretKey,
    /// Kept alive so the accept loop continues after `start` returns.
    #[allow(dead_code)]
    pub router: Router,
    handler: StallHandler,
    traffic: Arc<Traffic>,
    routes: Arc<RouteBook>,
    plaza: Option<Plaza>,
    talk: Option<Talk>,
    helper: Option<HelperHub>,
    visit: Option<VisitHub>,
}

impl Node {
    pub async fn start(data_dir: &Path) -> anyhow::Result<Self> {
        let secret_key = load_or_create_secret_key(data_dir)?;
        let helper_prefs = crate::storage::helper_prefs::load(data_dir);
        let listen_port = crate::storage::helper_prefs::normalize_port(helper_prefs.listen_port);
        let traffic = Arc::new(Traffic::default());
        let routes = Arc::new(RouteBook::default());
        let handler = StallHandler::new(traffic.clone(), routes.clone());
        handler.set_host_identity(secret_key.clone()).await;
        let endpoint = bind_endpoint(secret_key.clone(), listen_port).await?;
        let mut router = Router::builder(endpoint.clone()).accept(ALPN, handler.clone());
        let (plaza, talk, helper, visit) = if cfg!(test) {
            (None, None, None, None)
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
                data_dir.to_path_buf(),
                listen_port,
            );
            let visit = super::visit::VisitHub::new(endpoint.clone(), handler.clone(), listen_port);
            router = router
                .accept(super::helper::ALPN, helper.handler())
                .accept(super::helper::INVITE_ALPN, helper.invite_handler())
                .accept(super::visit::CB_ALPN, visit.callback_handler())
                .accept(super::visit::INVITE_ALPN, visit.invite_handler());
            (Some(plaza), Some(talk), Some(helper), Some(visit))
        };
        let router = router.spawn();
        let locals: Vec<String> = endpoint
            .bound_sockets()
            .into_iter()
            .map(|socket| socket.to_string())
            .collect();
        tracing::info!(endpoint_id = %endpoint.id(), ?locals, "本机 UDP 已绑定");
        crate::netdebug::log(format!(
            "node_bound id={} locals={:?} addr_ips={:?} relays={:?}",
            endpoint.id(),
            locals,
            endpoint
                .addr()
                .ip_addrs()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            endpoint
                .addr()
                .relay_urls()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
        ));
        Ok(Self {
            endpoint,
            secret: secret_key,
            router,
            handler,
            traffic,
            routes,
            plaza,
            talk,
            helper,
            visit,
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
                netdebug_path: crate::netdebug::path_string(),
                listen_port: None,
                bound_ports: Vec::new(),
                port_note: String::new(),
            },
        }
    }

    pub async fn helper_set(
        &self,
        enabled: bool,
        limit_bps: u32,
        listen_port: Option<u16>,
    ) -> Result<(), String> {
        let Some(helper) = &self.helper else {
            return Err("互助中继还没有接上".to_string());
        };
        let port = crate::storage::helper_prefs::normalize_port(listen_port);
        if let Some(visit) = &self.visit {
            visit.set_listen_port(port);
        }
        helper.set(enabled, limit_bps, listen_port).await
    }

    pub async fn helper_offers(&self, exclude: Option<&str>) -> Vec<super::helper::HelperOffer> {
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
    ) -> anyhow::Result<GuestLink> {
        let Some(helper) = &self.helper else {
            anyhow::bail!("互助中继还没有接上");
        };
        let helper_id: EndpointId = helper_id
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("帮手地址无效"))?;
        let result = retry_with_backoff(|attempt| {
            let helper_id = helper_id.clone();
            let target = target.clone();
            async move {
                if attempt > 1 {
                    self.traffic.record_retry();
                }
                with_deadline(HELPER_DIAL_TIMEOUT, async move {
                    helper
                        .dial_as_helper(helper_id, target, self.routes.clone())
                        .await
                })
                .await
            }
        })
        .await;
        match result {
            Ok(connection) => Ok(GuestLink {
                connection,
                trust: PathTrust::HelperBridge {
                    session: SessionId::generate(),
                    guest: self.endpoint.id(),
                    host: target,
                    helper: helper_id,
                },
                secret: self.secret.clone(),
            }),
            Err(RetryError::Exhausted { attempts, source }) => Err(anyhow::anyhow!(
                "互助中继连接失败（{attempts} 次）: {source}"
            )),
            Err(RetryError::Deadline { deadline }) => {
                Err(anyhow::anyhow!("互助中继连接超时（{deadline:?}）"))
            }
            Err(RetryError::Operation(err)) => Err(err),
        }
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

    pub async fn set_integrity(
        &self,
        integrity: Option<Arc<crate::p2p::integrity::IntegrityIndex>>,
    ) {
        self.handler.set_integrity(integrity).await;
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
                tracing::warn!(
                    "relay address was not ready; the share code will be refreshed once it is"
                );
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

    pub async fn connect(&self, share_code: &str) -> anyhow::Result<GuestLink> {
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
        match self.dial(dial).await {
            Ok(direct) => {
                let (connection, via_helper) = self.upgrade_path(target, direct).await;
                Ok(self.guest_link(connection, target, via_helper))
            }
            Err(err) => self
                .try_callback_visit(target)
                .await
                .map(|callback| self.guest_link(callback, target, false))
                .map_err(|cb_err| anyhow::anyhow!("{err}；回拨也失败: {cb_err}")),
        }
    }

    pub async fn connect_endpoint(&self, endpoint_id: &str) -> anyhow::Result<GuestLink> {
        let id: EndpointId = endpoint_id
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("这个摊位的地址无效"))?;
        self.ensure_not_self(id)?;
        match self.dial(EndpointAddr::from(id)).await {
            Ok(direct) => {
                let (connection, via_helper) = self.upgrade_path(id, direct).await;
                Ok(self.guest_link(connection, id, via_helper))
            }
            Err(err) => self
                .try_callback_visit(id)
                .await
                .map(|callback| self.guest_link(callback, id, false))
                .map_err(|cb_err| anyhow::anyhow!("{err}；回拨也失败: {cb_err}")),
        }
    }

    /// Prefer direct, then mutual helper, then callback visit when we are dialable.
    async fn upgrade_path(&self, target: EndpointId, direct: Connection) -> (Connection, bool) {
        if super::helper::connection_is_direct(&direct) {
            if let Some(helper) = &self.helper {
                helper.set_via_helper(false);
            }
            return (direct, false);
        }
        let (after_helper, via_helper) = if let Some(helper) = &self.helper {
            let started = Instant::now();
            let upgraded = with_deadline(HELPER_UPGRADE_TIMEOUT, async {
                Ok::<_, anyhow::Error>(
                    super::helper::connect_preferring_helper(
                        &self.endpoint,
                        &helper.book(),
                        target,
                        direct.clone(),
                        self.routes.clone(),
                    )
                    .await,
                )
            })
            .await;
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            self.traffic.record_helper_upgrade_ms(elapsed_ms);
            let (connection, via) = match upgraded {
                Ok(pair) => pair,
                Err(err) => {
                    crate::netdebug::log(format!(
                        "helper_upgrade_timeout target={target} err={err}"
                    ));
                    (direct, false)
                }
            };
            helper.set_via_helper(via);
            if via || super::helper::connection_is_direct(&connection) {
                return (connection, via);
            }
            (connection, false)
        } else {
            (direct, false)
        };
        match self.try_callback_visit(target).await {
            Ok(callback) => {
                after_helper.close(0u8.into(), b"visit-cb");
                if let Some(helper) = &self.helper {
                    helper.set_via_helper(false);
                }
                (callback, false)
            }
            Err(err) => {
                crate::netdebug::log(format!("visit_callback_skip target={target} err={err}"));
                (after_helper, false)
            }
        }
    }

    fn guest_link(&self, connection: Connection, host: EndpointId, via_helper: bool) -> GuestLink {
        let trust = if via_helper {
            PathTrust::HelperBridge {
                session: SessionId::generate(),
                guest: self.endpoint.id(),
                host,
                helper: connection.remote_id(),
            }
        } else {
            PathTrust::EndToEnd {
                session: SessionId::generate(),
                guest: self.endpoint.id(),
                host,
            }
        };
        GuestLink {
            connection,
            trust,
            secret: self.secret.clone(),
        }
    }

    async fn try_callback_visit(&self, host: EndpointId) -> anyhow::Result<Connection> {
        let Some(visit) = &self.visit else {
            anyhow::bail!("回拨串门不可用");
        };
        let connection = with_deadline(CALLBACK_TIMEOUT, async {
            visit.connect_via_callback(host).await
        })
        .await?;
        watch_paths(connection.clone(), "visit-cb", self.routes.clone());
        Ok(connection)
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
        let result = retry_with_backoff(|attempt| {
            let addr = addr.clone();
            async move {
                if attempt > 1 {
                    self.traffic.record_retry();
                }
                with_deadline(CONNECT_TIMEOUT, async move {
                    self.endpoint
                        .connect(addr, ALPN)
                        .await
                        .map_err(|err| anyhow::anyhow!("P2P 连接失败: {err:?}"))
                })
                .await
            }
        })
        .await;
        match result {
            Ok(connection) => {
                watch_paths(connection.clone(), "guest", self.routes.clone());
                Ok(connection)
            }
            Err(RetryError::Exhausted { attempts, source }) => {
                Err(anyhow::anyhow!("P2P 连接失败（{attempts} 次）: {source}"))
            }
            Err(RetryError::Deadline { deadline }) => Err(anyhow::anyhow!(
                "P2P 连接超时（{deadline:?}）。对方需要正在开张；跨网第一次握手可能要半分钟"
            )),
            Err(RetryError::Operation(err)) => Err(err),
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
        if role == "visit-cb" {
            routes.set_path_label(&key, "回拨直连".to_string());
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

fn notice_wait(
    since: Option<Instant>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
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
    if role == "visit-cb" {
        return "回拨直连".to_string();
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

fn apply_listen_port(
    builder: iroh::endpoint::Builder,
    listen_port: Option<u16>,
) -> anyhow::Result<iroh::endpoint::Builder> {
    let Some(port) = listen_port else {
        return Ok(builder);
    };
    use iroh::endpoint::BindOpts;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    tracing::info!(port, "按互助偏好绑定固定 UDP 端口");
    let builder = builder
        .bind_addr(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .map_err(|err| anyhow::anyhow!("绑定 UDP {port} 失败: {err}"))?
        .bind_addr_with_opts(
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
            BindOpts::default().set_is_required(false),
        )
        .map_err(|err| anyhow::anyhow!("绑定 UDP IPv6 {port} 失败: {err}"))?;
    Ok(builder)
}

async fn bind_endpoint(
    secret_key: iroh::SecretKey,
    listen_port: Option<u16>,
) -> anyhow::Result<Endpoint> {
    if cfg!(test) {
        return apply_listen_port(endpoint_builder(secret_key), None)?
            .bind()
            .await
            .map_err(|err| anyhow::anyhow!("启动 P2P 节点失败: {err:?}"));
    }
    let with_dht = apply_listen_port(endpoint_builder(secret_key.clone()), listen_port)?
        .address_lookup(iroh_mainline_address_lookup::DhtAddressLookup::builder());
    match with_dht.bind().await {
        Ok(endpoint) => Ok(endpoint),
        Err(err) => {
            tracing::warn!(?err, "DHT address lookup did not start");
            apply_listen_port(endpoint_builder(secret_key), listen_port)?
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
