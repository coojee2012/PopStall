use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
/// One audio or video stream may be much larger because it is written to a
/// temporary file and played while it arrives. This only stops a single
/// connection from filling the disk.
pub const MAX_MEDIA_BYTES: u64 = 32 * 1024 * 1024 * 1024;
pub const MAX_REQUEST_BODY: usize = 1024 * 1024;
pub const BANDWIDTH_BYTES_PER_SEC: u64 = 8 * 1024 * 1024;

/// Shared per-stall byte budget. A full second may leave at once, then bytes
/// refill smoothly so a song is not paused until the next whole second.
#[derive(Debug)]
pub struct BandwidthLimit {
    bytes_per_sec: u64,
    state: Mutex<(Instant, f64)>,
}

impl BandwidthLimit {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec,
            state: Mutex::new((Instant::now(), bytes_per_sec as f64)),
        }
    }

    pub async fn consume(&self, nbytes: u64) {
        if self.bytes_per_sec == 0 || nbytes == 0 {
            return;
        }
        let rate = self.bytes_per_sec as f64;
        loop {
            let wait = {
                let mut guard = self.state.lock().await;
                let now = Instant::now();
                let (last, tokens) = &mut *guard;
                let elapsed = now.saturating_duration_since(*last).as_secs_f64();
                *last = now;
                *tokens = (*tokens + elapsed * rate).min(rate);
                if *tokens >= nbytes as f64 {
                    *tokens -= nbytes as f64;
                    return;
                }
                Duration::from_secs_f64((nbytes as f64 - *tokens) / rate)
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }
}
