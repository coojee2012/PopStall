use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use iroh::endpoint::{Connection, RecvStream, RelayMode, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey, RelayUrl, SecretKey, Signature};
use iroh_gossip::{Event, Gossip, GossipSender, JoinOptions, TopicId};
use serde::Serialize;
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};

use super::card;
use super::node::{self, RouteBook};
use super::plaza::Plaza;
use super::protocol::StallHandler;
use crate::security::{BandwidthLimit, SessionBudget};

pub const ALPN: &[u8] = b"popstall/helper/1";
pub const INVITE_ALPN: &[u8] = b"popstall/helper-invite/1";
pub const DEFAULT_LIMIT_BPS: u32 = 1024 * 1024;
pub const MIN_LIMIT_BPS: u32 = 512 * 1024;
pub const MAX_LIMIT_BPS: u32 = 4 * 1024 * 1024;

const MAX_SESSIONS: usize = 4;
const CARD_TTL_SECS: u32 = 90;
const TICK: Duration = Duration::from_secs(10);
const PATH_WAIT: Duration = Duration::from_secs(3);
const HELPER_DIAL: Duration = Duration::from_secs(8);
/// Hard timeout for IP-only dials (no relay, no path-upgrade wait).
const IP_ONLY_DIAL: Duration = Duration::from_secs(6);
const HOST_DIAL: Duration = Duration::from_secs(20);
const RENDEZVOUS_WAIT: Duration = Duration::from_secs(12);
/// Guest waits for helper hello-ack through rendezvous or fallback dial.
const HELPER_SESSION: Duration = Duration::from_secs(36);
const PROBE_PICKS: usize = 3;
const MAX_CARD_ADDRS: usize = 4;
const MAX_CARD_RELAYS: usize = 4;
const MAX_RELAY_URL_BYTES: usize = 256;
const CARD_VERSION_V1: u8 = 1;
const CARD_VERSION_V2: u8 = 2;
const INVITE_VERSION_V1: u8 = 1;
const INVITE_VERSION_V2: u8 = 2;
const ROLE_GUEST: u8 = 1;
const ROLE_HOST: u8 = 2;
/// Lightweight reachability check — must not open a bridge session.
const ROLE_PROBE: u8 = 3;

const TOPIC: TopicId = TopicId::from_bytes([
    0xdc, 0x3a, 0x9f, 0xdc, 0x8c, 0xfe, 0x1a, 0x46, 0xd3, 0x5f, 0xd2, 0x20, 0xd2, 0x41, 0xe3, 0x3f,
    0xa9, 0x9b, 0xb8, 0xb0, 0x1f, 0xf2, 0x8a, 0xdc, 0x3e, 0x7f, 0x0a, 0x5e, 0x4c, 0x6f, 0x08, 0xc8,
]);

const CARD_MAGIC: &[u8; 4] = b"PHC1";
const HELLO_MAGIC: &[u8; 4] = b"PHL1";
const INVITE_MAGIC: &[u8; 4] = b"PHI1";
const KIND_OPEN: u8 = 1;
const KIND_BYE: u8 = 2;

type SessionId = [u8; 16];

#[derive(Clone, Default)]
struct RendezvousBook {
    waiting: Arc<Mutex<HashMap<SessionId, oneshot::Sender<Connection>>>>,
}

impl RendezvousBook {
    async fn register(&self, session: SessionId) -> oneshot::Receiver<Connection> {
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(session, tx);
        rx
    }

    async fn take(&self, session: SessionId) -> Option<oneshot::Sender<Connection>> {
        self.waiting.lock().await.remove(&session)
    }

    async fn cancel(&self, session: SessionId) {
        self.waiting.lock().await.remove(&session);
    }
}

#[derive(Clone, Debug)]
struct HostInvite {
    helper_id: EndpointId,
    host_id: EndpointId,
    guest_id: EndpointId,
    session: SessionId,
    addrs: Vec<SocketAddr>,
    relays: Vec<RelayUrl>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperOffer {
    pub endpoint_id: String,
    pub limit_bps: u32,
    pub load: u8,
    pub rtt_ms: Option<u32>,
    pub direct: bool,
    /// True when the helper is relay-only: no public dial addresses, but relay URLs advertised.
    pub relay: bool,
    pub usable: bool,
    pub note: String,
}

/// Helpers terminate QUIC and can observe metadata, so they stay an untrusted
/// data plane. The default policy keeps them usable for multi-layer NAT/CGNAT;
/// callers can inject a stricter policy without changing the bridge semantics.
pub trait HelperPolicy: Send + Sync {
    fn allows_offer(&self, _offer: &HelperOffer) -> bool {
        true
    }

    fn allows_session(&self, _guest: EndpointId, _target: EndpointId, _session: &[u8; 16]) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultHelperPolicy;

impl HelperPolicy for DefaultHelperPolicy {
    fn allows_offer(&self, _offer: &HelperOffer) -> bool {
        true
    }

    fn allows_session(&self, _guest: EndpointId, _target: EndpointId, _session: &[u8; 16]) -> bool {
        true
    }
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
    pub netdebug_path: String,
    /// Preferred UDP port saved for router forwarding (`None` = ephemeral).
    pub listen_port: Option<u16>,
    /// Ports this process is actually bound to right now.
    pub bound_ports: Vec<u16>,
    pub port_note: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveHelper {
    limit_bps: u32,
    load: u8,
    /// Public dial addresses advertised by the helper (may be empty).
    addrs: Vec<SocketAddr>,
    /// Relay URLs advertised by the helper (may be empty).
    relays: Vec<RelayUrl>,
    expires: Instant,
}

#[derive(Clone)]
pub struct HelperBook {
    offers: Arc<Mutex<HashMap<EndpointId, LiveHelper>>>,
    policy: Arc<dyn HelperPolicy>,
}

impl HelperBook {
    fn new() -> Self {
        Self::with_policy(Arc::new(DefaultHelperPolicy))
    }

    fn with_policy(policy: Arc<dyn HelperPolicy>) -> Self {
        Self {
            offers: Arc::new(Mutex::new(HashMap::new())),
            policy,
        }
    }

    fn allows_offer(&self, id: EndpointId, live: &LiveHelper) -> bool {
        self.policy.allows_offer(&HelperOffer {
            endpoint_id: id.to_string(),
            limit_bps: live.limit_bps,
            load: live.load,
            rtt_ms: None,
            direct: !live.addrs.is_empty(),
            relay: live.addrs.is_empty() && !live.relays.is_empty(),
            usable: true,
            note: String::new(),
        })
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
                addrs,
                relays,
                ..
            } => {
                self.offers.lock().await.insert(
                    endpoint_id,
                    LiveHelper {
                        limit_bps,
                        load,
                        addrs,
                        relays,
                        expires: Instant::now() + Duration::from_secs(u64::from(ttl.max(30))),
                    },
                );
            }
            HelperCard::Bye { endpoint_id } => {
                self.offers.lock().await.remove(&endpoint_id);
            }
        }
    }

