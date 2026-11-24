use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes this process has sent to peers, and bytes it has received from them.
#[derive(Debug, Default)]
pub struct Traffic {
    sent: AtomicU64,
    received: AtomicU64,
}

impl Traffic {
    pub fn record_sent(&self, nbytes: u64) {
        if nbytes > 0 {
            self.sent.fetch_add(nbytes, Ordering::Relaxed);
        }
    }

    pub fn record_received(&self, nbytes: u64) {
        if nbytes > 0 {
            self.received.fetch_add(nbytes, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.sent.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed),
        )
    }
}
