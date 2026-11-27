use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrafficCounters {
    pub retry_total: u64,
    pub digest_checks: u64,
    pub digest_failures: u64,
    pub nonce_failures: u64,
    pub helper_upgrade_ms: u64,
}

/// Bytes this process has sent to peers, and bytes it has received from them.
#[derive(Debug, Default)]
pub struct Traffic {
    sent: AtomicU64,
    received: AtomicU64,
    retry_total: AtomicU64,
    digest_checks: AtomicU64,
    digest_failures: AtomicU64,
    nonce_failures: AtomicU64,
    helper_upgrade_ms: AtomicU64,
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

    pub(crate) fn record_retry(&self) {
        self.retry_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_digest_check(&self) {
        self.digest_checks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_digest_failure(&self) {
        self.digest_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_nonce_failure(&self) {
        self.nonce_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_helper_upgrade_ms(&self, ms: u64) {
        if ms > 0 {
            self.helper_upgrade_ms.fetch_add(ms, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.sent.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn counters(&self) -> TrafficCounters {
        TrafficCounters {
            retry_total: self.retry_total.load(Ordering::Relaxed),
            digest_checks: self.digest_checks.load(Ordering::Relaxed),
            digest_failures: self.digest_failures.load(Ordering::Relaxed),
            nonce_failures: self.nonce_failures.load(Ordering::Relaxed),
            helper_upgrade_ms: self.helper_upgrade_ms.load(Ordering::Relaxed),
        }
    }
}
