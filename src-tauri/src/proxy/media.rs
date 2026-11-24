use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::stream::unfold;
use iroh::endpoint::Connection;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, Notify, watch};

use crate::p2p::{Method, RequestFrame, Traffic, encode_request, read_response_head};
use crate::security::MAX_MEDIA_BYTES;

use super::PROXY_CSP;

/// One full transfer per audio or video file, written to a temporary file as it
/// arrives. The browser's small range requests are answered from that file, so
/// playback can start and continue without holding the whole video in memory.
#[derive(Clone)]
pub struct MediaHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    slots: Mutex<Vec<Arc<MediaSlot>>>,
    connection: Connection,
    traffic: Arc<Traffic>,
}

static MEDIA_FILE_SEQ: AtomicU64 = AtomicU64::new(1);

struct MediaSlot {
    target: String,
    path: PathBuf,
    store: StdMutex<Option<std::fs::File>>,
    downloaded: AtomicU64,
    content_type: watch::Sender<Option<String>>,
    status: AtomicU16,
    total: AtomicU64,
    done: AtomicBool,
    failed: Mutex<Option<String>>,
    notify: Notify,
    cancel: watch::Sender<bool>,
}

impl Drop for MediaSlot {
    fn drop(&mut self) {
        if let Ok(file) = self.store.get_mut() {
            *file = None;
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn is_media_target(target: &str) -> bool {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    let Some(ext) = path.rsplit('.').next() else {
        return false;
    };
    if ext.len() > 4 || ext == path {
        return false;
    }
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "aiff"
            | "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "ogv" | "mpg" | "mpeg"
    )
}

#[derive(Debug)]
pub enum ByteRange {
    Closed { start: u64, end: u64 },
    Open { start: u64 },
    Suffix { len: u64 },
}

pub fn parse_byte_range(header: &str) -> Option<ByteRange> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        let len = end.parse::<u64>().ok()?;
        return (len > 0).then_some(ByteRange::Suffix { len });
    }
    let start = start.parse::<u64>().ok()?;
    if end.is_empty() {
        return Some(ByteRange::Open { start });
    }
    let last = end.parse::<u64>().ok()?;
    if last < start {
        return None;
    }
    Some(ByteRange::Closed {
        start,
        end: last.saturating_add(1),
    })
}

impl MediaHub {
    pub fn new(connection: Connection, traffic: Arc<Traffic>) -> Self {
        Self {
            inner: Arc::new(HubInner {
                slots: Mutex::new(Vec::new()),
                connection,
                traffic,
            }),
        }
    }

