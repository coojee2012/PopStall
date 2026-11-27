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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, watch};

use crate::p2p::integrity::{IntegrityDigest, MerkleProof, MerkleRoot};
use crate::p2p::{
    GuestLink, Method, RequestFrame, Traffic, encode_request, open_guest_stream,
    read_response_head_any, verify_response_body,
};
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
    expected_digest: Option<IntegrityDigest>,
}

/// Bytes we want on disk past `start` before answering the first slice.
const PRIMING: u64 = 256 * 1024;
/// Max bytes returned in one HTTP response (keeps waits short; player re-Ranges).
const SLICE: u64 = 1024 * 1024;

struct HubInner {
    slots: Mutex<Vec<Arc<MediaSlot>>>,
    link: GuestLink,
    traffic: Arc<Traffic>,
    password: String,
}

#[derive(Clone)]
struct MediaFetch {
    link: GuestLink,
    traffic: Arc<Traffic>,
    password: String,
    expected_digest: Option<IntegrityDigest>,
}

struct RangeRequest {
    target: String,
    range: String,
    want: u64,
}

struct RangeSpec {
    ctype: String,
    start: u64,
    end: u64,
    total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RangeProof {
    start: u64,
    end: u64,
    total: u64,
}

static MEDIA_FILE_SEQ: AtomicU64 = AtomicU64::new(1);

struct MediaSlot {
    /// Normalized path used for cache lookup (percent-decoded).
    key: String,
    /// Wire target as the browser requested (keep percent-encoding for http::Uri).
    target: String,
    fetch: MediaFetch,
    path: PathBuf,
    store: StdMutex<Option<std::fs::File>>,
    downloaded: AtomicU64,
    content_type: watch::Sender<Option<String>>,
    /// Must keep at least one receiver alive: tokio `watch::Sender::send` is a
    /// no-op (returns SendError) when every receiver was dropped, which left us
    /// with a finished download and the opaque "没有读到影片" error.
    _content_type_rx: watch::Receiver<Option<String>>,
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
            "mp3"
                | "wav"
                | "flac"
                | "ogg"
                | "opus"
                | "m4a"
                | "aac"
                | "aiff"
                | "mp4"
                | "m4v"
                | "mov"
                | "webm"
                | "mkv"
                | "avi"
                | "ogv"
                | "mpg"
                | "mpeg"
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
fn media_slot_key(target: &str, digest: Option<&IntegrityDigest>) -> String {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    let base = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| path.to_string());
    match digest {
        Some(digest) => format!("{base}#{}", digest.to_hex()),
        None => base,
    }
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
    pub fn new(link: GuestLink, traffic: Arc<Traffic>, password: String) -> Self {
        Self {
            inner: Arc::new(HubInner {
                slots: Mutex::new(Vec::new()),
                link,
                traffic,
                password,
            }),
            expected_digest: None,
        }
    }

    pub fn with_expected_digest(mut self, digest: IntegrityDigest) -> Self {
        self.expected_digest = Some(digest);
        self
    }

    pub fn with_optional_expected_digest(mut self, digest: Option<IntegrityDigest>) -> Self {
        self.expected_digest = digest;
        self
    }

