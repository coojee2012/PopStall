use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Which accounting bucket a session charge belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    GuestHost,
    HelperEgress,
}

impl fmt::Display for BudgetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestHost => formatter.write_str("guest-host"),
            Self::HelperEgress => formatter.write_str("helper-egress"),
        }
    }
}

/// Typed rejection returned when a session byte bucket is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub kind: BudgetKind,
    pub limit: u64,
    pub requested: u64,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} session budget exhausted: requested {} bytes, limit {}",
            self.kind, self.requested, self.limit
        )
    }
}

impl std::error::Error for BudgetExceeded {}

/// Atomic byte accounting for one guest session.
///
/// `guest_host` bounds traffic between guest and host; `helper_egress` bounds
/// bytes a helper forwards onward. A bridge charge updates both buckets and
/// rolls back if either bucket would overflow, so a rejected frame is never
/// partially counted.
#[derive(Debug)]
pub struct SessionBudget {
    guest_host: AtomicU64,
    helper_egress: AtomicU64,
    exhaustions: AtomicU64,
    guest_host_limit: u64,
    helper_egress_limit: u64,
}

impl Default for SessionBudget {
    fn default() -> Self {
        Self::new(MAX_RESPONSE_BYTES, MAX_MEDIA_BYTES)
    }
}

impl SessionBudget {
    pub fn new(guest_host_limit: u64, helper_egress_limit: u64) -> Self {
        Self {
            guest_host: AtomicU64::new(0),
            helper_egress: AtomicU64::new(0),
            exhaustions: AtomicU64::new(0),
            guest_host_limit,
            helper_egress_limit,
        }
    }

    pub fn guest_host_used(&self) -> u64 {
        self.guest_host.load(Ordering::Relaxed)
    }

    pub fn helper_egress_used(&self) -> u64 {
        self.helper_egress.load(Ordering::Relaxed)
    }

    pub fn guest_host_limit(&self) -> u64 {
        self.guest_host_limit
    }

    pub fn helper_egress_limit(&self) -> u64 {
        self.helper_egress_limit
    }

    pub fn exhaustions(&self) -> u64 {
        self.exhaustions.load(Ordering::Relaxed)
    }

    pub fn charge_guest_host(&self, nbytes: u64) -> Result<(), BudgetExceeded> {
        self.charge(
            &self.guest_host,
            self.guest_host_limit,
            nbytes,
            BudgetKind::GuestHost,
        )
    }

    pub fn charge_helper_egress(&self, nbytes: u64) -> Result<(), BudgetExceeded> {
        self.charge(
            &self.helper_egress,
            self.helper_egress_limit,
            nbytes,
            BudgetKind::HelperEgress,
        )
    }

    /// Charge a forwarded bridge frame against both buckets atomically enough
    /// for accounting: the first bucket is rolled back if the second rejects.
    pub fn charge_bridge(&self, nbytes: u64) -> Result<(), BudgetExceeded> {
        self.charge_guest_host(nbytes)?;
        if let Err(err) = self.charge_helper_egress(nbytes) {
            self.guest_host.fetch_sub(nbytes, Ordering::Relaxed);
            return Err(err);
        }
        Ok(())
    }

    fn charge(
        &self,
        counter: &AtomicU64,
        limit: u64,
        nbytes: u64,
        kind: BudgetKind,
    ) -> Result<(), BudgetExceeded> {
        if nbytes == 0 {
            return Ok(());
        }
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                let next = used.checked_add(nbytes)?;
                (next <= limit).then_some(next)
            })
            .map(|_| ())
            .map_err(|_| {
                self.exhaustions.fetch_add(1, Ordering::Relaxed);
                BudgetExceeded {
                    kind,
                    limit,
                    requested: nbytes,
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_budget_charges_bridge_bytes_and_rolls_back_on_exhaustion() {
        let budget = SessionBudget::new(10, 20);
        assert_eq!(budget.charge_bridge(6), Ok(()));
        assert_eq!(budget.guest_host_used(), 6);
        assert_eq!(budget.helper_egress_used(), 6);

        let err = budget
            .charge_bridge(5)
            .expect_err("guest-host cap must reject the frame");
        assert_eq!(
            err,
            BudgetExceeded {
                kind: BudgetKind::GuestHost,
                limit: 10,
                requested: 5,
            }
        );
        assert_eq!(budget.guest_host_used(), 6);
        assert_eq!(budget.helper_egress_used(), 6);
        assert_eq!(budget.exhaustions(), 1);

        assert_eq!(budget.charge_helper_egress(14), Ok(()));
        assert_eq!(budget.helper_egress_used(), 20);
        let err = budget
            .charge_helper_egress(1)
            .expect_err("helper egress cap must reject");
        assert_eq!(err.kind, BudgetKind::HelperEgress);
        assert_eq!(budget.exhaustions(), 2);
    }

    #[test]
    fn session_budget_defaults_use_existing_ceiling_constants() {
        let budget = SessionBudget::default();
        assert_eq!(budget.guest_host_limit(), MAX_RESPONSE_BYTES);
        assert_eq!(budget.helper_egress_limit(), MAX_MEDIA_BYTES);
        assert_eq!(budget.guest_host_used(), 0);
        assert_eq!(budget.helper_egress_used(), 0);
    }

    #[test]
    fn session_budget_overflow_is_a_typed_error_not_a_panic() {
        let budget = SessionBudget::new(u64::MAX, u64::MAX);
        assert_eq!(budget.charge_guest_host(u64::MAX), Ok(()));
        let err = budget
            .charge_guest_host(1)
            .expect_err("counter overflow must be rejected");
        assert_eq!(err.kind, BudgetKind::GuestHost);
        assert_eq!(budget.guest_host_used(), u64::MAX);
        assert_eq!(budget.exhaustions(), 1);
    }

    #[test]
    fn session_budget_ignores_zero_byte_charges() {
        let budget = SessionBudget::new(0, 0);
        assert_eq!(budget.charge_bridge(0), Ok(()));
        assert_eq!(budget.exhaustions(), 0);
    }
}