    pub async fn serve(&self, method: Method, target: String, range: Option<String>) -> Response {
        let slot = self.slot_for(&target).await;
        if let Err(err) = slot.wait_ready().await {
            return plain(StatusCode::BAD_GATEWAY, &err);
        }
        let status = StatusCode::from_u16(slot.status.load(Ordering::Acquire))
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let ctype = slot
            .content_type
            .borrow()
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        if status != StatusCode::OK {
            if let Err(err) = slot.wait_finished().await {
                return plain(StatusCode::BAD_GATEWAY, &err);
            }
            let body = slot.read_prefix(64 * 1024).await.unwrap_or_default();
            return media_response(status, &ctype, None, None, Body::from(body), method == Method::Head);
        }

        let total = slot.total.load(Ordering::Acquire);
        let parsed = range.as_deref().and_then(parse_byte_range);
        match parsed {
            Some(ByteRange::Suffix { len }) => {
                if let Err(err) = slot.wait_finished().await {
                    return plain(StatusCode::BAD_GATEWAY, &err);
                }
                let file_len = slot.downloaded();
                let start = file_len.saturating_sub(len);
                if file_len.saturating_sub(start) > 8 * 1024 * 1024 {
                    return stream_range(
                        slot,
                        ctype,
                        start,
                        Some(file_len),
                        Some(file_len),
                        true,
                        method == Method::Head,
                    );
                }
                match slot.read_span(start, file_len).await {
                    Ok(slice) => ranged(ctype, start, file_len, slice, method == Method::Head),
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Closed { start, end }) => {
                let end = if total > 0 { end.min(total) } else { end };
                if total > 0 && start >= total {
                    return unsatisfiable(total);
                }
                if end.saturating_sub(start) > 8 * 1024 * 1024 {
                    let file_len = (total > 0).then_some(total);
                    return stream_range(slot, ctype, start, file_len, Some(end), true, method == Method::Head);
                }
                match slot.take(start, end).await {
                    Ok(slice) => {
                        let file_len = if total > 0 { total } else { slot.downloaded() };
                        ranged(ctype, start, file_len, slice, method == Method::Head)
                    }
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Open { start }) => {
                if total > 0 && start >= total {
                    return unsatisfiable(total);
                }
                let file_len = (total > 0).then_some(total);
                stream_range(slot, ctype, start, file_len, file_len, true, method == Method::Head)
            }
            None => {
                let file_len = (total > 0).then_some(total);
                stream_range(slot, ctype, 0, file_len, file_len, false, method == Method::Head)
            }
        }
    }

    async fn slot_for(&self, target: &str) -> Arc<MediaSlot> {
        let mut slots = self.inner.slots.lock().await;
        if let Some(found) = slots.iter().find(|slot| slot.target == target) {
            return found.clone();
        }
        while slots.len() >= 2 {
            slots.remove(0).cancel();
        }
        let slot = MediaSlot::start(
            target.to_string(),
            self.inner.connection.clone(),
            self.inner.traffic.clone(),
        );
        slots.push(slot.clone());
        slot
    }
}

impl MediaSlot {
    fn start(target: String, connection: Connection, traffic: Arc<Traffic>) -> Arc<Self> {
        let (content_type, _) = watch::channel(None);
        let (cancel, cancel_rx) = watch::channel(false);
        let seq = MEDIA_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("popstall-{}-{seq}.media", std::process::id()));
        let slot = Arc::new(Self {
            target,
            path,
            store: StdMutex::new(None),
            downloaded: AtomicU64::new(0),
            content_type,
            status: AtomicU16::new(0),
            total: AtomicU64::new(0),
            done: AtomicBool::new(false),
            failed: Mutex::new(None),
            notify: Notify::new(),
            cancel,
        });
        let pumping = slot.clone();
        tokio::spawn(async move {
            if let Err(err) = pump(pumping.clone(), connection, traffic, cancel_rx).await {
                tracing::warn!(target = %pumping.target, %err, "影音下载中断");
                *pumping.failed.lock().await = Some(err.to_string());
            }
            pumping.done.store(true, Ordering::Release);
            pumping.notify.notify_waiters();
        });
        slot
    }

    fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    async fn wait_ready(&self) -> Result<(), String> {
        loop {
            if self.content_type.borrow().is_some() {
                return Ok(());
            }
            if let Some(err) = self.failure().await {
                return Err(err);
            }
            if self.done.load(Ordering::Acquire) {
                return Err(self.failure().await.unwrap_or_else(|| "没有读到影片".to_string()));
            }
            wait_notify(self).await;
        }
    }

    async fn wait_finished(&self) -> Result<(), String> {
        loop {
            if let Some(err) = self.failure().await {
                return Err(err);
            }
            if self.done.load(Ordering::Acquire) {
                return Ok(());
            }
            wait_notify(self).await;
        }
    }

    fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Acquire)
    }

    async fn take(&self, start: u64, end: u64) -> Result<Bytes, String> {
        loop {
            if let Some(err) = self.failure().await {
                return Err(err);
            }
            let len = self.downloaded();
            let done = self.done.load(Ordering::Acquire);
            let want = if done { end.min(len) } else { end };
            if done && start >= len {
                return Err("请求的位置超出了文件".to_string());
            }
            if len >= want && want > start {
                return self.read_span(start, want).await;
            }
            if done {
                return Err("请求的位置超出了文件".to_string());
            }
            wait_notify(self).await;
        }
    }

    async fn read_prefix(&self, max_len: u64) -> Result<Bytes, String> {
        let end = self.downloaded().min(max_len);
        self.read_span(0, end).await
    }

    async fn read_span(&self, start: u64, end: u64) -> Result<Bytes, String> {
        if end <= start {
            return Ok(Bytes::new());
        }
        let len = usize::try_from(end - start).map_err(|_| "这段影音太长，读不进内存".to_string())?;
        let mut file = self
            .store
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .as_ref()
            .ok_or_else(|| "缓存还没准备好".to_string())?
            .try_clone()
            .map_err(|err| err.to_string())?;
        tokio::task::spawn_blocking(move || {
            file.seek(SeekFrom::Start(start))?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf)?;
            Ok::<_, std::io::Error>(Bytes::from(buf))
        })
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
    }

    async fn failure(&self) -> Option<String> {
        self.failed.lock().await.clone()
    }
}

