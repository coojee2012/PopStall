use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey, SecretKey, Signature};
use iroh_gossip::{Event, Gossip, GossipSender, JoinOptions, TopicId};
use serde::Serialize;
use tokio::sync::{watch, Mutex, Semaphore};

use super::node::{self, RouteBook};
use super::plaza::Plaza;
use crate::security::BandwidthLimit;

pub const ALPN: &[u8] = b"popstall/helper/1";
pub const DEFAULT_LIMIT_BPS: u32 = 1024 * 1024;
pub const MIN_LIMIT_BPS: u32 = 512 * 1024;
pub const MAX_LIMIT_BPS: u32 = 4 * 1024 * 1024;

const MAX_SESSIONS: usize = 4;
const CARD_TTL_SECS: u32 = 90;
const TICK: Duration = Duration::from_secs(20);
const PATH_WAIT: Duration = Duration::from_secs(3);
const HELPER_DIAL: Duration = Duration::from_secs(8);
const HOST_DIAL: Duration = Duration::from_secs(20);
const PROBE_PICKS: usize = 3;

const TOPIC: TopicId = TopicId::from_bytes([
    0xdc, 0x3a, 0x9f, 0xdc, 0x8c, 0xfe, 0x1a, 0x46, 0xd3, 0x5f, 0xd2, 0x20, 0xd2, 0x41, 0xe3, 0x3f,
    0xa9, 0x9b, 0xb8, 0xb0, 0x1f, 0xf2, 0x8a, 0xdc, 0x3e, 0x7f, 0x0a, 0x5e, 0x4c, 0x6f, 0x08, 0xc8,
]);

