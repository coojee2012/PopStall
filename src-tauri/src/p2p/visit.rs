//! Callback visit: a dialable guest invites a NATed host to dial back and serve.
//!
//! Company → home already works when home has a public port. Home → company needs
//! the company to dial out; this module runs that rendezvous over a direct path.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, PublicKey};
use tokio::sync::{Mutex, oneshot};

use super::helper::{self, connect_ip_only};
use super::protocol::StallHandler;

/// Guest accepts the host's dial-back on this ALPN, then browses over it.
pub const CB_ALPN: &[u8] = b"popstall/visit-cb/1";
/// Guest signals the host (may traverse n0) asking for a dial-back.
pub const INVITE_ALPN: &[u8] = b"popstall/visit-invite/1";

const INVITE_DIAL: Duration = Duration::from_secs(12);
const CALLBACK_WAIT: Duration = Duration::from_secs(20);
const HOST_DIAL: Duration = Duration::from_secs(12);

const MAGIC: &[u8; 4] = b"PVC1";
const KIND_INVITE: u8 = 1;
const KIND_HOST: u8 = 2;

type SessionId = [u8; 16];

#[derive(Clone, Default)]
struct WaitBook {
    waiting: Arc<Mutex<HashMap<SessionId, oneshot::Sender<Connection>>>>,
}

impl WaitBook {
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

#[derive(Clone)]
pub struct VisitHub {
    endpoint: Endpoint,
    stall: StallHandler,
    book: WaitBook,
    listen_port: Arc<AtomicU32>,
}

impl VisitHub {
    pub fn new(endpoint: Endpoint, stall: StallHandler, listen_port: Option<u16>) -> Self {
        Self {
            endpoint,
            stall,
            book: WaitBook::default(),
            listen_port: Arc::new(AtomicU32::new(u32::from(listen_port.unwrap_or(0)))),
        }
    }

    pub fn set_listen_port(&self, port: Option<u16>) {
        self.listen_port
            .store(u32::from(port.unwrap_or(0)), Ordering::Relaxed);
    }

    pub fn callback_handler(&self) -> CallbackHandler {
        CallbackHandler {
            book: self.book.clone(),
        }
    }

    pub fn invite_handler(&self) -> InviteHandler {
        InviteHandler {
            stall: self.stall.clone(),
            endpoint: self.endpoint.clone(),
        }
    }

    fn my_addrs(&self) -> Vec<SocketAddr> {
        let port = match self.listen_port.load(Ordering::Relaxed) {
            0 => None,
            p => u16::try_from(p).ok(),
        };
        helper::public_dial_addrs(&self.endpoint, port)
    }

    /// Ask `host` to dial us back and serve their stall on the callback connection.
    pub async fn connect_via_callback(&self, host: EndpointId) -> anyhow::Result<Connection> {
        let addrs = self.my_addrs();
        if addrs.is_empty() {
            anyhow::bail!("本机没有可被回拨的公网地址");
        }
        let session = new_session();
        let rx = self.book.register(session).await;
        crate::netdebug::log(format!(
            "visit_callback_start host={host} session={session:02x?} addrs={addrs:?}"
        ));
        notify_host(
            self.endpoint.clone(),
            EndpointAddr::from(host),
            self.endpoint.id(),
            session,
            addrs.clone(),
        )
        .await?;
        match tokio::time::timeout(CALLBACK_WAIT, rx).await {
            Ok(Ok(connection)) => {
                crate::netdebug::log(format!(
                    "visit_callback_ok host={host} paths={}",
                    describe_paths(&connection)
                ));
                Ok(connection)
            }
            Ok(Err(_)) => {
                self.book.cancel(session).await;
                anyhow::bail!("摊主回拨被取消");
            }
            Err(_) => {
                self.book.cancel(session).await;
                crate::netdebug::log(format!("visit_callback_timeout host={host}"));
                anyhow::bail!("等待摊主回拨超时");
            }
        }
    }
}

#[derive(Clone)]
pub struct CallbackHandler {
    book: WaitBook,
}

impl std::fmt::Debug for CallbackHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackHandler").finish_non_exhaustive()
    }
}