async fn pump(
    slot: Arc<MediaSlot>,
    connection: Connection,
    traffic: Arc<Traffic>,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("打开影音流失败: {err:?}"))?;
    let frame = encode_request(&RequestFrame {
        method: Method::Get,
        target: slot.target.clone(),
        range: None,
        content_type: None,
        body: Vec::new(),
    })?;
    let cache = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&slot.path)?;
    *slot.store.lock().unwrap_or_else(|err| err.into_inner()) = Some(cache);

    traffic.record_sent(frame.len() as u64);
    send.write_all(&frame).await?;
    send.finish()
        .map_err(|err| anyhow::anyhow!("结束影音请求失败: {err:?}"))?;
    let head = read_response_head(&mut recv).await?;
    if let Some(len) = head.content_length {
        if len > MAX_MEDIA_BYTES {
            anyhow::bail!("这个影音超过 32 GB");
        }
        slot.total.store(len, Ordering::Release);
    }
    slot.status.store(head.status, Ordering::Release);
    let _ = slot.content_type.send(Some(head.content_type));
    slot.notify.notify_waiters();
    let started = std::time::Instant::now();
    tracing::info!(
        target = %slot.target,
        status = head.status,
        bytes = head.content_length.unwrap_or(0),
        "开始下载影音"
    );

    let mut buf = vec![0u8; 64 * 1024];
    let mut next_report = 1024 * 1024;
    loop {
        let read = AsyncReadExt::read(&mut recv, &mut buf);
        tokio::pin!(read);
        let size = tokio::select! {
            _ = cancel.changed() => {
                if *cancel.borrow() {
                    anyhow::bail!("已换成另一段影音");
                }
                continue;
            }
            result = &mut read => result?,
        };
        if size == 0 {
            break;
        }
        traffic.record_received(size as u64);
        {
            let mut guard = slot.store.lock().unwrap_or_else(|err| err.into_inner());
            let file = guard.as_mut().ok_or_else(|| anyhow::anyhow!("缓存文件已关闭"))?;
            let received = slot.downloaded.load(Ordering::Acquire);
            if received + size as u64 > MAX_MEDIA_BYTES {
                anyhow::bail!("这个影音超过 32 GB");
            }
            file.seek(SeekFrom::End(0))?;
            file.write_all(&buf[..size])?;
            file.flush()?;
            slot.downloaded.fetch_add(size as u64, Ordering::Release);
        }
        let received = slot.downloaded();
        if received >= next_report {
            tracing::info!(target = %slot.target, received, "影音仍在下载");
            next_report += 1024 * 1024;
        }
        slot.notify.notify_waiters();
    }
    tracing::info!(
        target = %slot.target,
        received = slot.downloaded(),
        ms = started.elapsed().as_millis() as u64,
        "影音下载结束"
    );
    Ok(())
}

async fn wait_notify(slot: &MediaSlot) {
    let notified = slot.notify.notified();
    tokio::pin!(notified);
    tokio::select! {
        _ = notified => {}
        _ = tokio::time::sleep(Duration::from_millis(40)) => {}
    }
}