    async fn addresses_for(&self, id: EndpointId) -> (Vec<SocketAddr>, Vec<RelayUrl>) {
        let mut offers = self.offers.lock().await;
        let now = Instant::now();
        offers.retain(|_, live| live.expires > now);
        offers
            .get(&id)
            .map(|live| (live.addrs.clone(), live.relays.clone()))
            .unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct HelperHub {
    enabled: watch::Sender<bool>,
    hosting: watch::Sender<bool>,
    limit_bps: Arc<AtomicU32>,
    /// 0 means ephemeral / unset.
    listen_port: Arc<AtomicU32>,
    active: Arc<AtomicUsize>,
    sessions: Arc<Semaphore>,
    bandwidth: Arc<Mutex<Arc<BandwidthLimit>>>,
    book: HelperBook,
    policy: Arc<dyn HelperPolicy>,
    rendezvous: RendezvousBook,
    invite_tx: mpsc::Sender<HostInvite>,
    endpoint: Endpoint,
    routes: Arc<RouteBook>,
    stall: StallHandler,
    data_dir: std::path::PathBuf,
    via_helper: Arc<std::sync::atomic::AtomicBool>,
}

impl HelperHub {
    pub async fn status(&self) -> HelperStatus {
        let enabled = *self.enabled.borrow();
        let limit_bps = self.limit_bps.load(Ordering::Relaxed);
        let active = self.active.load(Ordering::Relaxed);
        let via_helper = self.via_helper.load(Ordering::Relaxed);
        let offers = self.book.list(self.endpoint.id()).await.len();
        let listen_port = stored_listen_port(&self.listen_port);
        let bound_ports = bound_udp_ports(&self.endpoint);
        let (capable, reach_note) =
            reach_snapshot(&self.endpoint, &self.routes, listen_port, &bound_ports);
        let port_note = port_status_note(listen_port, &bound_ports);
        HelperStatus {
            enabled,
            limit_bps,
            active,
            offers,
            via_helper,
            capable,
            reach_note,
            netdebug_path: crate::netdebug::path_string(),
            listen_port,
            bound_ports,
            port_note,
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
        let (addrs, advertised) = self.book.addresses_for(helper_id).await;
        let relays = shared_relays(&advertised, &local_relay_urls(&self.endpoint));
        let (connection, known_direct) = tokio::time::timeout(
            HELPER_SESSION,
            dial_helper(&self.endpoint, helper_id, target, &addrs, &relays),
        )
        .await
        .map_err(|_| anyhow::anyhow!("互助中继连接超时"))?
        .map_err(|err| anyhow::anyhow!("{err}"))?;
        // IP-only dials never touch n0 (known_direct). A shared relay is an
        // explicitly allowed path; otherwise require a non-relay path.
        if !known_direct && relays.is_empty() && !ensure_has_direct(&connection, PATH_WAIT).await {
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

    pub fn set_hosting(&self, hosting: bool) {
        let _ = self.hosting.send(hosting);
    }

    pub async fn set(
        &self,
        enabled: bool,
        limit_bps: u32,
        listen_port: Option<u16>,
    ) -> Result<(), String> {
        let limit = clamp_limit(limit_bps);
        self.limit_bps.store(limit, Ordering::Relaxed);
        *self.bandwidth.lock().await = Arc::new(BandwidthLimit::new(u64::from(limit)));
        let port = crate::storage::helper_prefs::normalize_port(listen_port);
        let mut prefs = crate::storage::helper_prefs::load(&self.data_dir);
        prefs.listen_port = port;
        crate::storage::helper_prefs::save(&self.data_dir, &prefs)?;
        self.listen_port
            .store(u32::from(port.unwrap_or(0)), Ordering::Relaxed);
        if enabled {
            let bound = bound_udp_ports(&self.endpoint);
            if let Some(want) = port {
                if !bound.contains(&want) {
                    return Err(format!(
                        "端口 {want} 已保存，但当前还在监听 {}。请完全退出闪摊再打开，才会按路由器转发的端口监听。",
                        bound_ports_label(&bound)
                    ));
                }
                // User asserted router UDP forward on this port — don't block on STUN.
            } else {
                wait_reachability(
                    &self.endpoint,
                    &self.routes,
                    None,
                    &bound,
                    Duration::from_secs(20),
                )
                .await?;
            }
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
            policy: self.policy.clone(),
            endpoint: self.endpoint.clone(),
            rendezvous: self.rendezvous.clone(),
            invite_tx: self.invite_tx.clone(),
            listen_port: self.listen_port.clone(),
        }
    }

    pub fn invite_handler(&self) -> InviteHandler {
        InviteHandler {
            hosting: self.hosting.subscribe(),
            stall: self.stall.clone(),
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
    policy: Arc<dyn HelperPolicy>,
    endpoint: Endpoint,
    rendezvous: RendezvousBook,
    invite_tx: mpsc::Sender<HostInvite>,
    listen_port: Arc<AtomicU32>,
}

impl std::fmt::Debug for HelperHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperHandler")
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for HelperHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if !*self.enabled.borrow() {
            connection.close(0u8.into(), b"off");
            return Ok(());
        }
        // Wait for a real session hello before taking a slot — latency probes only
        // open the connection and must not burn MAX_SESSIONS.
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let hello = match read_hello(&mut recv).await {
            Ok(hello) => hello,
            Err(err) => {
                tracing::debug!(%err, "helper hello rejected");
                let _ = write_hello_status(&mut send, 1).await;
                return Ok(());
            }
        };
        match hello {
            Hello::Probe => {
                // Write status, then wait for the client to close. Returning from
                // accept() drops the connection and can race the status flush.
                let _ = write_hello_status(&mut send, 0).await;
                connection.closed().await;
                Ok(())
            }
            Hello::Host { session } => {
                // Ack first so the host proceeds into serve_helper_bridge, then
                // hand the connection to the waiting guest task.
                let Some(tx) = self.rendezvous.take(session).await else {
                    let _ = write_hello_status(&mut send, 6).await;
                    return Ok(());
                };
                let _ = write_hello_status(&mut send, 0).await;
                if tx.send(connection).is_ok() {
                    tracing::info!("摊主已回连互助中继");
                }
                Ok(())
            }
            Hello::Guest { target, session } => {
                self.accept_guest(connection, send, target, session).await
            }
        }
    }
}

impl HelperHandler {
    async fn accept_guest(
        &self,
        connection: Connection,
        mut send: SendStream,
        target: EndpointId,
        session: SessionId,
    ) -> Result<(), AcceptError> {
        let guest_id = connection.remote_id();
        if !self.policy.allows_session(guest_id, target, &session) {
            let _ = write_hello_status(&mut send, 7).await;
            connection.close(0u8.into(), b"policy");
            return Ok(());
        }
        let Ok(permit) = self.sessions.clone().try_acquire_owned() else {
            let _ = write_hello_status(&mut send, 5).await;
            connection.close(0u8.into(), b"busy");
            return Ok(());
        };
        self.active.fetch_add(1, Ordering::Relaxed);
        let _guard = SessionGuard {
            active: self.active.clone(),
            _permit: permit,
        };
        if target == self.endpoint.id() {
            let _ = write_hello_status(&mut send, 2).await;
            return Ok(());
        }
        let host_rx = self.rendezvous.register(session).await;
        let invite = HostInvite {
            helper_id: self.endpoint.id(),
            host_id: target,
            guest_id,
            session,
            addrs: dialable_addrs(&self.endpoint, stored_listen_port(&self.listen_port)),
            relays: local_relay_urls(&self.endpoint),
        };
        let _ = self.invite_tx.try_send(invite.clone());
        tokio::spawn(notify_host_via_alpn(self.endpoint.clone(), invite));

        crate::netdebug::log(format!(
            "rendezvous_wait guest={guest_id} target={target} session={session:02x?} timeout_ms={}",
            RENDEZVOUS_WAIT.as_millis()
        ));
        let host = match tokio::time::timeout(RENDEZVOUS_WAIT, host_rx).await {
            Ok(Ok(host)) => {
                tracing::info!(guest = %guest_id, %target, "互助汇合成功（摊主回连）");
                crate::netdebug::log(format!(
                    "rendezvous_ok guest={guest_id} target={target} host_paths={}",
                    describe_conn_paths(&host)
                ));
                host
            }
            _ => {
                self.rendezvous.cancel(session).await;
                tracing::info!(%target, "摊主未及时回连，改由帮手拨摊主");
                crate::netdebug::log(format!(
                    "rendezvous_fallback guest={guest_id} target={target} reason=host_no_callback"
                ));
                match tokio::time::timeout(
                    HOST_DIAL,
                    self.endpoint
                        .connect(EndpointAddr::from(target), node::ALPN),
                )
                .await
                {
                    Ok(Ok(host)) => {
                        crate::netdebug::log(format!(
                            "fallback_host_ok target={target} paths={}",
                            describe_conn_paths(&host)
                        ));
                        host
                    }
                    Ok(Err(err)) => {
                        tracing::info!(%target, ?err, "helper could not reach host");
                        crate::netdebug::log(format!(
                            "fallback_host_err target={target} err={err:?}"
                        ));
                        let _ = write_hello_status(&mut send, 3).await;
                        return Ok(());
                    }
                    Err(_) => {
                        crate::netdebug::log(format!("fallback_host_timeout target={target}"));
                        let _ = write_hello_status(&mut send, 4).await;
                        return Ok(());
                    }
                }
            }
        };
        if write_hello_status(&mut send, 0).await.is_err() {
            host.close(0u8.into(), b"hello");
            return Ok(());
        }
        tracing::info!(guest = %guest_id, %target, "互助中继会话已接通");
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
            let budget = Arc::new(SessionBudget::default());
            tokio::spawn(async move {
                if let Err(err) = bridge_with_budget(guest_pair, host_pair, bandwidth, budget).await
                {
                    tracing::debug!(%err, "helper bridge ended");
                }
            });
        }
        host.close(0u8.into(), b"done");
        Ok(())
    }
}

#[derive(Clone)]
pub struct InviteHandler {
    hosting: watch::Receiver<bool>,
    stall: StallHandler,
    endpoint: Endpoint,
}

impl std::fmt::Debug for InviteHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InviteHandler")
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for InviteHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if !*self.hosting.borrow() {
            connection.close(0u8.into(), b"idle");
            return Ok(());
        }
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let parsed = match read_invite_wire(&mut recv).await {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::debug!(%err, "helper invite rejected");
                let _ = write_hello_status(&mut send, 1).await;
                return Ok(());
            }
        };
        if parsed.host_id != self.endpoint.id() {
            let _ = write_hello_status(&mut send, 2).await;
            return Ok(());
        }
        let invite = HostInvite {
            helper_id: connection.remote_id(),
            host_id: parsed.host_id,
            guest_id: parsed.guest_id,
            session: parsed.session,
            addrs: parsed.addrs,
            relays: parsed.relays,
        };
        if !claim_invite_session(invite.session) {
            let _ = write_hello_status(&mut send, 6).await;
            return Ok(());
        }
        let _ = write_hello_status(&mut send, 0).await;
        let stall = self.stall.clone();
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            if let Err(err) = answer_invite(endpoint, stall, invite).await {
                tracing::debug!(%err, "摊主回连互助失败");
            }
        });
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

pub fn start(
    endpoint: Endpoint,
    gossip: Gossip,
    plaza: Plaza,
    routes: Arc<RouteBook>,
    stall: StallHandler,
    data_dir: std::path::PathBuf,
    listen_port: Option<u16>,
) -> HelperHub {
    start_with_policy(
        endpoint,
        gossip,
        plaza,
        routes,
        stall,
        data_dir,
        listen_port,
        Arc::new(DefaultHelperPolicy),
    )
}

pub fn start_with_policy(
    endpoint: Endpoint,
    gossip: Gossip,
    plaza: Plaza,
    routes: Arc<RouteBook>,
    stall: StallHandler,
    data_dir: std::path::PathBuf,
    listen_port: Option<u16>,
    policy: Arc<dyn HelperPolicy>,
) -> HelperHub {
    let (enabled, _) = watch::channel(false);
    let (hosting, _) = watch::channel(false);
    let (invite_tx, invite_rx) = mpsc::channel::<HostInvite>(32);
    let limit_bps = Arc::new(AtomicU32::new(DEFAULT_LIMIT_BPS));
    let listen_port = Arc::new(AtomicU32::new(u32::from(
        crate::storage::helper_prefs::normalize_port(listen_port).unwrap_or(0),
    )));
    let active = Arc::new(AtomicUsize::new(0));
    let book = HelperBook::with_policy(policy.clone());
    let rendezvous = RendezvousBook::default();
    let hub = HelperHub {
        enabled: enabled.clone(),
        hosting: hosting.clone(),
        limit_bps: limit_bps.clone(),
        listen_port: listen_port.clone(),
        active: active.clone(),
        sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
        bandwidth: Arc::new(Mutex::new(Arc::new(BandwidthLimit::new(u64::from(
            DEFAULT_LIMIT_BPS,
        ))))),
        book: book.clone(),
        policy: policy.clone(),
        rendezvous,
        invite_tx,
        endpoint: endpoint.clone(),
        routes: routes.clone(),
        stall: stall.clone(),
        data_dir,
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
            hosting,
            invite_rx,
            limit_bps,
            listen_port,
            active,
            book,
            routes,
            endpoint,
            stall,
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
    hosting: watch::Sender<bool>,
    mut invite_rx: mpsc::Receiver<HostInvite>,
    limit_bps: Arc<AtomicU32>,
    listen_port: Arc<AtomicU32>,
    active: Arc<AtomicUsize>,
    book: HelperBook,
    routes: Arc<RouteBook>,
    endpoint: Endpoint,
    stall: StallHandler,
    self_id: EndpointId,
) {
    let mut enabled_rx = enabled.subscribe();
    let mut answered: HashMap<SessionId, Instant> = HashMap::new();
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
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut was_on = *enabled_rx.borrow();
        // Join plaza peers before the first announce, otherwise the first
        // broadcast goes into an empty helper mesh.
        refresh_helper_peers(&sender, &plaza).await;
        if was_on {
            broadcast_open(
                &sender,
                &secret,
                &limit_bps,
                &listen_port,
                &active,
                &endpoint,
            )
            .await;
        }
        loop {
            tokio::select! {
                changed = enabled_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let on = *enabled_rx.borrow();
                    if on {
                        refresh_helper_peers(&sender, &plaza).await;
                        broadcast_open(&sender, &secret, &limit_bps, &listen_port, &active, &endpoint).await;
                        was_on = true;
                    } else if was_on {
                        let _ = sender.broadcast(Bytes::from(encode_bye(&secret))).await;
                        was_on = false;
                    }
                }
                Some(invite) = invite_rx.recv() => {
                    let bytes = encode_invite_gossip(&secret, &invite);
                    if let Err(err) = sender.broadcast(Bytes::from(bytes)).await {
                        tracing::debug!(?err, "互助邀请 gossip 失败");
                    }
                }
                _ = tick.tick() => {
                    refresh_helper_peers(&sender, &plaza).await;
                    answered.retain(|_, at| at.elapsed() < Duration::from_secs(120));
                    if *enabled_rx.borrow() {
                        // Fixed listen port means the user set up router forwarding —
                        // don't auto-disable when STUN flaps; keep advertising.
                        let port = stored_listen_port(&listen_port);
                        let bound = bound_udp_ports(&endpoint);
                        if !reachability_ok(&endpoint, &routes, port, &bound) {
                            tracing::info!("互助中继已关闭：当前直连不到这台机器");
                            let _ = enabled.send(false);
                            continue;
                        }
                        broadcast_open(&sender, &secret, &limit_bps, &listen_port, &active, &endpoint).await;
                        was_on = true;
                    }
                }
                event = receiver.next() => {
                    let Some(event) = event else {
                        break;
                    };
                    match event {
                        Ok(Event::Received(message)) => {
                            if let Some(invite) = decode_invite_gossip(&message.content, card::unix_now()) {
                                if invite.host_id == self_id
                                    && *hosting.borrow()
                                    && claim_invite_session(invite.session)
                                {
                                    answered.insert(invite.session, Instant::now());
                                    let stall = stall.clone();
                                    let endpoint = endpoint.clone();
                                    tokio::spawn(async move {
                                        if let Err(err) = answer_invite(endpoint, stall, invite).await {
                                            tracing::debug!(%err, "gossip 邀请回连失败");
                                        }
                                    });
                                }
                                continue;
                            }
                            match decode_card(&message.content, card::unix_now()) {
                                Some(HelperCard::Open { endpoint_id, .. }) if endpoint_id == self_id => {}
                                Some(HelperCard::Bye { endpoint_id }) if endpoint_id == self_id => {}
                                Some(parsed) => {
                                    let who = match &parsed {
                                        HelperCard::Open { endpoint_id, .. } | HelperCard::Bye { endpoint_id } => {
                                            endpoint_id.to_string()
                                        }
                                    };
                                    book.ingest(parsed).await;
                                    tracing::info!(helper = %who, "收到互助中继广播");
                                }
                                None => {
                                    tracing::debug!(
                                        from = %message.delivered_from,
                                        bytes = message.content.len(),
                                        "互助广播无法解析"
                                    );
                                }
                            }
                        }
                        Ok(Event::NeighborUp(id)) => {
                            tracing::debug!(%id, "helper neighbor up");
                            let _ = sender.join_peers(vec![id]).await;
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

async fn refresh_helper_peers(sender: &GossipSender, plaza: &Plaza) {
    let peers = plaza.peer_ids(32).await;
    if peers.is_empty() {
        return;
    }
    if let Err(err) = sender.join_peers(peers).await {
        tracing::debug!(?err, "helper join failed");
    }
}

async fn broadcast_open(
    sender: &GossipSender,
    secret: &SecretKey,
    limit_bps: &Arc<AtomicU32>,
    listen_port: &Arc<AtomicU32>,
    active: &Arc<AtomicUsize>,
    endpoint: &Endpoint,
) {
    let load = active.load(Ordering::Relaxed).min(255) as u8;
    let port = stored_listen_port(listen_port);
    let addrs = dialable_addrs(endpoint, port);
    let relays = local_relay_urls(endpoint);
    // Tell iroh itself about the forwarded WAN socket so path selection / disco
    // use the same port we put on the helper card (not a random STUN mapping).
    publish_external_addrs(endpoint, &addrs).await;
    if addrs.is_empty() && relays.is_empty() {
        tracing::warn!(
            ?port,
            "互助广播暂时没有公网拨号地址或中继（等 STUN/有人直连后再带上 IP）"
        );
    }
    let bytes = encode_open_with_relays(
        secret,
        limit_bps.load(Ordering::Relaxed),
        load,
        &addrs,
        &relays,
    );
    match sender.broadcast(Bytes::from(bytes)).await {
        Ok(()) => tracing::info!(?addrs, ?relays, "已广播互助中继"),
        Err(err) => tracing::warn!(?err, "互助中继广播失败"),
    }
}

async fn publish_external_addrs(endpoint: &Endpoint, addrs: &[SocketAddr]) {
    for addr in addrs {
        endpoint.add_external_addr(*addr).await;
        crate::netdebug::log(format!("helper_external_addr {addr}"));
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
    if path_looks_direct(&direct) {
        return (direct, false);
    }
    // Give hole punch a short window before spending time on helpers.
    if wait_until_direct(&direct, PATH_WAIT).await {
        return (direct, false);
    }
    let offers = book.list(endpoint.id()).await;
    // Prefer a shared relay over a merely advertised public address.
    let allowed: Vec<(EndpointId, LiveHelper)> = offers
        .into_iter()
        .filter(|(id, live)| *id != target && book.allows_offer(*id, live))
        .collect();
    let candidates: Vec<HelperCandidate> = rank_candidates(&local_relay_urls(endpoint), allowed)
        .into_iter()
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
    // Serialize probes so overlapping UI refreshes wait instead of wiping the list.
    static PROBE_GATE: OnceLock<Mutex<()>> = OnceLock::new();
    let gate = PROBE_GATE.get_or_init(|| Mutex::new(()));
    let _probe_guard = gate.lock().await;

    let raw = book.list(self_id).await;
    let local_addr = format_endpoint_addr(&endpoint.addr());
    let local_relays = local_relay_urls(endpoint);
    crate::netdebug::log(format!(
        "probe_start self={} local={} offers={} exclude={:?}",
        self_id,
        local_addr,
        raw.len(),
        exclude.map(|id| id.to_string())
    ));
    let mut out = Vec::new();
    for (id, live) in raw {
        if exclude == Some(id) {
            crate::netdebug::log(format!("probe_skip helper={id} reason=current_host"));
            out.push(HelperOffer {
                endpoint_id: id.to_string(),
                limit_bps: live.limit_bps,
                load: live.load,
                rtt_ms: None,
                direct: false,
                relay: false,
                usable: false,
                note: "这是当前摊主，不能给自己的摊当桥".to_string(),
            });
            continue;
        }
        if !book.allows_offer(id, &live) {
            crate::netdebug::log(format!("probe_skip helper={id} reason=policy"));
            out.push(HelperOffer {
                endpoint_id: id.to_string(),
                limit_bps: live.limit_bps,
                load: live.load,
                rtt_ms: None,
                direct: false,
                relay: live.addrs.is_empty() && !live.relays.is_empty(),
                usable: false,
                note: "策略拒绝该帮手".to_string(),
            });
            continue;
        }
        let started = Instant::now();
        // Card already has a WAN socket: probe it with a relay-disabled endpoint.
        // The main endpoint merges cached n0 relays and happily "connects" via relay
        // while never opening an IP path — that made refresh look stuck for 10s+.
        if !live.addrs.is_empty() {
            let shared = shared_relays(&live.relays, &local_relays);
            match probe_ip_only(id, &live.addrs).await {
                Ok(rtt_ms) => {
                    crate::netdebug::log(format!(
                        "probe_result helper={id} mode=ip_only direct=true rtt_ms={rtt_ms}"
                    ));
                    out.push(HelperOffer {
                        endpoint_id: id.to_string(),
                        limit_bps: live.limit_bps,
                        load: live.load,
                        rtt_ms: Some(rtt_ms),
                        direct: true,
                        relay: false,
                        usable: true,
                        note: format!("公网直拨成功 · 约 {rtt_ms} ms"),
                    });
                }
                Err(err) => {
                    crate::netdebug::log(format!(
                        "probe_result helper={id} mode=ip_only direct=false err={err} elapsed_ms={}",
                        started.elapsed().as_millis()
                    ));
                    if shared.is_empty() {
                        out.push(HelperOffer {
                            endpoint_id: id.to_string(),
                            limit_bps: live.limit_bps,
                            load: live.load,
                            rtt_ms: None,
                            direct: false,
                            relay: false,
                            usable: false,
                            note: format!(
                                "公网 {} 直拨失败：检查端口转发/防火墙，或对方互助未开启/未在该端口监听",
                                live.addrs
                                    .first()
                                    .map(|a| a.to_string())
                                    .unwrap_or_default()
                            ),
                        });
                    } else {
                        match probe_relay(endpoint, id, &shared).await {
                            Ok(rtt_ms) => {
                                crate::netdebug::log(format!(
                                    "probe_result helper={id} mode=shared_relay direct=false rtt_ms={rtt_ms}"
                                ));
                                out.push(HelperOffer {
                                    endpoint_id: id.to_string(),
                                    limit_bps: live.limit_bps,
                                    load: live.load,
                                    rtt_ms: Some(rtt_ms),
                                    direct: false,
                                    relay: false,
                                    usable: true,
                                    note: format!("共享中继可达 · 约 {rtt_ms} ms"),
                                });
                            }
                            Err(relay_err) => {
                                crate::netdebug::log(format!(
                                    "probe_result helper={id} mode=shared_relay direct=false err={relay_err}"
                                ));
                                out.push(HelperOffer {
                                    endpoint_id: id.to_string(),
                                    limit_bps: live.limit_bps,
                                    load: live.load,
                                    rtt_ms: None,
                                    direct: false,
                                    relay: false,
                                    usable: false,
                                    note: format!("公网直拨失败，共享中继也不可用：{relay_err}"),
                                });
                            }
                        }
                    }
                }
            }
            continue;
        }

        let shared = shared_relays(&live.relays, &local_relays);
        if !shared.is_empty() {
            match probe_relay(endpoint, id, &shared).await {
                Ok(rtt_ms) => {
                    crate::netdebug::log(format!(
                        "probe_result helper={id} mode=shared_relay direct=false rtt_ms={rtt_ms}"
                    ));
                    out.push(HelperOffer {
                        endpoint_id: id.to_string(),
                        limit_bps: live.limit_bps,
                        load: live.load,
                        rtt_ms: Some(rtt_ms),
                        direct: false,
                        relay: true,
                        usable: true,
                        note: format!("共享中继可达 · 约 {rtt_ms} ms"),
                    });
                }
                Err(err) => {
                    crate::netdebug::log(format!(
                        "probe_result helper={id} mode=shared_relay direct=false err={err}"
                    ));
                    out.push(HelperOffer {
                        endpoint_id: id.to_string(),
                        limit_bps: live.limit_bps,
                        load: live.load,
                        rtt_ms: None,
                        direct: false,
                        relay: true,
                        usable: false,
                        note: format!("共享中继探测失败：{err}"),
                    });
                }
            }
            continue;
        }

        out.push(HelperOffer {
            endpoint_id: id.to_string(),
            limit_bps: live.limit_bps,
            load: live.load,
            rtt_ms: None,
            direct: false,
            relay: true,
            usable: false,
            note: "没有与本机共享的中继，无法探测该帮手".to_string(),
        });
        continue;
    }
    out.sort_by(|left, right| {
        right
            .usable
            .cmp(&left.usable)
            .then(
                left.rtt_ms
                    .unwrap_or(u32::MAX)
                    .cmp(&right.rtt_ms.unwrap_or(u32::MAX)),
            )
            .then(left.endpoint_id.cmp(&right.endpoint_id))
    });
    for offer in &mut out {
        offer.note = format!("互助桥不可信 · {}", offer.note);
    }
    out
}

async fn pick_helper(
    endpoint: &Endpoint,
    target: EndpointId,
    candidates: &[HelperCandidate],
) -> Option<Connection> {
    // A shared relay is an explicitly allowed path; an unshared relay hop is not.
    for candidate in candidates {
        match tokio::time::timeout(
            HELPER_SESSION,
            dial_helper(
                endpoint,
                candidate.id,
                target,
                &candidate.addrs,
                &candidate.relays,
            ),
        )
        .await
        {
            Ok(Ok((connection, known_direct))) => {
                if known_direct
                    || candidate.shared_relay
                    || ensure_has_direct(&connection, PATH_WAIT).await
                {
                    return Some(connection);
                }
                tracing::debug!(helper = %candidate.id, "互助节点也在走中继，跳过");
                connection.close(0u8.into(), b"skip");
            }
            Ok(Err(err)) => tracing::debug!(helper = %candidate.id, ?err, "互助中继不可用"),
            Err(_) => tracing::debug!(helper = %candidate.id, "互助中继探测超时"),
        }
    }
    None
}

fn fixed_port_ready(listen_port: Option<u16>, bound: &[u16]) -> bool {
    listen_port.is_some_and(|port| bound.contains(&port))
}

fn reachability_ok(
    endpoint: &Endpoint,
    routes: &RouteBook,
    listen_port: Option<u16>,
    bound: &[u16],
) -> bool {
    fixed_port_ready(listen_port, bound)
        || routes.inbound_direct()
        || has_global_dial_addr(endpoint)
        || has_local_relay(endpoint)
}

fn reach_snapshot(
    endpoint: &Endpoint,
    routes: &RouteBook,
    listen_port: Option<u16>,
    bound: &[u16],
) -> (bool, String) {
    if fixed_port_ready(listen_port, bound) {
        let port = listen_port.unwrap_or(0);
        let has_wan = has_global_dial_addr(endpoint);
        return (
            true,
            if has_wan {
                format!("已固定 UDP {port}，且已看到公网地址，适合当互助中继。")
            } else {
                format!(
                    "已固定 UDP {port}（按端口转发可开）。若广播里还没有公网 IP，等几秒或确认路由器已转发到本机。"
                )
            },
        );
    }
    if routes.inbound_direct() {
        return (true, "已有人直连打到过你，适合当互助中继。".to_string());
    }
    if has_global_dial_addr(endpoint) {
        return (
            true,
            "本机已有可被拨号的公网地址，适合当互助中继。".to_string(),
        );
    }
    if has_local_relay(endpoint) {
        return (
            true,
            "本机已连上公共中继，可作为只走中继的互助帮手。".to_string(),
        );
    }
    (
        false,
        "现在还不能当互助：没有公网地址，也还没有人打洞打到这台机器。可在上方填路由器已转发的 UDP 端口后重启再开。".to_string(),
    )
}

async fn wait_reachability(
    endpoint: &Endpoint,
    routes: &RouteBook,
    listen_port: Option<u16>,
    bound: &[u16],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let bound_now = bound_udp_ports(endpoint);
        let ports = if bound_now.is_empty() {
            bound
        } else {
            &bound_now
        };
        if reachability_ok(endpoint, routes, listen_port, ports) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(reach_snapshot(endpoint, routes, listen_port, ports).1);
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

fn has_local_relay(endpoint: &Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
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
            !(v6.is_unspecified() || v6.is_loopback() || v6.is_unicast_link_local() || unique_local)
        }
    }
}

/// Public IP:port pairs peers should dial (optionally forcing the forwarded port).
pub(crate) fn public_dial_addrs(endpoint: &Endpoint, force_port: Option<u16>) -> Vec<SocketAddr> {
    dialable_addrs(endpoint, force_port)
}

pub(crate) fn connection_is_direct(connection: &Connection) -> bool {
    path_looks_direct(connection)
}

fn dialable_addrs(endpoint: &Endpoint, force_port: Option<u16>) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = endpoint
        .addr()
        .ip_addrs()
        .copied()
        .filter(|socket| is_globally_reachable_ip(socket.ip()))
        .map(|socket| match force_port {
            Some(port) => SocketAddr::new(socket.ip(), port),
            None => socket,
        })
        .collect();
    out.sort_by_key(|socket| (socket.is_ipv6(), socket.to_string()));
    out.dedup();
    out.truncate(MAX_CARD_ADDRS);
    out
}

fn stored_listen_port(listen_port: &AtomicU32) -> Option<u16> {
    match listen_port.load(Ordering::Relaxed) {
        0 => None,
        port => u16::try_from(port).ok(),
    }
}

fn bound_udp_ports(endpoint: &Endpoint) -> Vec<u16> {
    let mut ports: Vec<u16> = endpoint
        .bound_sockets()
        .into_iter()
        .map(|socket| socket.port())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn bound_ports_label(ports: &[u16]) -> String {
    if ports.is_empty() {
        "未知端口".to_string()
    } else {
        ports
            .iter()
            .map(|port| port.to_string())
            .collect::<Vec<_>>()
            .join("/")
    }
}

fn port_status_note(listen_port: Option<u16>, bound: &[u16]) -> String {
    match listen_port {
        None => format!(
            "当前随机监听 UDP {}。若路由器做了固定端口转发，请在上方填写同一端口后重启闪摊。",
            bound_ports_label(bound)
        ),
        Some(want) if bound.contains(&want) => {
            format!("已按固定端口 UDP {want} 监听，请确认路由器把 UDP {want} 转到本机。")
        }
        Some(want) => format!(
            "已保存端口 {want}，但当前进程仍在监听 {}。请完全退出闪摊再打开。",
            bound_ports_label(bound)
        ),
    }
}

fn helper_endpoint_addr(id: EndpointId, addrs: &[SocketAddr], relays: &[RelayUrl]) -> EndpointAddr {
    let mut addr = EndpointAddr::new(id);
    for socket in addrs {
        addr = addr.with_ip_addr(*socket);
    }
    for relay in relays {
        addr = addr.with_relay_url(relay.clone());
    }
    addr
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HelperCandidate {
    id: EndpointId,
    addrs: Vec<SocketAddr>,
    relays: Vec<RelayUrl>,
    shared_relay: bool,
}

fn local_relay_urls(endpoint: &Endpoint) -> Vec<RelayUrl> {
    let mut relays: Vec<RelayUrl> = endpoint.addr().relay_urls().cloned().collect();
    relays.sort();
    relays.dedup();
    relays
}

fn shared_relays(advertised: &[RelayUrl], local: &[RelayUrl]) -> Vec<RelayUrl> {
    let mut relays: Vec<RelayUrl> = advertised
        .iter()
        .filter(|relay| local.contains(relay))
        .cloned()
        .collect();
    relays.sort();
    relays.dedup();
    relays
}

fn rank_candidates(
    local: &[RelayUrl],
    offers: Vec<(EndpointId, LiveHelper)>,
) -> Vec<HelperCandidate> {
    let mut candidates: Vec<HelperCandidate> = offers
        .into_iter()
        .map(|(id, live)| {
            let relays = shared_relays(&live.relays, local);
            HelperCandidate {
                id,
                addrs: live.addrs,
                shared_relay: !relays.is_empty(),
                relays,
            }
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .shared_relay
            .cmp(&left.shared_relay)
            .then(left.addrs.is_empty().cmp(&right.addrs.is_empty()))
            .then(right.relays.len().cmp(&left.relays.len()))
            .then(left.id.as_bytes().cmp(right.id.as_bytes()))
    });
    candidates
}

/// Shared relay-disabled endpoint so public-IP dials never fall back to n0.
async fn ip_only_endpoint() -> anyhow::Result<Endpoint> {
    static CELL: OnceLock<Mutex<Option<Endpoint>>> = OnceLock::new();
    let cell = CELL.get_or_init(|| Mutex::new(None));
    let mut slot = cell.lock().await;
    if let Some(existing) = slot.as_ref() {
        if !existing.is_closed() {
            return Ok(existing.clone());
        }
    }
    // Minimal installs the rustls CryptoProvider; Empty does not and bind() fails
    // with "Missing or incompatible rustls crypto provider configured".
    let fresh = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .map_err(|err| anyhow::anyhow!("直连探测端点启动失败: {err:?}"))?;
    crate::netdebug::log(format!(
        "ip_only_endpoint_bound self={} sockets={:?}",
        fresh.id(),
        fresh.bound_sockets()
    ));
    *slot = Some(fresh.clone());
    Ok(fresh)
}

/// Dial `id` using only the given sockets — no relay, no main-endpoint address book.
pub(crate) async fn connect_ip_only(
    id: EndpointId,
    addrs: &[SocketAddr],
    alpn: &[u8],
    timeout: Duration,
) -> anyhow::Result<Connection> {
    if addrs.is_empty() {
        anyhow::bail!("没有公网地址可直拨");
    }
    let ep = ip_only_endpoint().await?;
    let dial = helper_endpoint_addr(id, addrs, &[]);
    crate::netdebug::log(format!(
        "ip_only_dial helper={id} dial={}",
        format_endpoint_addr(&dial)
    ));
    let started = Instant::now();
    let connection = tokio::time::timeout(timeout, ep.connect(dial, alpn))
        .await
        .map_err(|_| anyhow::anyhow!("公网直拨超时"))?
        .map_err(|err| anyhow::anyhow!("公网直拨失败: {err:?}"))?;
    crate::netdebug::log(format!(
        "ip_only_connected helper={id} elapsed_ms={} paths={}",
        started.elapsed().as_millis(),
        describe_conn_paths(&connection)
    ));
    Ok(connection)
}

/// Connect + Probe hello so "usable" means helper is on and answering, not just
/// that QUIC handshake completed against a closed accept handler.
async fn probe_ip_only(id: EndpointId, addrs: &[SocketAddr]) -> anyhow::Result<u32> {
    let started = Instant::now();
    let connection = connect_ip_only(id, addrs, ALPN, IP_ONLY_DIAL).await?;
    let result = async {
        let (mut send, mut recv) =
            tokio::time::timeout(Duration::from_secs(3), connection.open_bi())
                .await
                .map_err(|_| anyhow::anyhow!("帮手未响应探测流"))?
                .map_err(|err| anyhow::anyhow!("打开探测流失败: {err:?}"))?;
        write_hello_probe(&mut send).await?;
        let status = read_hello_status(&mut recv).await?;
        // 0 = modern helper ack. 1 = older helper rejected unknown ROLE_PROBE
        // but still answered on this ALPN — UDP path and accept loop work.
        if status != 0 && status != 1 {
            anyhow::bail!("帮手拒绝探测 ({status})");
        }
        if status == 1 {
            crate::netdebug::log(format!("probe_hello_legacy helper={id} status=1"));
        }
        Ok(started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32)
    }
    .await;
    connection.close(0u8.into(), b"probe");
    result
}

async fn probe_relay(
    endpoint: &Endpoint,
    id: EndpointId,
    relays: &[RelayUrl],
) -> anyhow::Result<u32> {
    let started = Instant::now();
    let dial = helper_endpoint_addr(id, &[], relays);
    crate::netdebug::log(format!(
        "probe_relay_dial helper={id} dial={}",
        format_endpoint_addr(&dial)
    ));
    let connection = tokio::time::timeout(Duration::from_secs(15), endpoint.connect(dial, ALPN))
        .await
        .map_err(|_| anyhow::anyhow!("共享中继探测超时"))?
        .map_err(|err| anyhow::anyhow!("共享中继探测失败: {err:?}"))?;
    let result = async {
        let (mut send, mut recv) =
            tokio::time::timeout(Duration::from_secs(3), connection.open_bi())
                .await
                .map_err(|_| anyhow::anyhow!("帮手未响应探测流"))?
                .map_err(|err| anyhow::anyhow!("打开探测流失败: {err:?}"))?;
        write_hello_probe(&mut send).await?;
        let status = read_hello_status(&mut recv).await?;
        if status != 0 && status != 1 {
            anyhow::bail!("帮手拒绝探测 ({status})");
        }
        let elapsed = started.elapsed().as_millis().min(u128::from(u32::MAX));
        Ok(u32::try_from(elapsed).unwrap_or(u32::MAX))
    }
    .await;
    crate::netdebug::log(format!(
        "probe_relay_result helper={id} elapsed_ms={} paths={}",
        started.elapsed().as_millis(),
        describe_conn_paths(&connection)
    ));
    connection.close(0u8.into(), b"probe");
    result
}

/// Deduplicate ALPN + gossip invite delivery for the same session.
fn claim_invite_session(session: SessionId) -> bool {
    static RECENT: OnceLock<std::sync::Mutex<HashMap<SessionId, Instant>>> = OnceLock::new();
    let map = RECENT.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let Ok(mut guard) = map.lock() else {
        return true;
    };
    guard.retain(|_, at| at.elapsed() < Duration::from_secs(120));
    match guard.entry(session) {
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(Instant::now());
            true
        }
        std::collections::hash_map::Entry::Occupied(_) => false,
    }
}

/// Returns `(connection, known_direct)` — `known_direct` is true when the
/// dial used the relay-disabled endpoint (definitionally not n0).
async fn dial_helper(
    endpoint: &Endpoint,
    helper: EndpointId,
    target: EndpointId,
    addrs: &[SocketAddr],
    relays: &[RelayUrl],
) -> anyhow::Result<(Connection, bool)> {
    if addrs.is_empty() && relays.is_empty() {
        anyhow::bail!("帮手没有公网地址或共享中继");
    }
    let (connection, known_direct) = if !addrs.is_empty() {
        match connect_ip_only(helper, addrs, ALPN, HELPER_DIAL).await {
            Ok(conn) => (conn, true),
            Err(err) => {
                crate::netdebug::log(format!(
                    "session_ip_only_fail helper={helper} err={err}; fallback_main"
                ));
                let dial = helper_endpoint_addr(helper, addrs, relays);
                crate::netdebug::log(format!(
                    "session_dial helper={helper} target={target} dial={}",
                    format_endpoint_addr(&dial)
                ));
                let conn = endpoint
                    .connect(dial, ALPN)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
                (conn, false)
            }
        }
    } else {
        let dial = helper_endpoint_addr(helper, addrs, relays);
        crate::netdebug::log(format!(
            "session_dial helper={helper} target={target} dial={}",
            format_endpoint_addr(&dial)
        ));
        let conn = endpoint
            .connect(dial, ALPN)
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;
        (conn, false)
    };
    crate::netdebug::log(format!(
        "session_connected helper={helper} known_direct={known_direct} paths={}",
        describe_conn_paths(&connection)
    ));
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let session = new_session_id();
    write_hello_guest(&mut send, target, session).await?;
    crate::netdebug::log(format!(
        "session_hello_sent helper={helper} target={target} session={session:02x?}"
    ));
    let status = read_hello_status(&mut recv).await?;
    crate::netdebug::log(format!(
        "session_hello_ack helper={helper} status={status} paths={}",
        describe_conn_paths(&connection)
    ));
    if status != 0 {
        connection.close(0u8.into(), b"reject");
        anyhow::bail!("互助中继拒绝了这次转发 ({status})");
    }
    Ok((connection, known_direct))
}

async fn notify_host_via_alpn(endpoint: Endpoint, invite: HostInvite) {
    crate::netdebug::log(format!(
        "invite_alpn_start host={} guest={} session={:02x?}",
        invite.host_id, invite.guest_id, invite.session
    ));
    let Ok(Ok(connection)) = tokio::time::timeout(
        Duration::from_secs(8),
        endpoint.connect(EndpointAddr::from(invite.host_id), INVITE_ALPN),
    )
    .await
    else {
        crate::netdebug::log(format!(
            "invite_alpn_fail host={} reason=connect_timeout_or_err",
            invite.host_id
        ));
        return;
    };
    crate::netdebug::log(format!(
        "invite_alpn_connected host={} paths={}",
        invite.host_id,
        describe_conn_paths(&connection)
    ));
    let Ok((mut send, mut recv)) = connection.open_bi().await else {
        return;
    };
    if write_invite_wire(&mut send, &invite).await.is_err() {
        return;
    }
    let status = read_hello_status(&mut recv).await.unwrap_or(255);
    crate::netdebug::log(format!(
        "invite_alpn_ack host={} status={status}",
        invite.host_id
    ));
    connection.close(0u8.into(), b"invite");
}

async fn answer_invite(
    endpoint: Endpoint,
    stall: StallHandler,
    invite: HostInvite,
) -> anyhow::Result<()> {
    if !stall.is_hosting().await {
        anyhow::bail!("摊位已打烊");
    }
    let relays = shared_relays(&invite.relays, &local_relay_urls(&endpoint));
    crate::netdebug::log(format!(
        "host_callback_dial helper={} session={:02x?} addrs={:?} relays={:?}",
        invite.helper_id, invite.session, invite.addrs, relays
    ));
    let connection = if !invite.addrs.is_empty() {
        match connect_ip_only(invite.helper_id, &invite.addrs, ALPN, HELPER_DIAL).await {
            Ok(conn) => conn,
            Err(err) => {
                crate::netdebug::log(format!(
                    "host_callback_ip_only_fail helper={} err={err}; fallback_main",
                    invite.helper_id
                ));
                let dial = helper_endpoint_addr(invite.helper_id, &invite.addrs, &relays);
                tokio::time::timeout(HELPER_DIAL, endpoint.connect(dial, ALPN))
                    .await
                    .map_err(|_| anyhow::anyhow!("回连帮手超时"))?
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
            }
        }
    } else {
        let dial = helper_endpoint_addr(invite.helper_id, &invite.addrs, &relays);
        tokio::time::timeout(HELPER_DIAL, endpoint.connect(dial, ALPN))
            .await
            .map_err(|_| anyhow::anyhow!("回连帮手超时"))?
            .map_err(|err| anyhow::anyhow!("{err:?}"))?
    };
    crate::netdebug::log(format!(
        "host_callback_connected helper={} paths={}",
        invite.helper_id,
        describe_conn_paths(&connection)
    ));
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    write_hello_host(&mut send, invite.session).await?;
    let status = read_hello_status(&mut recv).await?;
    crate::netdebug::log(format!(
        "host_callback_ack helper={} status={status}",
        invite.helper_id
    ));
    if status != 0 {
        connection.close(0u8.into(), b"reject");
        anyhow::bail!("帮手拒绝摊主回连 ({status})");
    }
    let direct = wait_until_direct(&connection, PATH_WAIT).await;
    crate::netdebug::log(format!(
        "host_callback_ready helper={} direct={direct} paths={}",
        invite.helper_id,
        describe_conn_paths(&connection)
    ));
    tracing::info!(helper = %invite.helper_id, "摊主已接到互助会话");
    stall.serve_helper_bridge(connection).await;
    Ok(())
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

fn has_ip_path(connection: &Connection) -> bool {
    connection.paths().iter().any(|path| !path.is_relay())
}

fn path_looks_direct(connection: &Connection) -> bool {
    has_ip_path(connection) || selected_is_relay(connection) == Some(false)
}

/// True when the connection already has (or quickly gains) a non-relay path.
async fn ensure_has_direct(connection: &Connection, timeout: Duration) -> bool {
    if path_looks_direct(connection) {
        return true;
    }
    wait_until_direct(connection, timeout).await
}

/// Wait for hole punch to promote off the public relay.
async fn wait_until_direct(connection: &Connection, timeout: Duration) -> bool {
    let remote = connection.remote_id();
    if path_looks_direct(connection) {
        crate::netdebug::log(format!(
            "path_already_direct remote={remote} paths={}",
            describe_conn_paths(connection)
        ));
        return true;
    }
    crate::netdebug::log(format!(
        "path_wait_direct remote={remote} timeout_ms={} paths={}",
        timeout.as_millis(),
        describe_conn_paths(connection)
    ));
    let deadline = Instant::now() + timeout;
    let mut events = connection.path_events();
    loop {
        if path_looks_direct(connection) {
            crate::netdebug::log(format!(
                "path_became_direct remote={remote} paths={}",
                describe_conn_paths(connection)
            ));
            return true;
        }
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            let direct = path_looks_direct(connection);
            crate::netdebug::log(format!(
                "path_wait_done remote={remote} direct={direct} paths={}",
                describe_conn_paths(connection)
            ));
            return direct;
        }
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(ev) => {
                        crate::netdebug::log(format!(
                            "path_event remote={remote} event={ev:?} paths={}",
                            describe_conn_paths(connection)
                        ));
                        continue;
                    }
                    None => {
                        let direct = path_looks_direct(connection);
                        crate::netdebug::log(format!(
                            "path_events_closed remote={remote} direct={direct}"
                        ));
                        return direct;
                    }
                }
            }
            _ = tokio::time::sleep(rest) => {
                let direct = path_looks_direct(connection);
                crate::netdebug::log(format!(
                    "path_wait_timeout remote={remote} direct={direct} paths={}",
                    describe_conn_paths(connection)
                ));
                return direct;
            }
        }
    }
}

fn describe_conn_paths(connection: &Connection) -> String {
    let paths = connection.paths();
    let mut parts = Vec::new();
    for path in &paths {
        let selected = if path.is_selected() { "*" } else { "" };
        let kind = if path.is_relay() { "relay" } else { "direct" };
        parts.push(format!(
            "{selected}{kind}:{}:{}ms",
            path.remote_addr(),
            path.rtt().as_millis()
        ));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(" | ")
    }
}

fn format_endpoint_addr(addr: &EndpointAddr) -> String {
    let ips: Vec<String> = addr.ip_addrs().map(|s| s.to_string()).collect();
    let relays: Vec<String> = addr.relay_urls().map(|u| u.to_string()).collect();
    format!("id={} ips={ips:?} relays={relays:?}", addr.id)
}

async fn bridge(
    guest: (SendStream, RecvStream),
    host: (SendStream, RecvStream),
    bandwidth: Arc<BandwidthLimit>,
) -> anyhow::Result<()> {
    bridge_with_budget(guest, host, bandwidth, Arc::new(SessionBudget::default())).await
}

async fn bridge_with_budget(
    guest: (SendStream, RecvStream),
    host: (SendStream, RecvStream),
    bandwidth: Arc<BandwidthLimit>,
    budget: Arc<SessionBudget>,
) -> anyhow::Result<()> {
    let (mut guest_send, mut guest_recv) = guest;
    let (mut host_send, mut host_recv) = host;
    // Request frames finish quickly; the stall response is written afterwards.
    // `select!` used to cancel the downlink as soon as the uplink finished,
    // so dual-NAT visits via a helper saw "P2P 流提前结束" before any magic.
    let up = {
        let bandwidth = bandwidth.clone();
        let budget = budget.clone();
        async move {
            copy_limited_with_budget(&mut guest_recv, &mut host_send, bandwidth, &budget).await?;
            let _ = host_send.finish();
            Ok::<(), anyhow::Error>(())
        }
    };
    let down = async move {
        copy_limited_with_budget(&mut host_recv, &mut guest_send, bandwidth, &budget).await?;
        let _ = guest_send.finish();
        Ok::<(), anyhow::Error>(())
    };
    let (up_result, down_result) = tokio::join!(up, down);
    up_result?;
    down_result?;
    Ok(())
}

async fn copy_limited_with_budget(
    from: &mut RecvStream,
    into: &mut SendStream,
    bandwidth: Arc<BandwidthLimit>,
    budget: &SessionBudget,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let size = tokio::io::AsyncReadExt::read(from, &mut buf)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        if size == 0 {
            break;
        }
        let nbytes = u64::try_from(size).unwrap_or(u64::MAX);
        budget
            .charge_bridge(nbytes)
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        bandwidth.consume(nbytes).await;
        into.write_all(&buf[..size])
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
    }
    Ok(())
}

#[derive(Debug)]
enum Hello {
    Guest {
        target: EndpointId,
        session: SessionId,
    },
    Host {
        session: SessionId,
    },
    Probe,
}

fn new_session_id() -> SessionId {
    let bytes = SecretKey::generate().to_bytes();
    let mut session = [0u8; 16];
    session.copy_from_slice(&bytes[..16]);
    session
}

async fn write_hello_guest(
    send: &mut SendStream,
    target: EndpointId,
    session: SessionId,
) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(4 + 1 + 1 + 32 + 16);
    frame.extend_from_slice(HELLO_MAGIC);
    frame.push(2);
    frame.push(ROLE_GUEST);
    frame.extend_from_slice(target.as_bytes());
    frame.extend_from_slice(&session);
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn write_hello_host(send: &mut SendStream, session: SessionId) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(4 + 1 + 1 + 16);
    frame.extend_from_slice(HELLO_MAGIC);
    frame.push(2);
    frame.push(ROLE_HOST);
    frame.extend_from_slice(&session);
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn write_hello_probe(send: &mut SendStream) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(4 + 1 + 1);
    frame.extend_from_slice(HELLO_MAGIC);
    frame.push(2);
    frame.push(ROLE_PROBE);
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn read_hello(recv: &mut RecvStream) -> anyhow::Result<Hello> {
    let bytes = recv.read_to_end(4 + 1 + 1 + 32 + 16).await?;
    if bytes.len() < 5 || &bytes[..4] != HELLO_MAGIC {
        anyhow::bail!("hello 无效");
    }
    match bytes[4] {
        1 => {
            if bytes.len() != 4 + 1 + 32 {
                anyhow::bail!("hello 无效");
            }
            let target = PublicKey::from_bytes(bytes[5..37].try_into()?)?;
            Ok(Hello::Guest {
                target,
                session: new_session_id(),
            })
        }
        2 => {
            let role = *bytes.get(5).ok_or_else(|| anyhow::anyhow!("hello 无效"))?;
            match role {
                ROLE_GUEST => {
                    if bytes.len() != 4 + 1 + 1 + 32 + 16 {
                        anyhow::bail!("hello 无效");
                    }
                    let target = PublicKey::from_bytes(bytes[6..38].try_into()?)?;
                    let mut session = [0u8; 16];
                    session.copy_from_slice(&bytes[38..54]);
                    Ok(Hello::Guest { target, session })
                }
                ROLE_HOST => {
                    if bytes.len() != 4 + 1 + 1 + 16 {
                        anyhow::bail!("hello 无效");
                    }
                    let mut session = [0u8; 16];
                    session.copy_from_slice(&bytes[6..22]);
                    Ok(Hello::Host { session })
                }
                ROLE_PROBE => {
                    if bytes.len() != 4 + 1 + 1 {
                        anyhow::bail!("hello 无效");
                    }
                    Ok(Hello::Probe)
                }
                _ => anyhow::bail!("hello 角色无效"),
            }
        }
        _ => anyhow::bail!("hello 版本无效"),
    }
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

struct InviteWire {
    host_id: EndpointId,
    guest_id: EndpointId,
    session: SessionId,
    addrs: Vec<SocketAddr>,
    relays: Vec<RelayUrl>,
}

async fn write_invite_wire(send: &mut SendStream, invite: &HostInvite) -> anyhow::Result<()> {
    let version = if relay_count(&invite.relays) == 0 {
        INVITE_VERSION_V1
    } else {
        INVITE_VERSION_V2
    };
    let mut frame = Vec::new();
    frame.extend_from_slice(INVITE_MAGIC);
    frame.push(version);
    frame.extend_from_slice(&invite.session);
    frame.extend_from_slice(invite.host_id.as_bytes());
    frame.extend_from_slice(invite.guest_id.as_bytes());
    encode_addrs(&mut frame, &invite.addrs);
    if version == INVITE_VERSION_V2 {
        encode_relays(&mut frame, &invite.relays);
    }
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn read_invite_wire(recv: &mut RecvStream) -> anyhow::Result<InviteWire> {
    let bytes = recv.read_to_end(2048).await?;
    if bytes.len() < 4 + 1 + 16 + 32 + 32 + 1 || &bytes[..4] != INVITE_MAGIC {
        anyhow::bail!("invite 无效");
    }
    let version = bytes[4];
    if version != INVITE_VERSION_V1 && version != INVITE_VERSION_V2 {
        anyhow::bail!("invite 版本无效");
    }
    let mut session = [0u8; 16];
    session.copy_from_slice(&bytes[5..21]);
    let host_id = PublicKey::from_bytes(bytes[21..53].try_into()?)?;
    let guest_id = PublicKey::from_bytes(bytes[53..85].try_into()?)?;
    let mut cursor = &bytes[85..];
    let addrs = decode_addrs(&mut cursor).ok_or_else(|| anyhow::anyhow!("invite 地址无效"))?;
    let relays = if version == INVITE_VERSION_V2 {
        decode_relays(&mut cursor).ok_or_else(|| anyhow::anyhow!("invite 中继无效"))?
    } else {
        Vec::new()
    };
    if !cursor.is_empty() {
        anyhow::bail!("invite 多余字节");
    }
    Ok(InviteWire {
        host_id,
        guest_id,
        session,
        addrs,
        relays,
    })
}

fn encode_invite_gossip(secret: &SecretKey, invite: &HostInvite) -> Vec<u8> {
    let version = if relay_count(&invite.relays) == 0 {
        INVITE_VERSION_V1
    } else {
        INVITE_VERSION_V2
    };
    let mut body = Vec::new();
    body.extend_from_slice(INVITE_MAGIC);
    body.push(version);
    body.extend_from_slice(secret.public().as_bytes());
    body.extend_from_slice(invite.host_id.as_bytes());
    body.extend_from_slice(invite.guest_id.as_bytes());
    body.extend_from_slice(&invite.session);
    body.extend_from_slice(&card::unix_now().to_be_bytes());
    encode_addrs(&mut body, &invite.addrs);
    if version == INVITE_VERSION_V2 {
        encode_relays(&mut body, &invite.relays);
    }
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn decode_invite_gossip(bytes: &[u8], now: u64) -> Option<HostInvite> {
    if bytes.len() < 4 + 1 + 32 + 32 + 32 + 16 + 8 + 1 + 64 || &bytes[..4] != INVITE_MAGIC {
        return None;
    }
    let signed_len = bytes.len() - 64;
    let signed = &bytes[..signed_len];
    let mut cursor = &bytes[4..signed_len];
    let version = *cursor.first()?;
    if version != INVITE_VERSION_V1 && version != INVITE_VERSION_V2 {
        return None;
    }
    cursor = &cursor[1..];
    let helper_id = PublicKey::from_bytes(cursor.get(..32)?.try_into().ok()?).ok()?;
    cursor = &cursor[32..];
    let host_id = PublicKey::from_bytes(cursor.get(..32)?.try_into().ok()?).ok()?;
    cursor = &cursor[32..];
    let guest_id = PublicKey::from_bytes(cursor.get(..32)?.try_into().ok()?).ok()?;
    cursor = &cursor[32..];
    let mut session = [0u8; 16];
    session.copy_from_slice(cursor.get(..16)?);
    cursor = &cursor[16..];
    let timestamp = u64::from_be_bytes(cursor.get(..8)?.try_into().ok()?);
    cursor = &cursor[8..];
    let addrs = decode_addrs(&mut cursor)?;
    let relays = if version == INVITE_VERSION_V2 {
        decode_relays(&mut cursor)?
    } else {
        Vec::new()
    };
    if !cursor.is_empty() {
        return None;
    }
    let signature = Signature::from_bytes(bytes[signed_len..].try_into().ok()?);
    helper_id.verify(signed, &signature).ok()?;
    if timestamp == 0 || timestamp > now.saturating_add(180) {
        return None;
    }
    if timestamp + 90 < now {
        return None;
    }
    Some(HostInvite {
        helper_id,
        host_id,
        guest_id,
        session,
        addrs,
        relays,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelperCard {
    Open {
        endpoint_id: PublicKey,
        limit_bps: u32,
        load: u8,
        timestamp: u64,
        ttl: u32,
        addrs: Vec<SocketAddr>,
        relays: Vec<RelayUrl>,
    },
    Bye {
        endpoint_id: PublicKey,
    },
}

fn encode_open(secret: &SecretKey, limit_bps: u32, load: u8, addrs: &[SocketAddr]) -> Vec<u8> {
    encode_open_with_relays(secret, limit_bps, load, addrs, &[])
}

fn encode_open_with_relays(
    secret: &SecretKey,
    limit_bps: u32,
    load: u8,
    addrs: &[SocketAddr],
    relays: &[RelayUrl],
) -> Vec<u8> {
    let limit = clamp_limit(limit_bps);
    let version = if relay_count(relays) == 0 {
        CARD_VERSION_V1
    } else {
        CARD_VERSION_V2
    };
    let mut body = Vec::with_capacity(4 + 1 + 1 + 32 + 8 + 4 + 4 + 1 + 1 + 64);
    body.extend_from_slice(CARD_MAGIC);
    body.push(version);
    body.push(KIND_OPEN);
    body.extend_from_slice(secret.public().as_bytes());
    // Same clock basis as plaza cards so peers with different uptimes still accept us.
    body.extend_from_slice(&card::unix_now().to_be_bytes());
    body.extend_from_slice(&CARD_TTL_SECS.to_be_bytes());
    body.extend_from_slice(&limit.to_be_bytes());
    body.push(load);
    encode_addrs(&mut body, addrs);
    if version == CARD_VERSION_V2 {
        encode_relays(&mut body, relays);
    }
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn encode_addrs(body: &mut Vec<u8>, addrs: &[SocketAddr]) {
    let n = addrs.len().min(MAX_CARD_ADDRS) as u8;
    body.push(n);
    for socket in addrs.iter().take(MAX_CARD_ADDRS) {
        match socket.ip() {
            IpAddr::V4(ip) => {
                body.push(4);
                body.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                body.push(6);
                body.extend_from_slice(&ip.octets());
            }
        }
        body.extend_from_slice(&socket.port().to_be_bytes());
    }
}

fn decode_addrs(cursor: &mut &[u8]) -> Option<Vec<SocketAddr>> {
    if cursor.is_empty() {
        // Legacy cards without an address trailer.
        return Some(Vec::new());
    }
    let n = *cursor.first()? as usize;
    *cursor = &cursor[1..];
    if n > MAX_CARD_ADDRS {
        return None;
    }
    let mut addrs = Vec::with_capacity(n);
    for _ in 0..n {
        let family = *cursor.first()?;
        *cursor = &cursor[1..];
        let ip = match family {
            4 => {
                let octets: [u8; 4] = cursor.get(..4)?.try_into().ok()?;
                *cursor = &cursor[4..];
                IpAddr::V4(Ipv4Addr::from(octets))
            }
            6 => {
                let octets: [u8; 16] = cursor.get(..16)?.try_into().ok()?;
                *cursor = &cursor[16..];
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            _ => return None,
        };
        let port = u16::from_be_bytes(cursor.get(..2)?.try_into().ok()?);
        *cursor = &cursor[2..];
        if is_globally_reachable_ip(ip) {
            addrs.push(SocketAddr::new(ip, port));
        }
    }
    Some(addrs)
}

fn relay_count(relays: &[RelayUrl]) -> usize {
    relays
        .iter()
        .take(MAX_CARD_RELAYS)
        .filter(|relay| relay.as_str().len() <= MAX_RELAY_URL_BYTES)
        .count()
}

fn encode_relays(body: &mut Vec<u8>, relays: &[RelayUrl]) {
    let encoded: Vec<&[u8]> = relays
        .iter()
        .take(MAX_CARD_RELAYS)
        .map(|relay| relay.as_str().as_bytes())
        .filter(|bytes| bytes.len() <= MAX_RELAY_URL_BYTES)
        .collect();
    body.push(u8::try_from(encoded.len()).unwrap_or(u8::MAX));
    for bytes in encoded {
        let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
        body.extend_from_slice(&len.to_be_bytes());
        body.extend_from_slice(bytes);
    }
}

fn decode_relays(cursor: &mut &[u8]) -> Option<Vec<RelayUrl>> {
    let count = usize::from(*cursor.first()?);
    *cursor = &cursor[1..];
    if count > MAX_CARD_RELAYS {
        return None;
    }
    let mut relays = Vec::with_capacity(count);
    for _ in 0..count {
        let len = usize::from(u16::from_be_bytes(cursor.get(..2)?.try_into().ok()?));
        *cursor = &cursor[2..];
        if len == 0 || len > MAX_RELAY_URL_BYTES {
            return None;
        }
        let raw = cursor.get(..len)?;
        *cursor = &cursor[len..];
        let text = std::str::from_utf8(raw).ok()?;
        let url = url::Url::parse(text).ok()?;
        if url.scheme() != "https" {
            return None;
        }
        let Some(host) = url.host_str() else {
            return None;
        };
        if host.is_empty() || !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        let after_scheme = text.get(url.scheme().len() + 1..)?;
        if !after_scheme.starts_with("//") || after_scheme[2..].starts_with('/') {
            return None;
        }
        let relay = RelayUrl::from(url);
        if !relays.contains(&relay) {
            relays.push(relay);
        }
    }
    Some(relays)
}

fn encode_bye(secret: &SecretKey) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + 1 + 1 + 32 + 8 + 4 + 64);
    body.extend_from_slice(CARD_MAGIC);
    body.push(1);
    body.push(KIND_BYE);
    body.extend_from_slice(secret.public().as_bytes());
    body.extend_from_slice(&card::unix_now().to_be_bytes());
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
    if version != CARD_VERSION_V1 && version != CARD_VERSION_V2 {
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
            let addrs = decode_addrs(&mut cursor)?;
            let relays = if version == CARD_VERSION_V2 {
                decode_relays(&mut cursor)?
            } else {
                Vec::new()
            };
            if !cursor.is_empty() {
                return None;
            }
            Some(HelperCard::Open {
                endpoint_id,
                limit_bps: clamp_limit(limit_bps),
                load,
                timestamp,
                ttl,
                addrs,
                relays,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_cards_round_trip_and_reject_tampering() {
        let key = SecretKey::generate();
        let addrs = vec!["1.2.3.4:1234".parse().unwrap()];
        let open = encode_open(&key, DEFAULT_LIMIT_BPS, 2, &addrs);
        let parsed = decode_card(&open, card::unix_now()).unwrap();
        match parsed {
            HelperCard::Open {
                endpoint_id,
                limit_bps,
                load,
                addrs: got,
                ..
            } => {
                assert_eq!(endpoint_id, key.public());
                assert_eq!(limit_bps, DEFAULT_LIMIT_BPS);
                assert_eq!(load, 2);
                assert_eq!(got, addrs);
            }
            HelperCard::Bye { .. } => panic!("expected open"),
        }
        let mut bad = open;
        *bad.last_mut().unwrap() ^= 1;
        assert!(decode_card(&bad, card::unix_now()).is_none());
        let bye = encode_bye(&key);
        assert!(matches!(
            decode_card(&bye, card::unix_now()),
            Some(HelperCard::Bye { .. })
        ));
    }

    #[test]
    fn helper_cards_accept_across_different_process_uptimes() {
        let key = SecretKey::generate();
        let open = encode_open(&key, DEFAULT_LIMIT_BPS, 0, &[]);
        // Receiver "uptime" must not matter: unix time is shared.
        assert!(decode_card(&open, card::unix_now()).is_some());
        assert!(decode_card(&open, 1).is_none());
    }

    #[test]
    fn invite_gossip_round_trips() {
        let key = SecretKey::generate();
        let invite = HostInvite {
            helper_id: key.public(),
            host_id: SecretKey::generate().public(),
            guest_id: SecretKey::generate().public(),
            session: new_session_id(),
            addrs: vec!["9.8.7.6:4000".parse().unwrap()],
            relays: vec!["https://relay.example.com/".parse().unwrap()],
        };
        let bytes = encode_invite_gossip(&key, &invite);
        assert_eq!(bytes[4], INVITE_VERSION_V2);
        let parsed = decode_invite_gossip(&bytes, card::unix_now()).expect("invite");
        assert_eq!(parsed.helper_id, invite.helper_id);
        assert_eq!(parsed.host_id, invite.host_id);
        assert_eq!(parsed.guest_id, invite.guest_id);
        assert_eq!(parsed.session, invite.session);
        assert_eq!(parsed.addrs, invite.addrs);
        assert_eq!(parsed.relays, invite.relays);
    }

    #[test]
    fn helper_cards_keep_legacy_open_without_addrs() {
        let key = SecretKey::generate();
        // Build a pre-addr card: stop after load, then sign.
        let mut body = Vec::new();
        body.extend_from_slice(CARD_MAGIC);
        body.push(1);
        body.push(KIND_OPEN);
        body.extend_from_slice(key.public().as_bytes());
        body.extend_from_slice(&card::unix_now().to_be_bytes());
        body.extend_from_slice(&CARD_TTL_SECS.to_be_bytes());
        body.extend_from_slice(&DEFAULT_LIMIT_BPS.to_be_bytes());
        body.push(0);
        let signature = key.sign(&body);
        body.extend_from_slice(&signature.to_bytes());
        let parsed = decode_card(&body, card::unix_now()).unwrap();
        match parsed {
            HelperCard::Open { addrs, .. } => assert!(addrs.is_empty()),
            HelperCard::Bye { .. } => panic!("expected open"),
        }
    }

    #[test]
    fn helper_cards_v2_round_trip_with_relay_and_reject_tampering() {
        let key = SecretKey::generate();
        let relays = vec![
            "https://relay.example.com/".parse().unwrap(),
            "https://backup.example.net/".parse().unwrap(),
        ];
        let open = encode_open_with_relays(&key, DEFAULT_LIMIT_BPS, 3, &[], &relays);
        assert_eq!(open[4], CARD_VERSION_V2);
        let parsed = decode_card(&open, card::unix_now()).unwrap();
        match parsed {
            HelperCard::Open {
                addrs, relays: got, ..
            } => {
                assert!(addrs.is_empty());
                assert_eq!(got, relays);
            }
            HelperCard::Bye { .. } => panic!("expected open"),
        }
        let mut bad_body = open.clone();
        let signed_len = bad_body.len() - 64;
        bad_body[signed_len - 1] ^= 1;
        assert!(decode_card(&bad_body, card::unix_now()).is_none());
        let mut bad_signature = open;
        *bad_signature.last_mut().unwrap() ^= 1;
        assert!(decode_card(&bad_signature, card::unix_now()).is_none());
    }

    #[test]
    fn helper_cards_v1_decode_has_no_relays() {
        let key = SecretKey::generate();
        let open = encode_open(
            &key,
            DEFAULT_LIMIT_BPS,
            0,
            &["1.2.3.4:1234".parse().unwrap()],
        );
        assert_eq!(open[4], CARD_VERSION_V1);
        match decode_card(&open, card::unix_now()).unwrap() {
            HelperCard::Open { relays, .. } => assert!(relays.is_empty()),
            HelperCard::Bye { .. } => panic!("expected open"),
        }
    }

    #[test]
    fn helper_cards_reject_invalid_relay_urls() {
        let key = SecretKey::generate();
        let cases: [&[u8]; 4] = [
            b"http://relay.example.com/",
            b"not-a-url",
            b"https:///missing-host",
            &[0xff],
        ];
        for raw in cases {
            let mut body = Vec::new();
            body.extend_from_slice(CARD_MAGIC);
            body.push(CARD_VERSION_V2);
            body.push(KIND_OPEN);
            body.extend_from_slice(key.public().as_bytes());
            body.extend_from_slice(&card::unix_now().to_be_bytes());
            body.extend_from_slice(&CARD_TTL_SECS.to_be_bytes());
            body.extend_from_slice(&DEFAULT_LIMIT_BPS.to_be_bytes());
            body.push(0);
            encode_addrs(&mut body, &[]);
            body.push(1);
            let len = u16::try_from(raw.len()).unwrap();
            body.extend_from_slice(&len.to_be_bytes());
            body.extend_from_slice(raw);
            let signature = key.sign(&body);
            body.extend_from_slice(&signature.to_bytes());
            assert!(
                decode_card(&body, card::unix_now()).is_none(),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn invite_gossip_v1_still_decodes_without_relays() {
        let key = SecretKey::generate();
        let invite = HostInvite {
            helper_id: key.public(),
            host_id: SecretKey::generate().public(),
            guest_id: SecretKey::generate().public(),
            session: new_session_id(),
            addrs: vec!["9.8.7.6:4000".parse().unwrap()],
            relays: vec![],
        };
        let bytes = encode_invite_gossip(&key, &invite);
        assert_eq!(bytes[4], INVITE_VERSION_V1);
        let parsed = decode_invite_gossip(&bytes, card::unix_now()).unwrap();
        assert!(parsed.relays.is_empty());
        assert_eq!(parsed.addrs, invite.addrs);
    }

    #[test]
    fn rank_candidates_prefers_shared_relay_over_direct_only() {
        let shared: RelayUrl = "https://relay.example.com/".parse().unwrap();
        let unshared: RelayUrl = "https://other.example.net/".parse().unwrap();
        let direct_id = SecretKey::generate().public();
        let relay_id = SecretKey::generate().public();
        let unshared_id = SecretKey::generate().public();
        let expires = Instant::now() + Duration::from_secs(60);
        let offers = vec![
            (
                direct_id,
                LiveHelper {
                    limit_bps: DEFAULT_LIMIT_BPS,
                    load: 0,
                    addrs: vec!["1.2.3.4:1234".parse().unwrap()],
                    relays: vec![],
                    expires,
                },
            ),
            (
                relay_id,
                LiveHelper {
                    limit_bps: DEFAULT_LIMIT_BPS,
                    load: 0,
                    addrs: vec![],
                    relays: vec![shared.clone()],
                    expires,
                },
            ),
            (
                unshared_id,
                LiveHelper {
                    limit_bps: DEFAULT_LIMIT_BPS,
                    load: 0,
                    addrs: vec![],
                    relays: vec![unshared],
                    expires,
                },
            ),
        ];

        let ranked = rank_candidates(&[shared.clone()], offers);
        assert_eq!(ranked[0].id, relay_id);
        assert!(ranked[0].shared_relay);
        assert_eq!(ranked[0].relays, vec![shared]);
        assert_eq!(ranked[1].id, direct_id);
        assert!(!ranked[1].shared_relay);
        assert_eq!(ranked[2].id, unshared_id);
        assert!(ranked[2].relays.is_empty());
    }

    #[test]
    fn helper_endpoint_addr_includes_relay() {
        let id = SecretKey::generate().public();
        let socket: SocketAddr = "1.2.3.4:1234".parse().unwrap();
        let relay: RelayUrl = "https://relay.example.com/".parse().unwrap();
        let addr = helper_endpoint_addr(id, &[socket], &[relay.clone()]);
        assert_eq!(addr.id, id);
        assert_eq!(addr.ip_addrs().copied().collect::<Vec<_>>(), vec![socket]);
        assert_eq!(addr.relay_urls().cloned().collect::<Vec<_>>(), vec![relay]);
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(clamp_limit(100), MIN_LIMIT_BPS);
        assert_eq!(clamp_limit(DEFAULT_LIMIT_BPS), DEFAULT_LIMIT_BPS);
        assert_eq!(clamp_limit(u32::MAX), MAX_LIMIT_BPS);
    }

    #[derive(Debug, Default)]
    struct DenyPolicy;

    impl HelperPolicy for DenyPolicy {
        fn allows_offer(&self, _offer: &HelperOffer) -> bool {
            false
        }

        fn allows_session(
            &self,
            _guest: EndpointId,
            _target: EndpointId,
            _session: &[u8; 16],
        ) -> bool {
            false
        }
    }

    #[test]
    fn helper_policy_default_allows_untrusted_helpers() {
        let offer = HelperOffer {
            endpoint_id: SecretKey::generate().public().to_string(),
            limit_bps: DEFAULT_LIMIT_BPS,
            load: 0,
            rtt_ms: None,
            direct: false,
            relay: false,
            usable: true,
            note: String::new(),
        };
        let guest = SecretKey::generate().public();
        let target = SecretKey::generate().public();
        let session = new_session_id();

        let allow = DefaultHelperPolicy;
        assert!(allow.allows_offer(&offer));
        assert!(allow.allows_session(guest, target, &session));

        let deny = DenyPolicy;
        assert!(!deny.allows_offer(&offer));
        assert!(!deny.allows_session(guest, target, &session));
    }

    #[tokio::test]
    async fn helper_book_policy_filters_offers() {
        let helper_id = SecretKey::generate().public();
        let self_id = SecretKey::generate().public();
        let card = HelperCard::Open {
            endpoint_id: helper_id,
            limit_bps: DEFAULT_LIMIT_BPS,
            load: 1,
            timestamp: card::unix_now(),
            ttl: CARD_TTL_SECS,
            addrs: vec![],
            relays: vec![],
        };

        let permissive = HelperBook::new();
        permissive.ingest(card.clone()).await;
        let offers = permissive.list(self_id).await;
        assert_eq!(offers.len(), 1);
        assert!(permissive.allows_offer(offers[0].0, &offers[0].1));

        let restrictive = HelperBook::with_policy(Arc::new(DenyPolicy));
        restrictive.ingest(card).await;
        let offers = restrictive.list(self_id).await;
        assert_eq!(offers.len(), 1);
        assert!(!restrictive.allows_offer(offers[0].0, &offers[0].1));
    }

    #[test]
    fn global_ip_skips_lan_and_keeps_public() {
        assert!(!is_globally_reachable_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_globally_reachable_ip("192.168.1.2".parse().unwrap()));
        assert!(!is_globally_reachable_ip("127.0.0.1".parse().unwrap()));
        assert!(is_globally_reachable_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_globally_reachable_ip("fd12::1".parse().unwrap()));
        assert!(is_globally_reachable_ip(
            "2001:4860:4860::8888".parse().unwrap()
        ));
    }

    #[tokio::test]
    async fn ip_only_endpoint_binds_with_rustls_provider() {
        // Regression: presets::Empty bind fails with missing crypto provider.
        let ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .expect("Minimal + RelayMode::Disabled must bind");
        assert!(!ep.bound_sockets().is_empty());
        ep.close().await;
    }

    #[tokio::test]
    async fn ip_only_probe_reaches_local_helper() {
        use iroh::protocol::Router;

        let helper_key = SecretKey::generate();
        let helper_ep = Endpoint::builder(presets::Minimal)
            .secret_key(helper_key)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let (enabled, _) = watch::channel(true);
        let (invite_tx, _invite_rx) = mpsc::channel(1);
        let handler = HelperHandler {
            enabled: enabled.subscribe(),
            sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
            active: Arc::new(AtomicUsize::new(0)),
            bandwidth: Arc::new(Mutex::new(Arc::new(BandwidthLimit::new(u64::from(
                DEFAULT_LIMIT_BPS,
            ))))),
            policy: Arc::new(DefaultHelperPolicy),
            endpoint: helper_ep.clone(),
            rendezvous: RendezvousBook::default(),
            invite_tx,
            listen_port: Arc::new(AtomicU32::new(0)),
        };
        let router = Router::builder(helper_ep.clone())
            .accept(ALPN, handler)
            .spawn();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let port = helper_ep
            .bound_sockets()
            .into_iter()
            .find(|socket| socket.is_ipv4())
            .map(|socket| socket.port())
            .expect("ipv4 bind");
        let addrs = vec![SocketAddr::from(([127, 0, 0, 1], port))];
        let rtt = probe_ip_only(helper_ep.id(), &addrs)
            .await
            .expect("loopback ip_only probe must succeed");
        assert!(rtt < 5_000);

        router.shutdown().await.ok();
        helper_ep.close().await;
    }

    #[test]
    fn invite_session_claim_dedups() {
        let session = new_session_id();
        assert!(claim_invite_session(session));
        assert!(!claim_invite_session(session));
    }

    /// Regression: uplink EOF must not cancel the stall response copy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bridge_keeps_downlink_after_request_finishes() {
        use axum::Router;
        use axum::routing::get;
        use iroh::protocol::Router as IrohRouter;
        use tokio::io::AsyncReadExt;

        use crate::p2p::frame::{RequestFrame, encode_request};
        use crate::p2p::io::read_response_head;
        use crate::p2p::node::{ALPN as STALL_ALPN, RouteBook};
        use crate::p2p::protocol::{HostTarget, StallHandler};
        use crate::p2p::traffic::Traffic;
        use crate::security::BandwidthLimit;

        let host_ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let stall = StallHandler::new(Arc::new(Traffic::default()), Arc::new(RouteBook::default()));
        stall
            .set_host(HostTarget {
                router: Router::new().route(
                    "/",
                    get(|| async {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        "bridge-ok"
                    }),
                ),
                password: None,
            })
            .await;
        let host_router = IrohRouter::builder(host_ep.clone())
            .accept(STALL_ALPN, stall)
            .spawn();

        let helper_ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let host_port = host_ep
            .bound_sockets()
            .into_iter()
            .find(|socket| socket.is_ipv4())
            .unwrap()
            .port();
        let host_id = host_ep.id();
        let host_addr =
            EndpointAddr::new(host_id).with_ip_addr(SocketAddr::from(([127, 0, 0, 1], host_port)));
        let bandwidth = Arc::new(BandwidthLimit::new(u64::from(DEFAULT_LIMIT_BPS)));

        #[derive(Debug, Clone)]
        struct InstantBridge {
            endpoint: Endpoint,
            host_addr: EndpointAddr,
            bandwidth: Arc<BandwidthLimit>,
        }
        impl ProtocolHandler for InstantBridge {
            async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
                let (mut send, mut recv) = match connection.accept_bi().await {
                    Ok(pair) => pair,
                    Err(_) => return Ok(()),
                };
                let Ok(Hello::Guest { .. }) = read_hello(&mut recv).await else {
                    let _ = write_hello_status(&mut send, 1).await;
                    return Ok(());
                };
                let host = match self
                    .endpoint
                    .connect(self.host_addr.clone(), STALL_ALPN)
                    .await
                {
                    Ok(conn) => conn,
                    Err(_) => {
                        let _ = write_hello_status(&mut send, 3).await;
                        return Ok(());
                    }
                };
                if write_hello_status(&mut send, 0).await.is_err() {
                    return Ok(());
                }
                loop {
                    let guest_pair = match connection.accept_bi().await {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    let host_pair = match host.open_bi().await {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    let bandwidth = self.bandwidth.clone();
                    tokio::spawn(async move {
                        let _ = bridge(guest_pair, host_pair, bandwidth).await;
                    });
                }
                Ok(())
            }
        }

        let helper_router = IrohRouter::builder(helper_ep.clone())
            .accept(
                ALPN,
                InstantBridge {
                    endpoint: helper_ep.clone(),
                    host_addr,
                    bandwidth,
                },
            )
            .spawn();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let guest_ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let helper_port = helper_ep
            .bound_sockets()
            .into_iter()
            .find(|socket| socket.is_ipv4())
            .unwrap()
            .port();
        let dial = EndpointAddr::new(helper_ep.id())
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], helper_port)));
        let connection = guest_ep.connect(dial, ALPN).await.expect("dial helper");
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        write_hello_guest(&mut send, host_id, new_session_id())
            .await
            .unwrap();
        assert_eq!(read_hello_status(&mut recv).await.unwrap(), 0);

        let (mut req_send, mut req_recv) = connection.open_bi().await.unwrap();
        let frame = encode_request(&RequestFrame::get("/")).unwrap();
        req_send.write_all(&frame).await.unwrap();
        req_send.finish().unwrap();

        let head = tokio::time::timeout(Duration::from_secs(5), read_response_head(&mut req_recv))
            .await
            .expect("response head timed out")
            .expect("response head");
        assert_eq!(head.status, 200);
        let mut body = Vec::new();
        AsyncReadExt::read_to_end(&mut req_recv, &mut body)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("bridge-ok"),
            "{}",
            String::from_utf8_lossy(&body)
        );

        connection.close(0u8.into(), b"done");
        helper_router.shutdown().await.ok();
        host_router.shutdown().await.ok();
        guest_ep.close().await;
        helper_ep.close().await;
        host_ep.close().await;
    }
}