impl ProtocolHandler for CallbackHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let session = match read_host_hello(&mut recv).await {
            Ok(session) => session,
            Err(err) => {
                tracing::debug!(%err, "visit callback hello rejected");
                let _ = write_status(&mut send, 1).await;
                return Ok(());
            }
        };
        let Some(tx) = self.book.take(session).await else {
            let _ = write_status(&mut send, 6).await;
            return Ok(());
        };
        let _ = write_status(&mut send, 0).await;
        if tx.send(connection).is_err() {
            tracing::debug!("visit callback waiter gone");
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct InviteHandler {
    stall: StallHandler,
    endpoint: Endpoint,
}

impl std::fmt::Debug for InviteHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisitInviteHandler").finish_non_exhaustive()
    }
}

impl ProtocolHandler for InviteHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if !self.stall.is_hosting().await {
            connection.close(0u8.into(), b"idle");
            return Ok(());
        }
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let invite = match read_invite(&mut recv).await {
            Ok(invite) => invite,
            Err(err) => {
                tracing::debug!(%err, "visit invite rejected");
                let _ = write_status(&mut send, 1).await;
                return Ok(());
            }
        };
        let _ = write_status(&mut send, 0).await;
        let stall = self.stall.clone();
        let endpoint = self.endpoint.clone();
        tokio::spawn(async move {
            if let Err(err) = answer_invite(endpoint, stall, invite).await {
                tracing::debug!(%err, "摊主回拨串门失败");
                crate::netdebug::log(format!("visit_host_callback_err err={err}"));
            }
        });
        // Wait for the guest to finish reading the ack before dropping.
        connection.closed().await;
        Ok(())
    }
}

struct Invite {
    guest_id: EndpointId,
    session: SessionId,
    addrs: Vec<SocketAddr>,
}

