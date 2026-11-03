//! Background scheduler entry points.
//!
//! The crate deliberately does not start an infinite loop on its own. Callers
//! use [`run_once`] for tests or one-shot maintenance, or [`spawn`] to obtain a
//! cancellable Tokio task.

pub mod compaction;
pub mod records;

pub use compaction::{CompactionOptions, CompactionReport, compact, percentile, slot_start};
pub use records::{
    CleanupReport, DEFAULT_PRESERVE_HOURS, RECORD_TABLES, cleanup_from_config, cleanup_records,
};

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use komari_db::{Db, Result};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Options for [`run_once`].
#[derive(Clone, Debug)]
pub struct JobOptions {
    /// Override `record_preserve_time`; `None` reads it from the database.
    pub preserve_hours: Option<i64>,
    /// Rows fetched per batch during cleanup and compaction.
    pub batch_size: usize,
    /// Run `VACUUM` after compaction.
    pub vacuum: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            preserve_hours: None,
            batch_size: crate::compaction::DEFAULT_BATCH_SIZE,
            vacuum: true,
        }
    }
}

/// Outcome of one scheduled pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobReport {
    pub cleanup: CleanupReport,
    pub compaction: CompactionReport,
}

/// Run retention + compaction once. Safe to call repeatedly.
pub fn run_once(db: &Db, now: DateTime<Utc>) -> Result<JobReport> {
    run_once_with_options(db, now, &JobOptions::default())
}

/// Run retention + compaction with explicit options.
pub fn run_once_with_options(
    db: &Db,
    now: DateTime<Utc>,
    options: &JobOptions,
) -> Result<JobReport> {
    let cleanup = match options.preserve_hours {
        Some(hours) => cleanup_records(db, now, hours, options.batch_size)?,
        None => cleanup_from_config(db, now, options.batch_size)?,
    };

    let compaction = compact(
        db,
        &CompactionOptions {
            now,
            batch_size: options.batch_size,
            vacuum: options.vacuum,
        },
    )?;

    Ok(JobReport {
        cleanup,
        compaction,
    })
}

/// Retention hours effective for a run (`record_preserve_time` or 720).
pub fn preserve_hours(db: &Db) -> Result<i64> {
    Ok(db
        .config()
        .get_as::<i64>("record_preserve_time")?
        .unwrap_or(DEFAULT_PRESERVE_HOURS))
}

/// A cooperative cancellation token shared by the scheduler task.
#[derive(Clone)]
pub struct CancelToken {
    sender: Arc<watch::Sender<bool>>,
    receiver: watch::Receiver<bool>,
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
            receiver,
        }
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Resolve once cancellation is requested.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut receiver = self.receiver.clone();
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }
    }
}

/// Spawn the periodic maintenance loop.
///
/// The first tick runs immediately (Go calls `CompactRecord()` at startup),
/// then every `interval`. Cancellation stops the loop after the current pass.
pub fn spawn(db: Arc<Db>, interval: Duration, cancel: CancelToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(error) = run_once(&db, Utc::now()) {
                        tracing::error!(%error, "scheduled record maintenance failed");
                    }
                }
            }
        }
    })
}