const CARD_MAGIC: &[u8; 4] = b"PHC1";
const HELLO_MAGIC: &[u8; 4] = b"PHL1";
const KIND_OPEN: u8 = 1;
const KIND_BYE: u8 = 2;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperOffer {
    pub endpoint_id: String,
    pub limit_bps: u32,
    pub load: u8,
    pub rtt_ms: Option<u32>,
    pub direct: bool,
    pub usable: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperStatus {
    pub enabled: bool,
    pub limit_bps: u32,
    pub active: usize,
    pub offers: usize,
    pub via_helper: bool,
    pub capable: bool,
    pub reach_note: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveHelper {
    limit_bps: u32,
    load: u8,
    expires: Instant,
}

#[derive(Clone)]
pub struct HelperBook {
    offers: Arc<Mutex<HashMap<EndpointId, LiveHelper>>>,
}

impl HelperBook {
    fn new() -> Self {
        Self {
            offers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn list(&self, self_id: EndpointId) -> Vec<(EndpointId, LiveHelper)> {
        let mut offers = self.offers.lock().await;
        let now = Instant::now();
        offers.retain(|_, live| live.expires > now);
        offers
            .iter()
            .filter(|(id, _)| **id != self_id)
            .map(|(id, live)| (*id, live.clone()))
            .collect()
    }

    async fn ingest(&self, card: HelperCard) {
        match card {
            HelperCard::Open {
                endpoint_id,
                limit_bps,
                load,
                ttl,
                ..
            } => {
                self.offers.lock().await.insert(
                    endpoint_id,
                    LiveHelper {
                        limit_bps,
                        load,
                        expires: Instant::now() + Duration::from_secs(u64::from(ttl.max(30))),
                    },
                );
            }
            HelperCard::Bye { endpoint_id } => {
                self.offers.lock().await.remove(&endpoint_id);
            }
        }
    }
}

#[derive(Clone)]
pub struct HelperHub {
    enabled: watch::Sender<bool>,
    limit_bps: Arc<AtomicU32>,
    active: Arc<AtomicUsize>,
    sessions: Arc<Semaphore>,
    bandwidth: Arc<Mutex<Arc<BandwidthLimit>>>,
    book: HelperBook,
    endpoint: Endpoint,
    routes: Arc<RouteBook>,
    via_helper: Arc<std::sync::atomic::AtomicBool>,
}

impl HelperHub {
    pub async fn status(&self) -> HelperStatus {
        let enabled = *self.enabled.borrow();
        let limit_bps = self.limit_bps.load(Ordering::Relaxed);
        let active = self.active.load(Ordering::Relaxed);
        let via_helper = self.via_helper.load(Ordering::Relaxed);
        let offers = self.book.list(self.endpoint.id()).await.len();
        let (capable, reach_note) = reach_snapshot(&self.endpoint, &self.routes);
        HelperStatus {
            enabled,
            limit_bps,
            active,
            offers,
            via_helper,
            capable,
            reach_note,
        }
    }

    pub async fn probed_offers(&self, exclude: Option<EndpointId>) -> Vec<HelperOffer> {
        probe_offers(&self.endpoint, &self.book, self.endpoint.id(), exclude).await
    }

    pub async fn dial_as_helper(
        &self,
        helper_id: EndpointId,
        target: EndpointId,
        routes: Arc<RouteBook>,
    ) -> anyhow::Result<Connection> {
        if helper_id == target {
            anyhow::bail!("摊主自己不能给自己的摊当互助桥。需要第三方帮手，或等直连打通。");
        }
        let connection = tokio::time::timeout(HELPER_DIAL, dial_helper(&self.endpoint, helper_id, target))
            .await
            .map_err(|_| anyhow::anyhow!("互助中继连接超时"))?
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        tokio::time::sleep(Duration::from_millis(800)).await;
        if selected_is_relay(&connection) != Some(false) {
            connection.close(0u8.into(), b"relay");
            anyhow::bail!("这个帮手目前也只能走公共中继，换一个或稍后再试");
        }
        node::watch_paths(connection.clone(), "helper", routes);
        self.set_via_helper(true);
        Ok(connection)
    }

    pub fn set_via_helper(&self, value: bool) {
        self.via_helper.store(value, Ordering::Relaxed);
    }

    pub async fn set(&self, enabled: bool, limit_bps: u32) -> Result<(), String> {
        let limit = clamp_limit(limit_bps);
        self.limit_bps.store(limit, Ordering::Relaxed);
        *self.bandwidth.lock().await = Arc::new(BandwidthLimit::new(u64::from(limit)));
        if enabled {
            wait_reachability(&self.endpoint, &self.routes, Duration::from_secs(6)).await?;
        }
        let _ = self.enabled.send(enabled);
        Ok(())
    }

    pub fn book(&self) -> HelperBook {
        self.book.clone()
    }

    pub fn handler(&self) -> HelperHandler {
        HelperHandler {
            enabled: self.enabled.subscribe(),
            sessions: self.sessions.clone(),
            active: self.active.clone(),
            bandwidth: self.bandwidth.clone(),
            endpoint: self.endpoint.clone(),
        }
    }
}

#[derive(Clone)]
pub struct HelperHandler {
    enabled: watch::Receiver<bool>,
    sessions: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    bandwidth: Arc<Mutex<Arc<BandwidthLimit>>>,
    endpoint: Endpoint,
}

impl std::fmt::Debug for HelperHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HelperHandler").finish_non_exhaustive()
    }
}

impl ProtocolHandler for HelperHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if !*self.enabled.borrow() {
            connection.close(0u8.into(), b"off");
            return Ok(());
        }
        let Ok(permit) = self.sessions.clone().try_acquire_owned() else {
            connection.close(0u8.into(), b"busy");
            return Ok(());
        };
        self.active.fetch_add(1, Ordering::Relaxed);
        let _guard = SessionGuard {
            active: self.active.clone(),
            _permit: permit,
        };
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let target = match read_hello(&mut recv).await {
            Ok(target) => target,
            Err(err) => {
                tracing::debug!(%err, "helper hello rejected");
                let _ = write_hello_status(&mut send, 1).await;
                return Ok(());
            }
        };
        if target == self.endpoint.id() {
            let _ = write_hello_status(&mut send, 2).await;
            return Ok(());
        }
        let host = match tokio::time::timeout(
            HOST_DIAL,
            self.endpoint
                .connect(EndpointAddr::from(target), node::ALPN),
        )
        .await
        {
            Ok(Ok(host)) => host,
            Ok(Err(err)) => {
                tracing::info!(%target, ?err, "helper could not reach host");
                let _ = write_hello_status(&mut send, 3).await;
                return Ok(());
            }
            Err(_) => {
                let _ = write_hello_status(&mut send, 4).await;
                return Ok(());
            }
        };
        if write_hello_status(&mut send, 0).await.is_err() {
            host.close(0u8.into(), b"hello");
            return Ok(());
        }
        tracing::info!(guest = %connection.remote_id(), %target, "互助中继会话已接通");
        loop {
            let guest_pair = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let host_pair = match host.open_bi().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let bandwidth = self.bandwidth.lock().await.clone();
            tokio::spawn(async move {
                if let Err(err) = bridge(guest_pair, host_pair, bandwidth).await {
                    tracing::debug!(%err, "helper bridge ended");
                }
            });
        }
        host.close(0u8.into(), b"done");
        Ok(())
    }
}

