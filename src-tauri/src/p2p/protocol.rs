use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use iroh::SecretKey;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{RwLock, Semaphore};
use tower::ServiceExt;

use super::frame::{
    Method, ResponseHead, ResponseHeadProof, decode_request, encode_response_head,
    encode_response_head_proof,
};
use super::integrity::IntegrityIndex;
use super::io::read_limited;
use super::node::RouteBook;
use super::secure::{
    HandshakeIdentity, PathKind, PathTrust, SealedStream, helper_handshake_accept,
    helper_handshake_accept_peer,
};
use super::traffic::Traffic;
use crate::security::{
    BANDWIDTH_BYTES_PER_SEC, BandwidthLimit, MAX_MEDIA_BYTES, MAX_REQUEST_BODY, MAX_RESPONSE_BYTES,
    MAX_STREAMS_PER_VISITOR, MAX_TARGET_LEN, MAX_VISITORS,
};

#[derive(Clone)]
pub struct HostTarget {
    pub router: Router,
    pub password: Option<String>,
}

#[derive(Clone)]
pub struct StallHandler {
    visitors: Arc<Semaphore>,
    bandwidth: Arc<BandwidthLimit>,
    target: Arc<RwLock<Option<HostTarget>>>,
    traffic: Arc<Traffic>,
    routes: Arc<RouteBook>,
    host_identity: Arc<RwLock<Option<SecretKey>>>,
    integrity: Arc<RwLock<Option<Arc<IntegrityIndex>>>>,
}

