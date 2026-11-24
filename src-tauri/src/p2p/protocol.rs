use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use tokio::sync::{RwLock, Semaphore};
use tower::ServiceExt;

use super::frame::{ResponseHead, decode_request, encode_response_head};
use super::io::read_limited;
use super::traffic::Traffic;
use crate::security::{
    BANDWIDTH_BYTES_PER_SEC, BandwidthLimit, MAX_MEDIA_BYTES, MAX_REQUEST_BODY, MAX_RESPONSE_BYTES,
    MAX_STREAMS_PER_VISITOR, MAX_TARGET_LEN, MAX_VISITORS,
};

#[derive(Clone)]
pub struct HostTarget {
    pub router: Router,
}

#[derive(Clone)]
pub struct StallHandler {
    visitors: Arc<Semaphore>,
    bandwidth: Arc<BandwidthLimit>,
    target: Arc<RwLock<Option<HostTarget>>>,
    traffic: Arc<Traffic>,
}

impl StallHandler {
    pub fn new(traffic: Arc<Traffic>) -> Self {
        Self {
            visitors: Arc::new(Semaphore::new(MAX_VISITORS)),
            bandwidth: Arc::new(BandwidthLimit::new(BANDWIDTH_BYTES_PER_SEC)),
            target: Arc::new(RwLock::new(None)),
            traffic,
        }
    }

    pub async fn set_host(&self, target: HostTarget) {
        *self.target.write().await = Some(target);
    }

    pub async fn clear_host(&self) {
        *self.target.write().await = None;
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
        super::node::watch_paths(connection.clone(), "host");
        let peer = connection.remote_id().to_string();
        let stream_slots = Arc::new(Semaphore::new(MAX_STREAMS_PER_VISITOR));
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::debug!(?err, "visitor connection closed");
                    break;
                }
            };
            let target = self.target.clone();
            let stream_slots = stream_slots.clone();
            let bandwidth = self.bandwidth.clone();
            let traffic = self.traffic.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                if stream_slots.try_acquire_owned().is_err() {
                    let _ = write_text(send, 503, "这个摊位的并发请求已满").await;
                    return;
                }
                if let Err(err) = handle_stream(target, bandwidth, traffic, peer, send, recv).await {
                    tracing::warn!(%err, "stall stream failed");
                }
            });
        }
        Ok(())
    }
}

async fn handle_stream(
    target: Arc<RwLock<Option<HostTarget>>>,
    bandwidth: Arc<BandwidthLimit>,
    traffic: Arc<Traffic>,
    peer: String,
    mut send: SendStream,
    mut recv: RecvStream,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let bytes = read_limited(&mut recv, 16 + MAX_TARGET_LEN + MAX_REQUEST_BODY).await?;
    traffic.record_received(bytes.len() as u64);
    let request = match decode_request(&bytes) {
        Ok(request) => request,
        Err(err) => return write_text(send, 400, &err.to_string()).await,
    };
    let Some(host) = target.read().await.clone() else {
        return write_text(send, 503, "摊位已打烊").await;
    };

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
    let head = encode_response_head(&ResponseHead {
        status,
        content_type,
        content_length,
        content_range,
        accept_ranges,
    })?;
    send.write_all(&head).await?;
    traffic.record_sent(head.len() as u64);

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
    send.finish()
        .map_err(|err| anyhow::anyhow!("关闭响应流失败: {err:?}"))?;
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

fn is_media_type(content_type: &str) -> bool {
    let mime = content_type.split(';').next().unwrap_or(content_type).trim();
    mime.starts_with("audio/") || mime.starts_with("video/")
}

async fn write_text(mut send: SendStream, status: u16, message: &str) -> anyhow::Result<()> {
    let head = encode_response_head(&ResponseHead::new(
        status,
        "text/plain; charset=utf-8",
    ))?;
    send.write_all(&head).await?;
    send.write_all(message.as_bytes()).await?;
    send.finish()
        .map_err(|err| anyhow::anyhow!("关闭错误响应失败: {err:?}"))?;
    Ok(())
}