struct SessionGuard {
    active: Arc<AtomicUsize>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn start(endpoint: Endpoint, gossip: Gossip, plaza: Plaza, routes: Arc<RouteBook>) -> HelperHub {
    let (enabled, _) = watch::channel(false);
    let limit_bps = Arc::new(AtomicU32::new(DEFAULT_LIMIT_BPS));
    let active = Arc::new(AtomicUsize::new(0));
    let book = HelperBook::new();
    let hub = HelperHub {
        enabled: enabled.clone(),
        limit_bps: limit_bps.clone(),
        active: active.clone(),
        sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
        bandwidth: Arc::new(Mutex::new(Arc::new(BandwidthLimit::new(u64::from(
            DEFAULT_LIMIT_BPS,
        ))))),
        book: book.clone(),
        endpoint: endpoint.clone(),
        routes: routes.clone(),
        via_helper: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let secret = endpoint.secret_key().clone();
    let self_id = endpoint.id();
    tokio::spawn(async move {
        let _ = wait_relay(&endpoint, Duration::from_secs(15)).await;
        serve_gossip(
            gossip,
            plaza,
            secret,
            enabled,
            limit_bps,
            active,
            book,
            routes,
            endpoint,
            self_id,
        )
        .await;
    });
    hub
}

async fn serve_gossip(
    gossip: Gossip,
    plaza: Plaza,
    secret: SecretKey,
    enabled: watch::Sender<bool>,
    limit_bps: Arc<AtomicU32>,
    active: Arc<AtomicUsize>,
    book: HelperBook,
    routes: Arc<RouteBook>,
    endpoint: Endpoint,
    self_id: EndpointId,
) {
    let mut enabled_rx = enabled.subscribe();
    loop {
        let peers = plaza.peer_ids(16).await;
        let options = JoinOptions::with_bootstrap(peers).with_subscription_capacity(128);
        let topic = match gossip.subscribe_with_opts(TOPIC, options).await {
            Ok(topic) => topic,
            Err(err) => {
                tracing::debug!(?err, "helper topic subscribe failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let (sender, mut receiver) = topic.split();
        let mut tick = tokio::time::interval(TICK);
        let mut was_on = *enabled_rx.borrow();
        if was_on {
            broadcast_open(&sender, &secret, &limit_bps, &active).await;
        }
        loop {
            tokio::select! {
                changed = enabled_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let on = *enabled_rx.borrow();
                    if on {
                        broadcast_open(&sender, &secret, &limit_bps, &active).await;
                        was_on = true;
                    } else if was_on {
                        let _ = sender.broadcast(Bytes::from(encode_bye(&secret))).await;
                        was_on = false;
                    }
                }
                _ = tick.tick() => {
                    let peers = plaza.peer_ids(16).await;
                    let _ = sender.join_peers(peers).await;
                    if *enabled_rx.borrow() {
                        if !reachability_ok(&endpoint, &routes) {
                            tracing::info!("互助中继已关闭：当前直连不到这台机器");
                            let _ = enabled.send(false);
                            continue;
                        }
                        broadcast_open(&sender, &secret, &limit_bps, &active).await;
                        was_on = true;
                    }
                }
                event = receiver.next() => {
                    let Some(event) = event else {
                        break;
                    };
                    match event {
                        Ok(Event::Received(message)) => {
                            if message.delivered_from == self_id {
                                continue;
                            }
                            if let Some(card) = decode_card(&message.content, lamport_stamp()) {
                                book.ingest(card).await;
                            }
                        }
                        Ok(Event::Lagged) => tracing::debug!("helper gossip lagged"),
                        Ok(_) => {}
                        Err(err) => tracing::debug!(?err, "helper gossip event failed"),
                    }
                }
            }
        }
    }
}

async fn broadcast_open(
    sender: &GossipSender,
    secret: &SecretKey,
    limit_bps: &Arc<AtomicU32>,
    active: &Arc<AtomicUsize>,
) {
    let load = active.load(Ordering::Relaxed).min(255) as u8;
    let bytes = encode_open(secret, limit_bps.load(Ordering::Relaxed), load);
    if let Err(err) = sender.broadcast(Bytes::from(bytes)).await {
        tracing::debug!(?err, "helper announce failed");
    }
}

async fn wait_relay(endpoint: &Endpoint, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if endpoint.addr().relay_urls().next().is_some() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn connect_preferring_helper(
    endpoint: &Endpoint,
    book: &HelperBook,
    target: EndpointId,
    direct: Connection,
    routes: Arc<RouteBook>,
) -> (Connection, bool) {
    tokio::time::sleep(PATH_WAIT).await;
    if selected_is_relay(&direct) == Some(false) {
        return (direct, false);
    }
    let offers = book.list(endpoint.id()).await;
    let candidates: Vec<EndpointId> = offers
        .into_iter()
        .map(|(id, _)| id)
        .filter(|id| *id != target)
        .take(PROBE_PICKS)
        .collect();
    if candidates.is_empty() {
        tracing::info!(%target, "没有可用的第三方互助中继（摊主自己不能桥接自己）");
        return (direct, false);
    }
    if let Some(via) = pick_helper(endpoint, target, &candidates).await {
        tracing::info!(helper = %via.remote_id(), host = %target, "串门改走互助中继");
        node::watch_paths(via.clone(), "helper", routes);
        direct.close(0u8.into(), b"helper");
        return (via, true);
    }
    (direct, false)
}

pub async fn probe_offers(
    endpoint: &Endpoint,
    book: &HelperBook,
    self_id: EndpointId,
    exclude: Option<EndpointId>,
) -> Vec<HelperOffer> {
    let raw = book.list(self_id).await;
    let mut out = Vec::new();
    for (id, live) in raw {
        if exclude == Some(id) {
            out.push(HelperOffer {
                endpoint_id: id.to_string(),
                limit_bps: live.limit_bps,
                load: live.load,
                rtt_ms: None,
                direct: false,
                usable: false,
                note: "这是当前摊主，不能给自己的摊当桥".to_string(),
            });
            continue;
        }
        let started = Instant::now();
        match tokio::time::timeout(Duration::from_secs(4), endpoint.connect(EndpointAddr::from(id), ALPN)).await
        {
            Ok(Ok(connection)) => {
                tokio::time::sleep(Duration::from_millis(600)).await;
                let rtt_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
                let direct = selected_is_relay(&connection) == Some(false);
                connection.close(0u8.into(), b"probe");
                out.push(HelperOffer {
                    endpoint_id: id.to_string(),
                    limit_bps: live.limit_bps,
                    load: live.load,
                    rtt_ms: Some(rtt_ms),
                    direct,
                    usable: direct,
                    note: if direct {
                        format!("可直连 · 约 {rtt_ms} ms")
                    } else {
                        "连上了，但仍在走公共中继，暂不适合作帮手".to_string()
                    },
                });
            }
            Ok(Err(_)) => out.push(HelperOffer {
                endpoint_id: id.to_string(),
                limit_bps: live.limit_bps,
                load: live.load,
                rtt_ms: None,
                direct: false,
                usable: false,
                note: "连不上".to_string(),
            }),
            Err(_) => out.push(HelperOffer {
                endpoint_id: id.to_string(),
                limit_bps: live.limit_bps,
                load: live.load,
                rtt_ms: None,
                direct: false,
                usable: false,
                note: "探测超时".to_string(),
            }),
        }
    }
    out.sort_by(|left, right| {
        right
            .usable
            .cmp(&left.usable)
            .then(left.rtt_ms.unwrap_or(u32::MAX).cmp(&right.rtt_ms.unwrap_or(u32::MAX)))
            .then(left.endpoint_id.cmp(&right.endpoint_id))
    });
    out
}

async fn pick_helper(
    endpoint: &Endpoint,
    target: EndpointId,
    candidates: &[EndpointId],
) -> Option<Connection> {
    // Only a helper we can reach directly is worth using; another slow relay hop
    // does not fix the media path.
    for id in candidates {
        match tokio::time::timeout(HELPER_DIAL, dial_helper(endpoint, *id, target)).await {
            Ok(Ok(connection)) => {
                tokio::time::sleep(Duration::from_millis(800)).await;
                if selected_is_relay(&connection) == Some(false) {
                    return Some(connection);
                }
                tracing::debug!(helper = %id, "互助节点也在走中继，跳过");
                connection.close(0u8.into(), b"skip");
            }
            Ok(Err(err)) => tracing::debug!(helper = %id, ?err, "互助中继不可用"),
            Err(_) => tracing::debug!(helper = %id, "互助中继探测超时"),
        }
    }
    None
}

fn reachability_ok(endpoint: &Endpoint, routes: &RouteBook) -> bool {
    routes.inbound_direct() || has_global_dial_addr(endpoint)
}

fn reach_snapshot(endpoint: &Endpoint, routes: &RouteBook) -> (bool, String) {
    if routes.inbound_direct() {
        return (
            true,
            "已有人直连打到过你，适合当互助中继。".to_string(),
        );
    }
    if has_global_dial_addr(endpoint) {
        return (
            true,
            "本机已有可被拨号的公网地址，适合当互助中继。".to_string(),
        );
    }
    (
        false,
        "现在直连不到你：还没有公网地址，也还没有人打洞打到这台机器。不需要自己填端口；等网络就绪或有人成功直连后再开。".to_string(),
    )
}

async fn wait_reachability(
    endpoint: &Endpoint,
    routes: &RouteBook,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if reachability_ok(endpoint, routes) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(reach_snapshot(endpoint, routes).1);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn has_global_dial_addr(endpoint: &Endpoint) -> bool {
    endpoint
        .addr()
        .ip_addrs()
        .any(|socket| is_globally_reachable_ip(socket.ip()))
}

fn is_globally_reachable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation())
        }
        IpAddr::V6(v6) => {
            let octets = v6.octets();
            let unique_local = (octets[0] & 0xfe) == 0xfc;
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_unicast_link_local()
                || unique_local)
        }
    }
}

async fn dial_helper(
    endpoint: &Endpoint,
    helper: EndpointId,
    target: EndpointId,
) -> anyhow::Result<Connection> {
    let connection = endpoint
        .connect(EndpointAddr::from(helper), ALPN)
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    write_hello(&mut send, target).await?;
    let status = read_hello_status(&mut recv).await?;
    if status != 0 {
        connection.close(0u8.into(), b"reject");
        anyhow::bail!("互助中继拒绝了这次转发 ({status})");
    }
    Ok(connection)
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

async fn bridge(
    guest: (SendStream, RecvStream),
    host: (SendStream, RecvStream),
    bandwidth: Arc<BandwidthLimit>,
) -> anyhow::Result<()> {
    let (mut guest_send, mut guest_recv) = guest;
    let (mut host_send, mut host_recv) = host;
    let up = {
        let bandwidth = bandwidth.clone();
        async move {
            copy_limited(&mut guest_recv, &mut host_send, bandwidth).await?;
            let _ = host_send.finish();
            Ok::<(), anyhow::Error>(())
        }
    };
    let down = async move {
        copy_limited(&mut host_recv, &mut guest_send, bandwidth).await?;
        let _ = guest_send.finish();
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {
        result = up => result?,
        result = down => result?,
    }
    Ok(())
}

async fn copy_limited(
    from: &mut RecvStream,
    into: &mut SendStream,
    bandwidth: Arc<BandwidthLimit>,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let size = tokio::io::AsyncReadExt::read(from, &mut buf)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        if size == 0 {
            break;
        }
        bandwidth.consume(size as u64).await;
        into.write_all(&buf[..size])
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
    }
    Ok(())
}

async fn write_hello(send: &mut SendStream, target: EndpointId) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(4 + 1 + 32);
    frame.extend_from_slice(HELLO_MAGIC);
    frame.push(1);
    frame.extend_from_slice(target.as_bytes());
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn read_hello(recv: &mut RecvStream) -> anyhow::Result<EndpointId> {
    let bytes = recv.read_to_end(4 + 1 + 32).await?;
    if bytes.len() != 4 + 1 + 32 || &bytes[..4] != HELLO_MAGIC || bytes[4] != 1 {
        anyhow::bail!("hello 无效");
    }
    let id = PublicKey::from_bytes(bytes[5..37].try_into()?)?;
    Ok(id)
}

async fn write_hello_status(send: &mut SendStream, status: u8) -> anyhow::Result<()> {
    send.write_all(&[status]).await?;
    send.finish()?;
    Ok(())
}

async fn read_hello_status(recv: &mut RecvStream) -> anyhow::Result<u8> {
    let bytes = recv.read_to_end(8).await?;
    bytes
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("没有回应"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelperCard {
    Open {
        endpoint_id: PublicKey,
        limit_bps: u32,
        load: u8,
        timestamp: u64,
        ttl: u32,
    },
    Bye {
        endpoint_id: PublicKey,
    },
}

fn encode_open(secret: &SecretKey, limit_bps: u32, load: u8) -> Vec<u8> {
    let limit = clamp_limit(limit_bps);
    let mut body = Vec::with_capacity(4 + 1 + 1 + 32 + 8 + 4 + 4 + 1 + 64);
    body.extend_from_slice(CARD_MAGIC);
    body.push(1);
    body.push(KIND_OPEN);
    body.extend_from_slice(secret.public().as_bytes());
    let timestamp = lamport_stamp();
    body.extend_from_slice(&timestamp.to_be_bytes());
    body.extend_from_slice(&CARD_TTL_SECS.to_be_bytes());
    body.extend_from_slice(&limit.to_be_bytes());
    body.push(load);
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn encode_bye(secret: &SecretKey) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + 1 + 1 + 32 + 8 + 4 + 64);
    body.extend_from_slice(CARD_MAGIC);
    body.push(1);
    body.push(KIND_BYE);
    body.extend_from_slice(secret.public().as_bytes());
    body.extend_from_slice(&lamport_stamp().to_be_bytes());
    body.extend_from_slice(&CARD_TTL_SECS.to_be_bytes());
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn decode_card(bytes: &[u8], now: u64) -> Option<HelperCard> {
    if bytes.len() < 4 + 1 + 1 + 32 + 8 + 4 + 64 || &bytes[..4] != CARD_MAGIC {
        return None;
    }
    let signed_len = bytes.len() - 64;
    let signed = &bytes[..signed_len];
    let mut cursor = &bytes[4..signed_len];
    let version = *cursor.first()?;
    cursor = &cursor[1..];
    if version != 1 {
        return None;
    }
    let kind = *cursor.first()?;
    cursor = &cursor[1..];
    let id_bytes: [u8; 32] = cursor.get(..32)?.try_into().ok()?;
    cursor = &cursor[32..];
    let endpoint_id = PublicKey::from_bytes(&id_bytes).ok()?;
    let timestamp = u64::from_be_bytes(cursor.get(..8)?.try_into().ok()?);
    cursor = &cursor[8..];
    let ttl = u32::from_be_bytes(cursor.get(..4)?.try_into().ok()?);
    cursor = &cursor[4..];
    let signature = Signature::from_bytes(bytes[signed_len..].try_into().ok()?);
    endpoint_id.verify(signed, &signature).ok()?;
    if timestamp == 0 || ttl == 0 || timestamp > now.saturating_add(180) {
        return None;
    }
    if timestamp + u64::from(ttl) + 30 < now {
        return None;
    }
    match kind {
        KIND_OPEN => {
            let limit_bps = u32::from_be_bytes(cursor.get(..4)?.try_into().ok()?);
            cursor = &cursor[4..];
            let load = *cursor.first()?;
            cursor = &cursor[1..];
            if !cursor.is_empty() {
                return None;
            }
            Some(HelperCard::Open {
                endpoint_id,
                limit_bps: clamp_limit(limit_bps),
                load,
                timestamp,
                ttl,
            })
        }
        KIND_BYE => {
            if !cursor.is_empty() {
                return None;
            }
            Some(HelperCard::Bye { endpoint_id })
        }
        _ => None,
    }
}

fn clamp_limit(limit_bps: u32) -> u32 {
    limit_bps.clamp(MIN_LIMIT_BPS, MAX_LIMIT_BPS)
}

fn lamport_stamp() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    1 + start.elapsed().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_cards_round_trip_and_reject_tampering() {
        let key = SecretKey::generate();
        let open = encode_open(&key, DEFAULT_LIMIT_BPS, 2);
        let card = decode_card(&open, lamport_stamp() + 1).unwrap();
        match card {
            HelperCard::Open {
                endpoint_id,
                limit_bps,
                load,
                ..
            } => {
                assert_eq!(endpoint_id, key.public());
                assert_eq!(limit_bps, DEFAULT_LIMIT_BPS);
                assert_eq!(load, 2);
            }
            HelperCard::Bye { .. } => panic!("expected open"),
        }
        let mut bad = open;
        *bad.last_mut().unwrap() ^= 1;
        assert!(decode_card(&bad, lamport_stamp() + 1).is_none());
        let bye = encode_bye(&key);
        assert!(matches!(
            decode_card(&bye, lamport_stamp() + 1),
            Some(HelperCard::Bye { .. })
        ));
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(clamp_limit(100), MIN_LIMIT_BPS);
        assert_eq!(clamp_limit(DEFAULT_LIMIT_BPS), DEFAULT_LIMIT_BPS);
        assert_eq!(clamp_limit(u32::MAX), MAX_LIMIT_BPS);
    }

    #[test]
    fn global_ip_skips_lan_and_keeps_public() {
        assert!(!is_globally_reachable_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_globally_reachable_ip("192.168.1.2".parse().unwrap()));
        assert!(!is_globally_reachable_ip("127.0.0.1".parse().unwrap()));
        assert!(is_globally_reachable_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_globally_reachable_ip("fd12::1".parse().unwrap()));
        assert!(is_globally_reachable_ip("2001:4860:4860::8888".parse().unwrap()));
    }
}