impl StallHandler {
    pub fn new(traffic: Arc<Traffic>, routes: Arc<RouteBook>) -> Self {
        Self {
            visitors: Arc::new(Semaphore::new(MAX_VISITORS)),
            bandwidth: Arc::new(BandwidthLimit::new(BANDWIDTH_BYTES_PER_SEC)),
            target: Arc::new(RwLock::new(None)),
            traffic,
            routes,
            host_identity: Arc::new(RwLock::new(None)),
            integrity: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn set_host(&self, target: HostTarget) {
        *self.target.write().await = Some(target);
    }

    pub async fn set_host_identity(&self, secret: SecretKey) {
        *self.host_identity.write().await = Some(secret);
    }

    pub async fn set_integrity(&self, integrity: Option<Arc<IntegrityIndex>>) {
        *self.integrity.write().await = integrity;
    }

    pub async fn clear_host(&self) {
        *self.target.write().await = None;
        *self.integrity.write().await = None;
    }

    pub async fn is_hosting(&self) -> bool {
        self.target.read().await.is_some()
    }

    /// Serve stall requests arriving over a helper rendezvous bridge.
    pub async fn serve_helper_bridge(&self, connection: Connection) {
        let Some(host_secret) = self.host_identity.read().await.clone() else {
            connection.close(0u8.into(), b"no host identity");
            return;
        };
        let helper = connection.remote_id();
        let peer = helper.to_string();
        let stream_slots = Arc::new(Semaphore::new(MAX_STREAMS_PER_VISITOR));
        loop {
            let (mut send, mut recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let target = self.target.clone();
            let integrity = self.integrity.read().await.clone();
            let stream_slots = stream_slots.clone();
            let bandwidth = self.bandwidth.clone();
            let traffic = self.traffic.clone();
            let peer = peer.clone();
            let host_secret = host_secret.clone();
            tokio::spawn(async move {
                if stream_slots.try_acquire_owned().is_err() {
                    let _ = send.finish();
                    return;
                }
                let identity = HandshakeIdentity::new(host_secret);
                let (_trust, keys) =
                    match helper_handshake_accept_peer(&mut recv, &mut send, helper, &identity)
                        .await
                    {
                        Ok(result) => result,
                        Err(err) => {
                            tracing::debug!(%err, "helper-bridge handshake failed");
                            let _ = send.finish();
                            return;
                        }
                    };
                let sealed = SealedStream::new(recv, send, keys);
                let (mut sealed_recv, mut sealed_send) = tokio::io::split(sealed);
                if let Err(err) = handle_stream(
                    target,
                    bandwidth,
                    traffic,
                    integrity,
                    peer,
                    &mut sealed_send,
                    &mut sealed_recv,
                )
                .await
                {
                    tracing::debug!(%err, "helper-bridge stream ended");
                }
            });
        }
    }

    /// Serve helper-bridge streams authenticated and sealed end to end.
    pub async fn serve_helper_bridge_sealed(&self, connection: Connection, trust: PathTrust) {
        if trust.kind() != PathKind::HelperBridge || trust.helper() != Some(&connection.remote_id())
        {
            connection.close(0u8.into(), b"bad helper trust");
            return;
        }
        let Some(host_secret) = self.host_identity.read().await.clone() else {
            connection.close(0u8.into(), b"no host identity");
            return;
        };
        if host_secret.public() != *trust.host() {
            connection.close(0u8.into(), b"bad host identity");
            return;
        }

        let peer = connection.remote_id().to_string();
        let stream_slots = Arc::new(Semaphore::new(MAX_STREAMS_PER_VISITOR));
        loop {
            let (mut send, recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let target = self.target.clone();
            let integrity = self.integrity.read().await.clone();
            let stream_slots = stream_slots.clone();
            let bandwidth = self.bandwidth.clone();
            let traffic = self.traffic.clone();
            let peer = peer.clone();
            let trust = trust.clone();
            let identity = HandshakeIdentity::new(host_secret.clone());
            tokio::spawn(async move {
                if stream_slots.try_acquire_owned().is_err() {
                    let _ = send.finish();
                    return;
                }
                if let Err(err) = handle_sealed_stream(
                    target, bandwidth, traffic, integrity, peer, trust, identity, send, recv,
                )
                .await
                {
                    tracing::debug!(%err, "sealed helper-bridge stream ended");
                }
            });
        }
    }
}

impl std::fmt::Debug for StallHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StallHandler")
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for StallHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Ok(permit) = self.visitors.clone().try_acquire_owned() else {
            connection.close(0u8.into(), b"busy");
            tracing::info!("rejected a visitor because the stall is full");
            return Ok(());
        };
        let _permit = permit;
        super::node::watch_paths(connection.clone(), "host", self.routes.clone());
        let peer = connection.remote_id().to_string();
        let stream_slots = Arc::new(Semaphore::new(MAX_STREAMS_PER_VISITOR));
        loop {
            let (mut send, mut recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::debug!(?err, "visitor connection closed");
                    break;
                }
            };
            let target = self.target.clone();
            let integrity = self.integrity.read().await.clone();
            let stream_slots = stream_slots.clone();
            let bandwidth = self.bandwidth.clone();
            let traffic = self.traffic.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                if stream_slots.try_acquire_owned().is_err() {
                    let _ = write_text(&mut send, 503, "这个摊位的并发请求已满").await;
                    return;
                }
                if let Err(err) = handle_stream(
                    target, bandwidth, traffic, integrity, peer, &mut send, &mut recv,
                )
                .await
                {
                    tracing::warn!(%err, "stall stream failed");
                }
            });
        }
        Ok(())
    }
}

