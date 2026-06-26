//! The `Clock` seam — deterministic time. Proactive, commitments, and consolidation are all
//! time-driven; tests must control time. Production uses `SystemClock`; tests use `TestClock`.
use std::sync::atomic::{AtomicU64, Ordering};

/// Unix time in milliseconds.
pub type UnixMillis = u64;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> UnixMillis;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> UnixMillis {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Manually-advanced clock for deterministic tests.
pub struct TestClock(AtomicU64);

impl TestClock {
    pub fn new(start_ms: UnixMillis) -> Self {
        Self(AtomicU64::new(start_ms))
    }
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
    pub fn set(&self, ms: UnixMillis) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> UnixMillis {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_clock_advances_deterministically() {
        let c = TestClock::new(1000);
        assert_eq!(c.now_ms(), 1000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1500);
        c.set(42);
        assert_eq!(c.now_ms(), 42);
    }
}
