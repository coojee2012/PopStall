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

/// One P2P transfer per audio/video file, appended to a temp file while the
/// browser plays from whatever is already on disk.
///
/// Responses are short byte slices (with honest Content-Length). The player
/// issues the next Range as soon as it needs more — we never advertise a long
/// progressive body that WebViews wait on forever, and we never block start
/// until the whole movie finishes downloading.
#[derive(Clone)]
pub struct MediaHub {
    inner: Arc<HubInner>,
}

/// Bytes we want on disk past `start` before answering the first slice.
const PRIMING: u64 = 256 * 1024;
/// Max bytes returned in one HTTP response (keeps waits short; player re-Ranges).
const SLICE: u64 = 1024 * 1024;

struct HubInner {
    slots: Mutex<Vec<Arc<MediaSlot>>>,
    connection: Connection,
    traffic: Arc<Traffic>,
    password: String,
}

static MEDIA_FILE_SEQ: AtomicU64 = AtomicU64::new(1);

struct MediaSlot {
    /// Normalized path used for cache lookup (percent-decoded).
    key: String,
    /// Wire target as the browser requested (keep percent-encoding for http::Uri).
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

fn media_extension(target: &str) -> Option<String> {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .unwrap_or(std::borrow::Cow::Borrowed(path));
    let ext = decoded.rsplit('.').next()?;
    if ext.len() > 4 || ext == decoded.as_ref() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

pub fn is_media_target(target: &str) -> bool {
    matches!(
        media_extension(target).as_deref(),
        Some(
            "mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "aiff"
                | "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "ogv" | "mpg" | "mpeg"
        )
    )
}

pub fn is_audio_target(target: &str) -> bool {
    matches!(
        media_extension(target).as_deref(),
        Some("mp3" | "wav" | "flac" | "ogg" | "opus" | "m4a" | "aac" | "aiff")
    )
}

/// Stable slot key so `/media/a%20b.mp3` and decoded forms share one cache.
fn media_slot_key(target: &str) -> String {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| path.to_string())
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
    pub fn new(connection: Connection, traffic: Arc<Traffic>, password: String) -> Self {
        Self {
            inner: Arc::new(HubInner {
                slots: Mutex::new(Vec::new()),
                connection,
                traffic,
                password,
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
        let reported = slot
            .content_type
            .borrow()
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let ctype = media_content_type(&target, reported);
        if status != StatusCode::OK {
            if let Err(err) = slot.wait_finished().await {
                return plain(StatusCode::BAD_GATEWAY, &err);
            }
            let body = slot.read_prefix(64 * 1024).await.unwrap_or_default();
            return media_response(status, &ctype, None, None, Body::from(body), method == Method::Head);
        }

        // Songs are small on the wire here; WebView2 is unreliable with progressive
        // audio bodies. Wait for the whole file, then answer with an honest length.
        if is_audio_target(&target) {
            return self.serve_audio_complete(slot, ctype, method, range).await;
        }

        let total = slot.total.load(Ordering::Acquire);
        let head = method == Method::Head;
        let parsed = range.as_deref().and_then(parse_byte_range);

        // HEAD: advertise full size + Accept-Ranges so the player plans seeks
        // without us shipping any body.
        if head {
            let length = if total > 0 { Some(total) } else { None };
            return media_response(StatusCode::OK, &ctype, length, None, Body::empty(), true);
        }

        match parsed {
            Some(ByteRange::Suffix { len }) => {
                // Tail probes (duration / moov-at-end) need the finished file.
                if let Err(err) = slot.wait_finished().await {
                    return plain(StatusCode::BAD_GATEWAY, &err);
                }
                let file_len = slot.downloaded();
                if total > 0 && file_len < total {
                    return plain(
                        StatusCode::BAD_GATEWAY,
                        &format!("影音只下到 {file_len}/{total} 字节，连接中断了"),
                    );
                }
                let start = file_len.saturating_sub(len.min(SLICE));
                match slot.read_span(start, file_len).await {
                    Ok(slice) => ranged(ctype, start, file_len, slice, false),
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Closed { start, end }) => {
                let end = if total > 0 { end.min(total) } else { end };
                if total > 0 && start >= total {
                    return unsatisfiable(total);
                }
                // Cap each response so a huge Range does not stall start.
                let end = end.min(start.saturating_add(SLICE));
                if start > slot.downloaded() {
                    return self.side_range(&target, ctype, start, end, total).await;
                }
                match slot.take(start, end).await {
                    Ok(slice) => {
                        let file_len = if total > 0 { total } else { slot.downloaded() };
                        ranged(ctype, start, file_len, slice, false)
                    }
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Open { start }) => {
                if total > 0 && start >= total {
                    return unsatisfiable(total);
                }
                // `bytes=N-` means "from here to EOF". Stream the growing cache as
                // a single 206 whose Content-Range names the full resource.
                self.serve_open_stream(slot, ctype, start, total).await
            }
            None => {
                // WebView2 <audio src> often GETs without Range. An unsolicited
                // 206 becomes MEDIA_ERR_SRC_NOT_SUPPORTED ("no supported source").
                // Answer 200 and stream from the on-disk cache as it fills.
                self.serve_full_stream(slot, ctype, total).await
            }
        }
    }

    async fn serve_audio_complete(
        &self,
        slot: Arc<MediaSlot>,
        ctype: String,
        method: Method,
        range: Option<String>,
    ) -> Response {
        if let Err(err) = slot.wait_finished().await {
            return plain(StatusCode::BAD_GATEWAY, &err);
        }
        let got = slot.downloaded();
        let total = slot.total.load(Ordering::Acquire);
        if got == 0 {
            return plain(StatusCode::BAD_GATEWAY, "这首歌是空的");
        }
        if total > 0 && got < total {
            return plain(
                StatusCode::BAD_GATEWAY,
                &format!("这首歌只下到 {got}/{total} 字节，连接中断了"),
            );
        }
        let file_len = if total > 0 { total } else { got };
        let head = method == Method::Head;
        if head {
            return media_response(StatusCode::OK, &ctype, Some(file_len), None, Body::empty(), true);
        }
        let parsed = range.as_deref().and_then(parse_byte_range);
        match parsed {
            Some(ByteRange::Suffix { len }) => {
                let start = file_len.saturating_sub(len.min(SLICE).min(file_len));
                match slot.read_span(start, file_len).await {
                    Ok(slice) => ranged(ctype, start, file_len, slice, false),
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Closed { start, end }) => {
                let end = end.min(file_len);
                if start >= file_len {
                    return unsatisfiable(file_len);
                }
                match slot.read_span(start, end).await {
                    Ok(slice) => ranged(ctype, start, file_len, slice, false),
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            Some(ByteRange::Open { start }) => {
                if start >= file_len {
                    return unsatisfiable(file_len);
                }
                match slot.read_span(start, file_len).await {
                    Ok(slice) => ranged(ctype, start, file_len, slice, false),
                    Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
                }
            }
            None => match slot.read_span(0, file_len).await {
                Ok(slice) => media_response(
                    StatusCode::OK,
                    &ctype,
                    Some(slice.len() as u64),
                    None,
                    Body::from(slice),
                    false,
                ),
                Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
            },
        }
    }

    async fn serve_full_stream(&self, slot: Arc<MediaSlot>, ctype: String, total: u64) -> Response {
        if let Err(err) = slot.wait_until(priming_end(0, total)).await {
            return plain(StatusCode::BAD_GATEWAY, &err);
        }
        let file_len = (total > 0).then_some(total);
        stream_range(slot, ctype, 0, file_len, file_len, false)
    }

    async fn serve_open_stream(
        &self,
        slot: Arc<MediaSlot>,
        ctype: String,
        start: u64,
        total: u64,
    ) -> Response {
        if start > slot.downloaded() {
            let end = slice_end(start, total);
            return self.side_range(&slot.target, ctype, start, end, total).await;
        }
        if let Err(err) = slot.wait_until(priming_end(start, total)).await {
            return plain(StatusCode::BAD_GATEWAY, &err);
        }
        if slot.downloaded() <= start {
            return plain(StatusCode::BAD_GATEWAY, "影音还没下到这里就断了");
        }
        let file_len = (total > 0).then_some(total);
        stream_range(slot, ctype, start, file_len, file_len, true)
    }

    /// Parallel Range pull for seeks ahead of the sequential download cursor.
    async fn side_range(
        &self,
        target: &str,
        ctype: String,
        start: u64,
        end: u64,
        total: u64,
    ) -> Response {
        if end <= start {
            return plain(StatusCode::BAD_GATEWAY, "无效的影音范围");
        }
        let last = end - 1;
        let range = format!("bytes={start}-{last}");
        tracing::info!(%target, %range, "影音侧拉未缓存段");
        match fetch_host_range(
            &self.inner.connection,
            &self.inner.traffic,
            &self.inner.password,
            target,
            &range,
            end - start,
        )
        .await
        {
            Ok(slice) => {
                let file_len = if total > 0 {
                    total
                } else {
                    start + slice.len() as u64
                };
                ranged(ctype, start, file_len, slice, false)
            }
            Err(err) => plain(StatusCode::BAD_GATEWAY, &err),
        }
    }

    async fn slot_for(&self, target: &str) -> Arc<MediaSlot> {
        let key = media_slot_key(target);
        let mut slots = self.inner.slots.lock().await;
        if let Some(found) = slots.iter().find(|slot| slot.key == key) {
            // A cancelled/failed slot must not poison the next play of the same song.
            if !slot_is_poisoned(found) {
                return found.clone();
            }
            if let Some(index) = slots.iter().position(|slot| Arc::ptr_eq(slot, found)) {
                slots.remove(index).cancel();
            }
        }
        while slots.len() >= 6 {
            let drop_at = slots
                .iter()
                .position(|slot| slot.done.load(Ordering::Acquire))
                .unwrap_or(0);
            slots.remove(drop_at).cancel();
        }
        let slot = MediaSlot::start(
            key,
            target.to_string(),
            self.inner.connection.clone(),
            self.inner.traffic.clone(),
            self.inner.password.clone(),
        );
        slots.push(slot.clone());
        slot
    }
}

fn slot_is_poisoned(slot: &MediaSlot) -> bool {
    slot.done.load(Ordering::Acquire)
        && (slot.downloaded() == 0 || slot.failed.try_lock().ok().and_then(|g| g.clone()).is_some())
}

impl MediaSlot {
    fn start(
        key: String,
        target: String,
        connection: Connection,
        traffic: Arc<Traffic>,
        password: String,
    ) -> Arc<Self> {
        let (content_type, _) = watch::channel(None);
        let (cancel, cancel_rx) = watch::channel(false);
        let seq = MEDIA_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("popstall-{}-{seq}.media", std::process::id()));
        let slot = Arc::new(Self {
            key,
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
            if let Err(err) = pump(pumping.clone(), connection, traffic, password, cancel_rx).await {
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

    /// Block until at least `need` bytes are on disk, or the transfer ends/fails.
    async fn wait_until(&self, need: u64) -> Result<(), String> {
        loop {
            if let Some(err) = self.failure().await {
                return Err(err);
            }
            if self.downloaded() >= need || self.done.load(Ordering::Acquire) {
                return Ok(());
            }
            wait_notify(self).await;
        }
    }

    fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Acquire)
    }

    async fn take(&self, start: u64, end: u64) -> Result<Bytes, String> {
        if end <= start {
            return Ok(Bytes::new());
        }
        self.wait_until(end).await?;
        let len = self.downloaded();
        if start >= len {
            return Err("请求的位置超出了文件".to_string());
        }
        self.read_span(start, end.min(len)).await
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
    password: String,
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
        pass: password,
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
    if head.status == 401 {
        let message = recv.read_to_end(256).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&message);
        let text = text.trim();
        anyhow::bail!("{}", if text.is_empty() { "密码不对" } else { text });
    }
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

fn slice_end(start: u64, total: u64) -> u64 {
    let end = start.saturating_add(SLICE);
    if total > 0 { end.min(total) } else { end }
}

fn priming_end(start: u64, total: u64) -> u64 {
    let end = start.saturating_add(PRIMING);
    let end = if total > 0 { end.min(total) } else { end };
    if end <= start {
        slice_end(start, total).max(start.saturating_add(1))
    } else {
        end
    }
}

fn media_content_type(target: &str, reported: String) -> String {
    if reported.starts_with("audio/") || reported.starts_with("video/") {
        return reported;
    }
    match media_extension(target).as_deref() {
        Some("mp3") => "audio/mpeg".into(),
        Some("wav") => "audio/wav".into(),
        Some("flac") => "audio/flac".into(),
        Some("ogg") | Some("opus") => "audio/ogg".into(),
        Some("m4a") | Some("aac") => "audio/mp4".into(),
        Some("mp4") | Some("m4v") => "video/mp4".into(),
        Some("webm") => "video/webm".into(),
        Some("mov") => "video/quicktime".into(),
        Some("mkv") => "video/x-matroska".into(),
        Some("ogv") => "video/ogg".into(),
        _ => reported,
    }
}

/// Stream bytes from the growing cache. `Content-Length` is the remaining
/// resource size when known; the body blocks until each next chunk is on disk
/// so the player can start after PRIMING without waiting for the whole file.
fn stream_range(
    slot: Arc<MediaSlot>,
    ctype: String,
    start: u64,
    total: Option<u64>,
    read_end: Option<u64>,
    partial: bool,
) -> Response {
    let stop = read_end.or(total);
    let length = stop.and_then(|stop| stop.checked_sub(start));
    let content_range = if partial {
        Some(match stop {
            Some(stop) if stop > start => {
                let shown = total.map(|n| n.to_string()).unwrap_or_else(|| "*".to_string());
                format!("bytes {start}-{}/{shown}", stop - 1)
            }
            _ => format!("bytes {start}-/*"),
        })
    } else {
        None
    };
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
        if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        &ctype,
        length,
        content_range,
        body,
        false,
    )
}

async fn fetch_host_range(
    connection: &Connection,
    traffic: &Traffic,
    password: &str,
    target: &str,
    range: &str,
    want: u64,
) -> Result<Bytes, String> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| format!("打开影音侧拉流失败: {err:?}"))?;
    let frame = encode_request(&RequestFrame {
        method: Method::Get,
        target: target.to_string(),
        range: Some(range.to_string()),
        content_type: None,
        body: Vec::new(),
        pass: password.to_string(),
    })
    .map_err(|err| err.to_string())?;
    traffic.record_sent(frame.len() as u64);
    send.write_all(&frame)
        .await
        .map_err(|err| format!("侧拉请求失败: {err}"))?;
    send.finish()
        .map_err(|err| format!("结束侧拉请求失败: {err:?}"))?;
    let head = read_response_head(&mut recv)
        .await
        .map_err(|err| err.to_string())?;
    if head.status != 200 && head.status != 206 {
        let message = recv.read_to_end(256).await.unwrap_or_default();
        let text = String::from_utf8_lossy(&message);
        return Err(if text.trim().is_empty() {
            format!("侧拉失败 status={}", head.status)
        } else {
            text.trim().to_string()
        });
    }
    let limit = want.min(SLICE).max(1) as usize;
    let mut buf = vec![0u8; 64 * 1024];
    let mut body = Vec::with_capacity(limit.min(64 * 1024));
    while body.len() < limit {
        let max = (limit - body.len()).min(buf.len());
        match AsyncReadExt::read(&mut recv, &mut buf[..max]).await {
            Ok(0) => break,
            Ok(size) => {
                traffic.record_received(size as u64);
                body.extend_from_slice(&buf[..size]);
            }
            Err(err) => return Err(format!("侧拉中断: {err}")),
        }
    }
    if body.is_empty() {
        return Err("侧拉没有数据".to_string());
    }
    Ok(Bytes::from(body))
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
    use super::{
        ByteRange, PRIMING, SLICE, is_media_target, parse_byte_range, priming_end, slice_end,
    };

    #[test]
    fn recognizes_audio_and_video_names() {
        assert!(is_media_target("/room/night.MP4"));
        assert!(is_media_target("/room/song.mp3?x=1"));
        assert!(is_media_target("/media/Rolling%20in%20the%20deep.mp3"));
        assert!(super::is_audio_target("/media/Rolling%20in%20the%20deep.mp3"));
        assert!(!super::is_audio_target("/room/night.MP4"));
        assert!(!is_media_target("/room/index.html"));
        assert!(!is_media_target("/room/photo.png"));
        assert_eq!(
            super::media_slot_key("/media/Rolling%20in%20the%20deep.mp3"),
            "/media/Rolling in the deep.mp3"
        );
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

    #[test]
    fn priming_and_slice_stay_short_of_full_movie() {
        let total = 2 * 1024 * 1024 * 1024u64;
        assert_eq!(priming_end(0, total), PRIMING);
        assert_eq!(slice_end(0, total), SLICE);
        assert_eq!(slice_end(total - 100, total), total);
        assert_eq!(priming_end(total - 10, total), total);
    }

    #[test]
    fn media_content_type_fills_mp3_when_host_is_opaque() {
        assert_eq!(
            super::media_content_type("/media/Hallo.mp3", "application/octet-stream".into()),
            "audio/mpeg"
        );
        assert_eq!(
            super::media_content_type("/media/clip.mp4", "video/mp4".into()),
            "video/mp4"
        );
    }
}