async fn handle_stream<R, W>(
    target: Arc<RwLock<Option<HostTarget>>>,
    bandwidth: Arc<BandwidthLimit>,
    traffic: Arc<Traffic>,
    integrity: Option<Arc<IntegrityIndex>>,
    peer: String,
    send: &mut W,
    recv: &mut R,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let started = std::time::Instant::now();
    let bytes = read_limited(recv, 16 + MAX_TARGET_LEN + MAX_REQUEST_BODY).await?;
    traffic.record_received(bytes.len() as u64);
    let request = match decode_request(&bytes) {
        Ok(request) => request,
        Err(err) => return write_text(send, 400, &err.to_string()).await,
    };
    let Some(host) = target.read().await.clone() else {
        return write_text(send, 503, "摊位已打烊").await;
    };
    if let Err(message) = gate(&host.password, &request.pass) {
        return write_text(send, 401, message).await;
    }

    let method = request.method;
    let target_log = request.target.clone();
    let mut builder = Request::builder()
        .method(method.to_http())
        .uri(request.target);
    if let Some(range) = request.range.as_deref() {
        builder = builder.header(header::RANGE, range);
    }
    if let Some(content_type) = request.content_type.as_deref() {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let http_request = builder.body(Body::from(request.body))?;
    let response = host
        .router
        .clone()
        .oneshot(http_request)
        .await
        .map_err(|err| anyhow::anyhow!("站点处理失败: {err}"))?;

    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let ceiling = if is_media_type(&content_type) {
        MAX_MEDIA_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    if content_length.is_some_and(|length| length > ceiling) {
        return write_text(send, 413, "响应超过单文件上限").await;
    }
    let media = is_media_type(&content_type);
    let content_range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let accept_ranges = headers
        .get(header::ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("bytes"));
    let head = ResponseHead {
        status,
        content_type,
        content_length,
        content_range: content_range.clone(),
        accept_ranges,
    };
    let proof = if method == Method::Get && (status == 200 || status == 206) {
        if let Some(integrity) = integrity.as_deref() {
            let digest = integrity.digest(&target_log).await.ok();
            match (status, digest) {
                (200, Some(digest)) => Some(ResponseHeadProof::new(head.clone(), digest)),
                (206, Some(digest)) => {
                    let range = content_range.as_deref().and_then(|value| {
                        let spec = value.strip_prefix("bytes ")?;
                        let (span, total) = spec.split_once('/')?;
                        let (start, end) = span.split_once('-')?;
                        let start = start.parse::<u64>().ok()?;
                        let end = end.parse::<u64>().ok()?;
                        let total = total.parse::<u64>().ok()?;
                        let exclusive = end.checked_add(1)?;
                        if start >= exclusive || exclusive > total {
                            return None;
                        }
                        Some((start, exclusive))
                    });
                    match range {
                        Some((start, end)) => {
                            let tree = integrity.merkle_tree(&target_log).await.ok();
                            let proof = integrity.merkle_proof(&target_log, start, end).await.ok();
                            match (tree, proof) {
                                (Some(tree), Some(proof)) => Some(
                                    ResponseHeadProof::new(head.clone(), digest)
                                        .with_merkle(tree.root(), Some(proof)),
                                ),
                                _ => None,
                            }
                        }
                        None => None,
                    }
                }
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let encoded = match proof {
        Some(proof) => match encode_response_head_proof(&proof) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::debug!(%err, "PSR3 编码失败，回退 PSR2");
                encode_response_head(&head)?
            }
        },
        None => encode_response_head(&head)?,
    };
    send.write_all(&encoded).await?;
    traffic.record_sent(encoded.len() as u64);

    let mut body = response.into_body();
    let mut sent = 0u64;
    let mut next_report = 1024 * 1024;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| anyhow::anyhow!("读取站点响应失败: {err}"))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        sent += data.len() as u64;
        if sent > ceiling {
            anyhow::bail!("响应超过单文件上限");
        }
        bandwidth.consume(data.len() as u64).await;
        send.write_all(&data).await?;
        traffic.record_sent(data.len() as u64);
        if media && sent >= next_report {
            tracing::info!(%peer, target = %target_log, sent, "影音仍在发送");
            next_report += 1024 * 1024;
        }
    }
    send.shutdown()
        .await
        .map_err(|err| anyhow::anyhow!("关闭响应流失败: {err}"))?;
    if media || sent >= 32 * 1024 {
        tracing::info!(
            %peer,
            method = %method.to_http(),
            target = %target_log,
            status,
            bytes = sent,
            ms = started.elapsed().as_millis() as u64,
            "发送完成"
        );
    } else {
        tracing::debug!(
            %peer,
            method = %method.to_http(),
            target = %target_log,
            status,
            bytes = sent,
            "发送完成"
        );
    }
    Ok(())
}

async fn handle_sealed_stream<R, W>(
    target: Arc<RwLock<Option<HostTarget>>>,
    bandwidth: Arc<BandwidthLimit>,
    traffic: Arc<Traffic>,
    integrity: Option<Arc<IntegrityIndex>>,
    peer: String,
    trust: PathTrust,
    identity: HandshakeIdentity,
    mut send: W,
    mut recv: R,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let keys = helper_handshake_accept(&mut recv, &mut send, &trust, &identity).await?;
    let sealed = SealedStream::new(recv, send, keys);
    let (mut sealed_recv, mut sealed_send) = tokio::io::split(sealed);
    handle_stream(
        target,
        bandwidth,
        traffic,
        integrity,
        peer,
        &mut sealed_send,
        &mut sealed_recv,
    )
    .await
}

fn gate(expected: &Option<String>, given: &str) -> Result<(), &'static str> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let given = given.trim();
    if given.is_empty() {
        return Err("这间摊要密码");
    }
    if !same_secret(expected.as_bytes(), given.as_bytes()) {
        return Err("密码不对");
    }
    Ok(())
}

