use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_REQUEST_BODY: usize = 1024 * 1024;
pub const BANDWIDTH_BYTES_PER_SEC: u64 = 4 * 1024 * 1024;

/// Shared per-stall byte budget. Callers sleep outside the lock.
#[derive(Debug)]
pub struct BandwidthLimit {
    bytes_per_sec: u64,
    state: Mutex<(Instant, u64)>,
}

impl BandwidthLimit {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec,
            state: Mutex::new((Instant::now(), 0)),
        }
    }

    pub async fn consume(&self, nbytes: u64) {
        if self.bytes_per_sec == 0 || nbytes == 0 {
            return;
        }
        loop {
            let wait = {
                let mut guard = self.state.lock().await;
                let (started, used) = &mut *guard;
                if started.elapsed() >= Duration::from_secs(1) {
                    *started = Instant::now();
                    *used = 0;
                }
                if used.saturating_add(nbytes) <= self.bytes_per_sec {
                    *used += nbytes;
                    return;
                }
                Duration::from_secs(1).saturating_sub(started.elapsed())
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }
}