    pub async fn serve(&self, method: Method, target: String, range: Option<String>) -> Response {
        let slot = self.slot_for(&target).await;
        if let Err(err) = slot.wait_ready().await {
            // Finished bytes on disk but header watch was lost — still playable.
            if slot.done.load(Ordering::Acquire)
                && slot.downloaded() > 0
                && slot.failure().await.is_none()
            {
                tracing::warn!(target = %target, "影音头缺失，按扩展名继续播放");
            } else {
                return plain(StatusCode::BAD_GATEWAY, &err);
            }
        }
        let status = {
            let raw = slot.status.load(Ordering::Acquire);
            if raw == 0 && slot.downloaded() > 0 {
                200
            } else {
                raw
            }
        };
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
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
            return media_response(
                status,
                &ctype,
                None,
                None,
                Body::from(body),
                method == Method::Head,
            );
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
                    return self
                        .side_range(
                            slot,
                            RangeSpec {
                                ctype,
                                start,
                                end,
                                total,
                            },
                        )
                        .await;
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
            return media_response(
                StatusCode::OK,
                &ctype,
                Some(file_len),
                None,
                Body::empty(),
                true,
            );
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
            return self
                .side_range(
                    slot,
                    RangeSpec {
                        ctype,
                        start,
                        end,
                        total,
                    },
                )
                .await;
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
    async fn side_range(&self, slot: Arc<MediaSlot>, spec: RangeSpec) -> Response {
        if spec.end <= spec.start {
            return plain(StatusCode::BAD_GATEWAY, "无效的影音范围");
        }
        let last = spec.end - 1;
        let range = format!("bytes={}-{}", spec.start, last);
        tracing::info!(target = %slot.target, %range, "影音侧拉未缓存段");
        let request = RangeRequest {
            target: slot.target.clone(),
            range,
            want: spec.end - spec.start,
        };
        let mut attempt = 0u8;
        loop {
            attempt += 1;
            match fetch_host_range(&slot.fetch, &request).await {
                Ok((slice, proof, merkle_root, merkle_proof)) => {
                    match verify_range_slice_with_proof(RangeVerification {
                        expected_digest: slot.fetch.expected_digest.as_ref(),
                        merkle_root: merkle_root.as_ref(),
                        merkle_proof: merkle_proof.as_ref(),
                        range: proof.as_ref(),
                        start: spec.start,
                        slice: &slice,
                    }) {
                        Ok(()) => {
                            let slice_end = spec.start.saturating_add(slice.len() as u64);
                            if let Ok(cached) = slot.read_span(spec.start, slice_end).await {
                                if cached != slice {
                                    if attempt == 1 {
                                        let _ = slot.clear_from(spec.start).await;
                                        continue;
                                    }
                                    return plain(StatusCode::BAD_GATEWAY, "侧拉段与缓存不一致");
                                }
                            }
                            let file_len = if spec.total > 0 {
                                spec.total
                            } else {
                                slice_end
                            };
                            return ranged(spec.ctype, spec.start, file_len, slice, false);
                        }
                        Err(err) if attempt == 1 => {
                            tracing::warn!(%err, "影音切片校验失败，重试一次");
                            let _ = slot.clear_from(spec.start).await;
                            continue;
                        }
                        Err(err) => return plain(StatusCode::BAD_GATEWAY, &err),
                    }
                }
                Err(err) if attempt == 1 => {
                    tracing::warn!(%err, "影音侧拉失败，重试一次");
                    continue;
                }
                Err(err) => return plain(StatusCode::BAD_GATEWAY, &err),
            }
        }
    }

    async fn slot_for(&self, target: &str) -> Arc<MediaSlot> {
        let key = media_slot_key(target, self.expected_digest.as_ref());
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
        let fetch = MediaFetch {
            link: self.inner.link.clone(),
            traffic: self.inner.traffic.clone(),
            password: self.inner.password.clone(),
            expected_digest: self.expected_digest,
        };
        let slot = MediaSlot::start(key, target.to_string(), fetch);
        slots.push(slot.clone());
        slot
    }
}

fn slot_is_poisoned(slot: &MediaSlot) -> bool {
    slot.done.load(Ordering::Acquire)
        && (slot.downloaded() == 0
            || slot
                .failed
                .try_lock()
                .ok()
                .and_then(|g| g.clone())
                .is_some())
}

impl MediaSlot {
    fn start(key: String, target: String, fetch: MediaFetch) -> Arc<Self> {
        let (content_type, content_type_rx) = watch::channel(None);
        let (cancel, cancel_rx) = watch::channel(false);
        let seq = MEDIA_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("popstall-{}-{seq}.media", std::process::id()));
        let slot = Arc::new(Self {
            key,
            target,
            fetch: fetch.clone(),
            path,
            store: StdMutex::new(None),
            downloaded: AtomicU64::new(0),
            content_type,
            _content_type_rx: content_type_rx,
            status: AtomicU16::new(0),
            total: AtomicU64::new(0),
            done: AtomicBool::new(false),
            failed: Mutex::new(None),
            notify: Notify::new(),
            cancel,
        });
        let pumping = slot.clone();
        tokio::spawn(async move {
            if let Err(err) = pump(pumping.clone(), fetch, cancel_rx).await {
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
                return Err(self.failure().await.unwrap_or_else(|| {
                    if self.downloaded() > 0 {
                        "影音头信息丢失，请再点一次".to_string()
                    } else {
                        "没有读到影音数据".to_string()
                    }
                }));
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
        let len =
            usize::try_from(end - start).map_err(|_| "这段影音太长，读不进内存".to_string())?;
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

    async fn clear_from(&self, start: u64) -> Result<(), String> {
        let current = self.downloaded.load(Ordering::Acquire);
        let keep = start.min(current);
        {
            let mut guard = self.store.lock().unwrap_or_else(|err| err.into_inner());
            if let Some(file) = guard.as_mut() {
                truncate_cache(file, keep).map_err(|err| err.to_string())?;
            }
        }
        if current > keep {
            self.downloaded.store(keep, Ordering::Release);
        }
        self.notify.notify_waiters();
        Ok(())
    }

    async fn reset_cache(&self) -> Result<(), String> {
        {
            let mut guard = self.store.lock().unwrap_or_else(|err| err.into_inner());
            if let Some(file) = guard.as_mut() {
                truncate_cache(file, 0).map_err(|err| err.to_string())?;
            }
        }
        self.downloaded.store(0, Ordering::Release);
        self.status.store(0, Ordering::Release);
        self.total.store(0, Ordering::Release);
        self.content_type.send_replace(None);
        self.notify.notify_waiters();
        Ok(())
    }
}

async fn pump(
    slot: Arc<MediaSlot>,
    fetch: MediaFetch,
    mut cancel: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    for attempt in 0..2u8 {
        if attempt > 0 {
            slot.reset_cache()
                .await
                .map_err(|err| anyhow::anyhow!("{err}"))?;
        }
        let head_digest = pump_once(&slot, &fetch, &mut cancel, attempt).await?;
        let expected = head_digest.or(fetch.expected_digest);
        if let Some(expected) = expected {
            fetch.traffic.record_digest_check();
            let actual = IntegrityDigest::hash_file(&slot.path).await?;
            if actual != expected {
                fetch.traffic.record_digest_failure();
                if attempt == 0 {
                    tracing::warn!(target = %slot.target, "影音摘要不一致，重试一次");
                    continue;
                }
                anyhow::bail!("影音内容校验失败");
            }
        }
        return Ok(());
    }
    Ok(())
}

async fn pump_once(
    slot: &MediaSlot,
    fetch: &MediaFetch,
    cancel: &mut watch::Receiver<bool>,
    attempt: u8,
) -> anyhow::Result<Option<IntegrityDigest>> {
    let (mut send, mut recv) = open_guest_stream(
        &fetch.link.connection,
        &fetch.link.trust,
        &fetch.link.secret,
    )
    .await
    .map_err(|err| anyhow::anyhow!("打开影音流失败: {err:?}"))?;
    let frame = encode_request(&RequestFrame {
        method: Method::Get,
        target: slot.target.clone(),
        range: None,
        content_type: None,
        body: Vec::new(),
        pass: fetch.password.clone(),
    })?;
    let cache = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&slot.path)?;
    *slot.store.lock().unwrap_or_else(|err| err.into_inner()) = Some(cache);

    fetch.traffic.record_sent(frame.len() as u64);
    send.write_all(&frame).await?;
    send.shutdown()
        .await
        .map_err(|err| anyhow::anyhow!("结束影音请求失败: {err:?}"))?;
    let head = read_response_head_any(&mut recv).await?;
    if head.status() == 401 {
        let mut message = Vec::new();
        recv.take(256)
            .read_to_end(&mut message)
            .await
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&message);
        let text = text.trim();
        anyhow::bail!(
            "{}",
            if text.is_empty() {
                "密码不对"
            } else {
                text
            }
        );
    }
    if let Some(len) = head.content_length() {
        if len > MAX_MEDIA_BYTES {
            anyhow::bail!("这个影音超过 32 GB");
        }
        slot.total.store(len, Ordering::Release);
    }
    slot.status.store(head.status(), Ordering::Release);
    slot.content_type
        .send_replace(Some(head.content_type().to_string()));
    slot.notify.notify_waiters();
    let started = std::time::Instant::now();
    tracing::info!(
        target = %slot.target,
        status = head.status(),
        bytes = head.content_length().unwrap_or(0),
        attempt,
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
        fetch.traffic.record_received(size as u64);
        {
            let mut guard = slot.store.lock().unwrap_or_else(|err| err.into_inner());
            let file = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("缓存文件已关闭"))?;
            let received = slot.downloaded.load(Ordering::Acquire);
            if received + size as u64 > MAX_MEDIA_BYTES {
                anyhow::bail!("这个影音超过 32 GB");
            }
            file.seek(SeekFrom::Start(received))?;
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
    Ok(head.digest().cloned())
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
                let shown = total
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "*".to_string());
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
    fetch: &MediaFetch,
    request: &RangeRequest,
) -> Result<
    (
        Bytes,
        Option<RangeProof>,
        Option<MerkleRoot>,
        Option<MerkleProof>,
    ),
    String,
> {
    let (mut send, mut recv) = open_guest_stream(
        &fetch.link.connection,
        &fetch.link.trust,
        &fetch.link.secret,
    )
    .await
    .map_err(|err| format!("打开影音侧拉流失败: {err:?}"))?;
    let frame = encode_request(&RequestFrame {
        method: Method::Get,
        target: request.target.clone(),
        range: Some(request.range.clone()),
        content_type: None,
        body: Vec::new(),
        pass: fetch.password.clone(),
    })
    .map_err(|err| err.to_string())?;
    fetch.traffic.record_sent(frame.len() as u64);
    send.write_all(&frame)
        .await
        .map_err(|err| format!("侧拉请求失败: {err}"))?;
    send.shutdown()
        .await
        .map_err(|err| format!("结束侧拉请求失败: {err:?}"))?;
    let head = read_response_head_any(&mut recv)
        .await
        .map_err(|err| err.to_string())?;
    if head.status() != 200 && head.status() != 206 {
        let mut message = Vec::new();
        recv.take(256)
            .read_to_end(&mut message)
            .await
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&message);
        return Err(if text.trim().is_empty() {
            format!("侧拉失败 status={}", head.status())
        } else {
            text.trim().to_string()
        });
    }
    let proof = head.content_range().and_then(parse_content_range);
    let merkle_root = head.merkle_root().cloned();
    let merkle_proof = head.range_proof().cloned();
    let limit = request.want.min(SLICE).max(1) as usize;
    let mut buf = vec![0u8; 64 * 1024];
    let mut body = Vec::with_capacity(limit.min(64 * 1024));
    while body.len() < limit {
        let max = (limit - body.len()).min(buf.len());
        match AsyncReadExt::read(&mut recv, &mut buf[..max]).await {
            Ok(0) => break,
            Ok(size) => {
                fetch.traffic.record_received(size as u64);
                body.extend_from_slice(&buf[..size]);
            }
            Err(err) => return Err(format!("侧拉中断: {err}")),
        }
    }
    if body.is_empty() {
        return Err("侧拉没有数据".to_string());
    }
    verify_response_body(&head, &body, &fetch.traffic)
        .map_err(|err| format!("侧拉内容校验失败: {err}"))?;
    Ok((Bytes::from(body), proof, merkle_root, merkle_proof))
}

fn parse_content_range(value: &str) -> Option<RangeProof> {
    let (spec, total_text) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let total = total_text.trim().parse::<u64>().ok()?;
    if spec.trim() == "*" {
        return Some(RangeProof {
            start: 0,
            end: 0,
            total,
        });
    }
    let (start_text, end_text) = spec.trim().split_once('-')?;
    let start = start_text.trim().parse::<u64>().ok()?;
    let end = end_text.trim().parse::<u64>().ok()?.checked_add(1)?;
    if end <= start {
        return None;
    }
    Some(RangeProof { start, end, total })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RangeVerification<'a> {
    pub expected_digest: Option<&'a IntegrityDigest>,
    pub merkle_root: Option<&'a MerkleRoot>,
    pub merkle_proof: Option<&'a MerkleProof>,
    pub range: Option<&'a RangeProof>,
    pub start: u64,
    pub slice: &'a [u8],
}

pub(crate) fn verify_range_slice_with_proof(
    verification: RangeVerification<'_>,
) -> Result<(), String> {
    verify_range_slice(
        verification.expected_digest,
        verification.range,
        verification.start,
        verification.slice,
    )?;
    match (verification.merkle_root, verification.merkle_proof) {
        (None, None) => Ok(()),
        (Some(root), Some(proof)) => root
            .verify_range(proof, verification.slice)
            .map_err(|err| err.to_string()),
        _ => Err("Merkle 根与范围证明必须同时提供".to_string()),
    }
}

fn verify_range_slice(
    expected: Option<&IntegrityDigest>,
    proof: Option<&RangeProof>,
    start: u64,
    slice: &[u8],
) -> Result<(), String> {
    let len = slice.len() as u64;
    let Some(proof) = proof else {
        return Ok(());
    };
    if proof.total > 0 && proof.end > proof.total {
        return Err("范围头超出文件长度".to_string());
    }
    if proof.end > proof.start && (proof.start != start || proof.end - proof.start != len) {
        return Err("范围头与正文长度不一致".to_string());
    }
    if let Some(expected) = expected {
        if proof.start == 0 && proof.total == len {
            let actual = IntegrityDigest::hash_bytes(slice);
            if actual != *expected {
                return Err("影音内容校验失败".to_string());
            }
        }
    }
    Ok(())
}

fn truncate_cache(file: &mut std::fs::File, keep: u64) -> std::io::Result<()> {
    file.set_len(keep)?;
    file.seek(SeekFrom::Start(keep))?;
    Ok(())
}

fn ranged(ctype: String, start: u64, total: u64, slice: Bytes, head: bool) -> Response {
    if slice.is_empty() {
        return unsatisfiable(total);
    }
    let last = start + slice.len() as u64 - 1;
    let shown = if total > 0 {
        total.to_string()
    } else {
        "*".to_string()
    };
    let length = slice.len() as u64;
    media_response(
        StatusCode::PARTIAL_CONTENT,
        &ctype,
        Some(length),
        Some(format!("bytes {start}-{last}/{shown}")),
        if head {
            Body::empty()
        } else {
            Body::from(slice)
        },
        head,
    )
}

fn unsatisfiable(total: u64) -> Response {
    let mut response = plain(StatusCode::RANGE_NOT_SATISFIABLE, "请求的位置超出了文件");
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{total}"))
            .unwrap_or_else(|_| HeaderValue::from_static("bytes */*")),
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
            HeaderValue::from_str(ctype)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
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
    use std::io::{Read, Seek};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use super::{
        ByteRange, IntegrityDigest, MEDIA_FILE_SEQ, MerkleRoot, PRIMING, RangeProof,
        RangeVerification, SLICE, is_media_target, media_slot_key, parse_byte_range,
        parse_content_range, priming_end, slice_end, truncate_cache, verify_range_slice,
        verify_range_slice_with_proof,
    };
    use crate::p2p::integrity::{MERKLE_CHUNK_SIZE, MerkleTree};

    #[test]
    fn recognizes_audio_and_video_names() {
        assert!(is_media_target("/room/night.MP4"));
        assert!(is_media_target("/room/song.mp3?x=1"));
        assert!(is_media_target("/media/Rolling%20in%20the%20deep.mp3"));
        assert!(super::is_audio_target(
            "/media/Rolling%20in%20the%20deep.mp3"
        ));
        assert!(!super::is_audio_target("/room/night.MP4"));
        assert!(!is_media_target("/room/index.html"));
        assert!(!is_media_target("/room/photo.png"));
        assert_eq!(
            media_slot_key("/media/Rolling%20in%20the%20deep.mp3", None),
            "/media/Rolling in the deep.mp3"
        );
    }

    #[test]
    fn parses_closed_open_and_suffix_ranges() {
        match parse_byte_range("bytes=0-1023") {
            Some(ByteRange::Closed {
                start: 0,
                end: 1024,
            }) => {}
            other => panic!("closed range parsed as {other:?}"),
        }
        assert!(matches!(
            parse_byte_range("bytes=8-"),
            Some(ByteRange::Open { start: 8 })
        ));
        assert!(matches!(
            parse_byte_range("bytes=-4"),
            Some(ByteRange::Suffix { len: 4 })
        ));
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

    #[test]
    fn cache_key_includes_expected_digest() {
        let digest = IntegrityDigest::hash_bytes(b"abc");
        let plain = media_slot_key("/media/song.mp3", None);
        let verified = media_slot_key("/media/song.mp3", Some(&digest));
        assert_ne!(plain, verified);
        assert!(verified.contains(&digest.to_hex()));
    }

    #[test]
    fn content_range_is_parsed_for_verification() {
        assert_eq!(
            parse_content_range("bytes 2-4/11"),
            Some(RangeProof {
                start: 2,
                end: 5,
                total: 11
            })
        );
        assert_eq!(parse_content_range("bytes */11").unwrap().total, 11);
        assert!(parse_content_range("nonsense").is_none());
    }

    #[test]
    fn verification_rejects_corrupted_body_and_bad_range() {
        let expected = IntegrityDigest::hash_bytes(b"hello world");
        let full = RangeProof {
            start: 0,
            end: 11,
            total: 11,
        };
        assert!(verify_range_slice(None, Some(&full), 0, b"hello world").is_ok());
        assert!(verify_range_slice(Some(&expected), Some(&full), 0, b"hello world").is_ok());
        assert!(verify_range_slice(Some(&expected), Some(&full), 0, b"hello warld").is_err());

        let bad = RangeProof {
            start: 2,
            end: 6,
            total: 11,
        };
        assert!(verify_range_slice(Some(&expected), Some(&bad), 2, b"llo").is_err());
    }

    #[test]
    fn truncating_cache_removes_bad_tail() {
        let path =
            std::env::temp_dir().join(format!("popstall-media-cache-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"abcdef").unwrap();
        truncate_cache(&mut file, 3).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 3);
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    fn media_bytes(len: usize) -> Vec<u8> {
        let mut state = 0xdead_beef_0bad_f00du64;
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push((state & 0xff).to_le_bytes()[0]);
        }
        out
    }

    async fn media_merkle_fixture(len: usize) -> (PathBuf, MerkleTree, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "popstall-media-merkle-{}-{}",
            std::process::id(),
            MEDIA_FILE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.bin");
        let bytes = media_bytes(len);
        std::fs::write(&path, &bytes).unwrap();
        let tree = MerkleTree::from_file(&path).await.unwrap();
        (dir, tree, bytes)
    }

    #[tokio::test]
    async fn merkle_hook_verifies_cross_chunk_range_slice() {
        let (dir, tree, bytes) = media_merkle_fixture(2 * MERKLE_CHUNK_SIZE + 123).await;
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let start = chunk - 5;
        let end = chunk + 11;
        let root = tree.root();
        let proof = tree.proof(start, end).await.unwrap();
        let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        let range = RangeProof {
            start,
            end,
            total: u64::try_from(bytes.len()).unwrap(),
        };

        verify_range_slice_with_proof(RangeVerification {
            expected_digest: None,
            merkle_root: Some(&root),
            merkle_proof: Some(&proof),
            range: Some(&range),
            start,
            slice,
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn merkle_hook_rejects_tampered_slice_and_root() {
        let (dir, tree, bytes) = media_merkle_fixture(2 * MERKLE_CHUNK_SIZE + 9).await;
        let chunk = u64::try_from(MERKLE_CHUNK_SIZE).unwrap();
        let start = chunk - 7;
        let end = chunk + 13;
        let root = tree.root();
        let proof = tree.proof(start, end).await.unwrap();
        let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        let range = RangeProof {
            start,
            end,
            total: u64::try_from(bytes.len()).unwrap(),
        };
        let mut tampered = slice.to_vec();
        tampered[0] ^= 1;
        let wrong_root = MerkleRoot::from_slice(&[0u8; 32]).unwrap();

        assert!(
            verify_range_slice_with_proof(RangeVerification {
                expected_digest: None,
                merkle_root: Some(&root),
                merkle_proof: Some(&proof),
                range: Some(&range),
                start,
                slice: &tampered,
            })
            .is_err()
        );
        assert!(
            verify_range_slice_with_proof(RangeVerification {
                expected_digest: None,
                merkle_root: Some(&wrong_root),
                merkle_proof: Some(&proof),
                range: Some(&range),
                start,
                slice,
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn merkle_hook_fails_closed_when_root_has_no_proof() {
        let (dir, tree, bytes) = media_merkle_fixture(MERKLE_CHUNK_SIZE + 17).await;
        let root = tree.root();
        let start = 3u64;
        let end = 19u64;
        let slice = &bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        let range = RangeProof {
            start,
            end,
            total: u64::try_from(bytes.len()).unwrap(),
        };

        assert!(
            verify_range_slice_with_proof(RangeVerification {
                expected_digest: None,
                merkle_root: Some(&root),
                merkle_proof: None,
                range: Some(&range),
                start,
                slice,
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