fn same_secret(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let width = left.len().max(right.len());
    for index in 0..width {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn is_media_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim();
    mime.starts_with("audio/") || mime.starts_with("video/")
}

async fn write_text<W>(send: &mut W, status: u16, message: &str) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let head = encode_response_head(&ResponseHead::new(status, "text/plain; charset=utf-8"))?;
    send.write_all(&head).await?;
    send.write_all(message.as_bytes()).await?;
    send.shutdown()
        .await
        .map_err(|err| anyhow::anyhow!("关闭错误响应失败: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use axum::routing::get;
    use iroh::SecretKey;
    use tokio::io::{AsyncWrite, AsyncWriteExt, duplex, split};

    use super::*;
    use crate::p2p::frame::{MAGIC_REQUEST, MAGIC_RESPONSE, RequestFrame, encode_request};
    use crate::p2p::io::read_response_head;
    use crate::p2p::secure::{SessionId, helper_handshake_request};

    struct RecordingWriter<W> {
        inner: W,
        seen: Arc<Mutex<Vec<u8>>>,
    }

    impl<W> RecordingWriter<W> {
        fn new(inner: W, seen: Arc<Mutex<Vec<u8>>>) -> Self {
            Self { inner, seen }
        }
    }

    impl<W> AsyncWrite for RecordingWriter<W>
    where
        W: AsyncWrite + Unpin,
    {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let this = self.get_mut();
            match Pin::new(&mut this.inner).poll_write(cx, buf) {
                Poll::Ready(Ok(written)) => {
                    this.seen
                        .lock()
                        .expect("recorder lock poisoned")
                        .extend_from_slice(&buf[..written]);
                    Poll::Ready(Ok(written))
                }
                other => other,
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn target() -> Arc<RwLock<Option<HostTarget>>> {
        Arc::new(RwLock::new(Some(HostTarget {
            router: Router::new().route(
                "/secret-target-never-visible-1234567890",
                get(|| async { "secret-body-never-visible-1234567890" }),
            ),
            password: None,
        })))
    }

    #[tokio::test]
    async fn sealed_helper_stream_hides_request_and_response_from_helper() {
        let guest_secret = SecretKey::generate();
        let host_secret = SecretKey::generate();
        let helper_secret = SecretKey::generate();
        let trust = PathTrust::HelperBridge {
            session: SessionId::generate(),
            guest: guest_secret.public(),
            host: host_secret.public(),
            helper: helper_secret.public(),
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (guest_io, host_io) = duplex(64 * 1024);
        let (guest_recv, guest_send) = split(guest_io);
        let (host_recv, host_send) = split(host_io);
        let guest_send = RecordingWriter::new(guest_send, seen.clone());
        let host_send = RecordingWriter::new(host_send, seen.clone());
        let bandwidth = Arc::new(BandwidthLimit::new(BANDWIDTH_BYTES_PER_SEC));
        let traffic = Arc::new(Traffic::default());

        let host_task = tokio::spawn({
            let trust = trust.clone();
            async move {
                handle_sealed_stream(
                    target(),
                    bandwidth,
                    traffic,
                    None,
                    "helper".to_string(),
                    trust,
                    HandshakeIdentity::new(host_secret),
                    host_send,
                    host_recv,
                )
                .await
            }
        });

        let mut guest_recv = guest_recv;
        let mut guest_send = guest_send;
        let guest_identity = HandshakeIdentity::new(guest_secret);
        let keys =
            helper_handshake_request(&mut guest_recv, &mut guest_send, &trust, &guest_identity)
                .await
                .expect("guest handshake should succeed");
        let mut sealed = SealedStream::new(guest_recv, guest_send, keys);
        let request = encode_request(&RequestFrame::get(
            "/secret-target-never-visible-1234567890",
        ))
        .expect("request encoding should succeed");
        sealed
            .write_all(&request)
            .await
            .expect("sealed request write should succeed");
        sealed
            .shutdown()
            .await
            .expect("sealed request close should succeed");
        let head = read_response_head(&mut sealed)
            .await
            .expect("response head should decode");
        assert_eq!(head.status, 200);
        let body = read_limited(&mut sealed, 1024)
            .await
            .expect("response body should decode");
        assert_eq!(body, b"secret-body-never-visible-1234567890");
        host_task
            .await
            .expect("host task should join")
            .expect("host stream should succeed");

        let seen = seen.lock().expect("recorder lock poisoned").clone();
        assert!(contains(&seen, b"PSH1"), "handshake should be visible");
        assert!(!contains(&seen, MAGIC_REQUEST), "request frame leaked");
        assert!(!contains(&seen, MAGIC_RESPONSE), "response head leaked");
        assert!(
            !contains(&seen, b"secret-target-never-visible"),
            "target leaked"
        );
        assert!(
            !contains(&seen, b"secret-body-never-visible"),
            "body leaked"
        );
    }

    #[tokio::test]
    async fn plain_helper_stream_keeps_existing_wire_behavior() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (guest_io, host_io) = duplex(64 * 1024);
        let (mut guest_recv, guest_send) = split(guest_io);
        let (mut host_recv, host_send) = split(host_io);
        let mut guest_send = RecordingWriter::new(guest_send, seen.clone());
        let mut host_send = RecordingWriter::new(host_send, seen.clone());
        let bandwidth = Arc::new(BandwidthLimit::new(BANDWIDTH_BYTES_PER_SEC));
        let traffic = Arc::new(Traffic::default());

        let host_task = tokio::spawn(async move {
            handle_stream(
                target(),
                bandwidth,
                traffic,
                None,
                "helper".to_string(),
                &mut host_send,
                &mut host_recv,
            )
            .await
        });

        let request = encode_request(&RequestFrame::get(
            "/secret-target-never-visible-1234567890",
        ))
        .expect("request encoding should succeed");
        guest_send
            .write_all(&request)
            .await
            .expect("plain request write should succeed");
        guest_send
            .shutdown()
            .await
            .expect("plain request close should succeed");
        let head = read_response_head(&mut guest_recv)
            .await
            .expect("response head should decode");
        assert_eq!(head.status, 200);
        let body = read_limited(&mut guest_recv, 1024)
            .await
            .expect("response body should decode");
        assert_eq!(body, b"secret-body-never-visible-1234567890");
        host_task
            .await
            .expect("host task should join")
            .expect("host stream should succeed");

        let seen = seen.lock().expect("recorder lock poisoned").clone();
        assert!(
            contains(&seen, MAGIC_REQUEST),
            "request frame should be plaintext"
        );
        assert!(
            contains(&seen, MAGIC_RESPONSE),
            "response head should be plaintext"
        );
        assert!(
            contains(&seen, b"secret-target-never-visible"),
            "target should be plaintext"
        );
        assert!(
            contains(&seen, b"secret-body-never-visible"),
            "body should be plaintext"
        );
    }
}