fn stream_range(
    slot: Arc<MediaSlot>,
    ctype: String,
    start: u64,
    total: Option<u64>,
    read_end: Option<u64>,
    partial: bool,
    head: bool,
) -> Response {
    let stop = read_end.or(total);
    let length = stop.and_then(|stop| stop.checked_sub(start));
    let content_range = if partial {
        Some(match stop {
            Some(stop) if stop > start => {
                let shown = total.map(|total| total.to_string()).unwrap_or_else(|| "*".to_string());
                format!("bytes {start}-{}/{shown}", stop - 1)
            }
            _ => format!("bytes {start}-/*"),
        })
    } else {
        None
    };
    if head {
        return media_response(
            if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK },
            &ctype,
            length,
            content_range,
            Body::empty(),
            true,
        );
    }
    let body = Body::from_stream(unfold(start, move |pos| {
        let slot = slot.clone();
        async move {
            loop {
                let available = stop.unwrap_or(u64::MAX).min(slot.downloaded());
                if pos < available {
                    let next = (pos + 64 * 1024).min(available);
                    match slot.read_span(pos, next).await {
                        Ok(chunk) => return Some((Ok(chunk), next)),
                        Err(err) => return Some((Err(std::io::Error::other(err)), pos)),
                    }
                }
                if let Some(err) = slot.failed.lock().await.clone() {
                    return Some((Err(std::io::Error::other(err)), pos));
                }
                if stop.is_some_and(|stop| pos >= stop) || slot.done.load(Ordering::Acquire) {
                    return None;
                }
                wait_notify(&slot).await;
            }
        }
    }));
    media_response(
        if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK },
        &ctype,
        length,
        content_range,
        body,
        false,
    )
}

fn ranged(ctype: String, start: u64, total: u64, slice: Bytes, head: bool) -> Response {
    if slice.is_empty() {
        return unsatisfiable(total);
    }
    let last = start + slice.len() as u64 - 1;
    let shown = if total > 0 { total.to_string() } else { "*".to_string() };
    let length = slice.len() as u64;
    media_response(
        StatusCode::PARTIAL_CONTENT,
        &ctype,
        Some(length),
        Some(format!("bytes {start}-{last}/{shown}")),
        if head { Body::empty() } else { Body::from(slice) },
        head,
    )
}

fn unsatisfiable(total: u64) -> Response {
    let mut response = plain(StatusCode::RANGE_NOT_SATISFIABLE, "请求的位置超出了文件");
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{total}")).unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
    );
    response
}

fn media_response(
    status: StatusCode,
    ctype: &str,
    length: Option<u64>,
    content_range: Option<String>,
    body: Body,
    _head: bool,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(ctype).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .header(header::CONTENT_SECURITY_POLICY, PROXY_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::REFERRER_POLICY, "no-referrer");
    if let Some(length) = length {
        builder = builder.header(header::CONTENT_LENGTH, length);
    }
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn plain(status: StatusCode, message: &str) -> Response {
    let mut response = (status, message.to_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PROXY_CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, is_media_target, parse_byte_range};

    #[test]
    fn recognizes_audio_and_video_names() {
        assert!(is_media_target("/room/night.MP4"));
        assert!(is_media_target("/room/song.mp3?x=1"));
        assert!(!is_media_target("/room/index.html"));
        assert!(!is_media_target("/room/photo.png"));
    }

    #[test]
    fn parses_closed_open_and_suffix_ranges() {
        match parse_byte_range("bytes=0-1023") {
            Some(ByteRange::Closed { start: 0, end: 1024 }) => {}
            other => panic!("closed range parsed as {other:?}"),
        }
        assert!(matches!(parse_byte_range("bytes=8-"), Some(ByteRange::Open { start: 8 })));
        assert!(matches!(parse_byte_range("bytes=-4"), Some(ByteRange::Suffix { len: 4 })));
        assert!(parse_byte_range("bytes=9-1").is_none());
    }
}
