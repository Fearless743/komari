//! Injectable wall clock.
//!
//! Presence expiry is time based. Production code uses [`SystemClock`], while
//! tests drive [`ManualClock`] and call `expire_due` explicitly so no test has
//! to sleep for the real timeout.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Minimal clock abstraction used by presence tracking.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> i64;
}

/// Wall clock backed by [`SystemTime`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Deterministic clock for tests.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    pub fn new(now_millis: i64) -> Self {
        Self {
            now: AtomicI64::new(now_millis),
        }
    }

    pub fn set(&self, now_millis: i64) {
        self.now.store(now_millis, Ordering::SeqCst);
    }

    pub fn advance(&self, millis: i64) {
        self.now.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}