async fn notify_host(
    endpoint: Endpoint,
    host_addr: EndpointAddr,
    guest_id: EndpointId,
    session: SessionId,
    addrs: Vec<SocketAddr>,
) -> anyhow::Result<()> {
    let host = host_addr.id;
    crate::netdebug::log(format!(
        "visit_invite_dial host={host} guest={guest_id} addrs={addrs:?}"
    ));
    let connection = tokio::time::timeout(INVITE_DIAL, endpoint.connect(host_addr, INVITE_ALPN))
        .await
        .map_err(|_| anyhow::anyhow!("通知摊主超时（信令）"))?
        .map_err(|err| anyhow::anyhow!("通知摊主失败: {err:?}"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    write_invite(&mut send, guest_id, session, &addrs).await?;
    let status = read_status(&mut recv).await.unwrap_or(255);
    connection.close(0u8.into(), b"invite");
    if status != 0 {
        anyhow::bail!("摊主未接回拨邀请 ({status})");
    }
    crate::netdebug::log(format!("visit_invite_ack host={host} status={status}"));
    Ok(())
}

async fn answer_invite(
    endpoint: Endpoint,
    stall: StallHandler,
    invite: Invite,
) -> anyhow::Result<()> {
    if !stall.is_hosting().await {
        anyhow::bail!("摊位已打烊");
    }
    crate::netdebug::log(format!(
        "visit_host_dial guest={} session={:02x?} addrs={:?}",
        invite.guest_id, invite.session, invite.addrs
    ));
    let connection = if !invite.addrs.is_empty() {
        match connect_ip_only(invite.guest_id, &invite.addrs, CB_ALPN, HOST_DIAL).await {
            Ok(conn) => conn,
            Err(err) => {
                crate::netdebug::log(format!(
                    "visit_host_ip_only_fail guest={} err={err}; fallback_main",
                    invite.guest_id
                ));
                let mut addr = EndpointAddr::new(invite.guest_id);
                for socket in &invite.addrs {
                    addr = addr.with_ip_addr(*socket);
                }
                tokio::time::timeout(HOST_DIAL, endpoint.connect(addr, CB_ALPN))
                    .await
                    .map_err(|_| anyhow::anyhow!("回拨访客超时"))?
                    .map_err(|e| anyhow::anyhow!("{e:?}"))?
            }
        }
    } else {
        anyhow::bail!("邀请里没有回拨地址");
    };
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    write_host_hello(&mut send, invite.session).await?;
    let status = read_status(&mut recv).await?;
    if status != 0 {
        connection.close(0u8.into(), b"reject");
        anyhow::bail!("访客拒绝回拨 ({status})");
    }
    crate::netdebug::log(format!(
        "visit_host_ready guest={} paths={}",
        invite.guest_id,
        describe_paths(&connection)
    ));
    tracing::info!(guest = %invite.guest_id, "摊主已回拨，开始接待串门");
    stall.serve_helper_bridge(connection).await;
    Ok(())
}

fn new_session() -> SessionId {
    let bytes = iroh::SecretKey::generate().to_bytes();
    let mut session = [0u8; 16];
    session.copy_from_slice(&bytes[..16]);
    session
}

async fn write_invite(
    send: &mut SendStream,
    guest_id: EndpointId,
    session: SessionId,
    addrs: &[SocketAddr],
) -> anyhow::Result<()> {
    let mut frame = Vec::new();
    frame.extend_from_slice(MAGIC);
    frame.push(KIND_INVITE);
    frame.extend_from_slice(&session);
    frame.extend_from_slice(guest_id.as_bytes());
    encode_addrs(&mut frame, addrs);
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn read_invite(recv: &mut RecvStream) -> anyhow::Result<Invite> {
    let bytes = recv.read_to_end(256).await?;
    if bytes.len() < 4 + 1 + 16 + 32 + 1 || &bytes[..4] != MAGIC || bytes[4] != KIND_INVITE {
        anyhow::bail!("invite 无效");
    }
    let mut session = [0u8; 16];
    session.copy_from_slice(&bytes[5..21]);
    let guest_id = PublicKey::from_bytes(bytes[21..53].try_into()?)?;
    let mut cursor = &bytes[53..];
    let addrs = decode_addrs(&mut cursor).ok_or_else(|| anyhow::anyhow!("地址无效"))?;
    if !cursor.is_empty() {
        anyhow::bail!("invite 多余字节");
    }
    Ok(Invite {
        guest_id,
        session,
        addrs,
    })
}

async fn write_host_hello(send: &mut SendStream, session: SessionId) -> anyhow::Result<()> {
    let mut frame = Vec::with_capacity(4 + 1 + 16);
    frame.extend_from_slice(MAGIC);
    frame.push(KIND_HOST);
    frame.extend_from_slice(&session);
    send.write_all(&frame).await?;
    send.finish()?;
    Ok(())
}

async fn read_host_hello(recv: &mut RecvStream) -> anyhow::Result<SessionId> {
    let bytes = recv.read_to_end(4 + 1 + 16).await?;
    if bytes.len() != 4 + 1 + 16 || &bytes[..4] != MAGIC || bytes[4] != KIND_HOST {
        anyhow::bail!("host hello 无效");
    }
    let mut session = [0u8; 16];
    session.copy_from_slice(&bytes[5..21]);
    Ok(session)
}

async fn write_status(send: &mut SendStream, status: u8) -> anyhow::Result<()> {
    send.write_all(&[status]).await?;
    send.finish()?;
    Ok(())
}

async fn read_status(recv: &mut RecvStream) -> anyhow::Result<u8> {
    let bytes = recv.read_to_end(8).await?;
    bytes
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("没有回应"))
}

fn encode_addrs(body: &mut Vec<u8>, addrs: &[SocketAddr]) {
    let n = addrs.len().min(4) as u8;
    body.push(n);
    for socket in addrs.iter().take(4) {
        match socket.ip() {
            std::net::IpAddr::V4(ip) => {
                body.push(4);
                body.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                body.push(6);
                body.extend_from_slice(&ip.octets());
            }
        }
        body.extend_from_slice(&socket.port().to_be_bytes());
    }
}

fn decode_addrs(cursor: &mut &[u8]) -> Option<Vec<SocketAddr>> {
    let n = *cursor.first()? as usize;
    *cursor = &cursor[1..];
    if n > 4 {
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
                std::net::IpAddr::V4(octets.into())
            }
            6 => {
                let octets: [u8; 16] = cursor.get(..16)?.try_into().ok()?;
                *cursor = &cursor[16..];
                std::net::IpAddr::V6(octets.into())
            }
            _ => return None,
        };
        let port = u16::from_be_bytes(cursor.get(..2)?.try_into().ok()?);
        *cursor = &cursor[2..];
        addrs.push(SocketAddr::new(ip, port));
    }
    Some(addrs)
}

fn describe_paths(connection: &Connection) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use iroh::SecretKey;
    use iroh::endpoint::{RelayMode, presets};
    use iroh::protocol::Router;

    use crate::p2p::node::RouteBook;
    use crate::p2p::protocol::{HostTarget, StallHandler};
    use crate::p2p::traffic::Traffic;

    #[test]
    fn invite_addrs_round_trip() {
        let mut body = Vec::new();
        let addrs = vec![
            "1.2.3.4:35175".parse().unwrap(),
            "[2001:db8::1]:35175".parse().unwrap(),
        ];
        encode_addrs(&mut body, &addrs);
        let mut cursor = body.as_slice();
        let got = decode_addrs(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        assert_eq!(got, addrs);
    }

    #[tokio::test]
    async fn callback_visit_loopback() {
        let guest_ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let host_ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();

        let stall = StallHandler::new(Arc::new(Traffic::default()), Arc::new(RouteBook::default()));
        stall
            .set_host(HostTarget {
                router: axum::Router::new(),
                password: None,
            })
            .await;

        let guest_port = guest_ep
            .bound_sockets()
            .into_iter()
            .find(|socket| socket.is_ipv4())
            .unwrap()
            .port();
        let book = WaitBook::default();
        let guest_router = Router::builder(guest_ep.clone())
            .accept(CB_ALPN, CallbackHandler { book: book.clone() })
            .spawn();
        let host_router = Router::builder(host_ep.clone())
            .accept(
                INVITE_ALPN,
                InviteHandler {
                    stall: stall.clone(),
                    endpoint: host_ep.clone(),
                },
            )
            .spawn();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let host_port = host_ep
            .bound_sockets()
            .into_iter()
            .find(|socket| socket.is_ipv4())
            .unwrap()
            .port();
        let session = new_session();
        let rx = book.register(session).await;
        let guest_addrs = vec![SocketAddr::from(([127, 0, 0, 1], guest_port))];
        let host_addr = EndpointAddr::new(host_ep.id())
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], host_port)));
        notify_host(
            guest_ep.clone(),
            host_addr,
            guest_ep.id(),
            session,
            guest_addrs,
        )
        .await
        .expect("invite");

        let connection = tokio::time::timeout(Duration::from_secs(8), rx)
            .await
            .expect("wait")
            .expect("callback conn");
        // Host may dial via the shared ip_only endpoint (ephemeral id); the
        // important part is that a callback connection arrived.
        assert!(!connection.remote_id().to_string().is_empty());

        connection.close(0u8.into(), b"done");
        guest_router.shutdown().await.ok();
        host_router.shutdown().await.ok();
        guest_ep.close().await;
        host_ep.close().await;
    }
}
