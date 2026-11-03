//! In-memory report buffer and periodic aggregation.
//!
//! Mirrors Go `api.Records` plus `api.SaveClientReportToDB`: agents append raw
//! samples, and once per minute the buffered reports are aggregated using the
//! top 30% (descending) of each metric before one row is written to
//! `records` / `gpu_records`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use komari_db::{Db, LocalTime};

use crate::clock::Clock;
use crate::report::{Report, aggregate_reports, save_report};

/// Counts produced by one flush.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushSummary {
    pub clients: usize,
    pub records: usize,
    pub gpu_records: usize,
}

struct CachedReport {
    report: Report,
    received_at_millis: i64,
}

/// Buffered reports awaiting aggregation.
pub struct ReportCache {
    clock: Arc<dyn Clock>,
    retention: Duration,
    top_percentage: f64,
    reports: Mutex<HashMap<String, Vec<CachedReport>>>,
}

impl ReportCache {
    pub fn new(clock: Arc<dyn Clock>, retention: Duration, top_percentage: f64) -> Self {
        Self {
            clock,
            retention,
            top_percentage,
            reports: Mutex::new(HashMap::new()),
        }
    }

    /// Append one raw report. The latest-report cache stores the same raw
    /// values; clamping happens only on the database write path.
    pub fn record(&self, uuid: &str, report: Report) {
        let received_at_millis = self.clock.now_millis();
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(uuid.to_owned())
            .or_default()
            .push(CachedReport {
                report,
                received_at_millis,
            });
    }

    pub fn buffered(&self, uuid: &str) -> usize {
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(uuid)
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub fn buffered_clients(&self) -> usize {
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Aggregate every buffered client and persist one record per client.
    ///
    /// Samples older than the retention window are discarded without being
    /// written, matching the cache expiry in Go.
    pub fn flush(&self, db: &Db) -> Result<FlushSummary, String> {
        let now_millis = self.clock.now_millis();
        let cutoff = now_millis.saturating_sub(self.retention.as_millis() as i64);
        let batches = {
            let mut reports = self
                .reports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut batches = HashMap::new();
            for (uuid, cached) in reports.drain() {
                let recent: Vec<Report> = cached
                    .into_iter()
                    .filter(|cached| cached.received_at_millis >= cutoff)
                    .map(|cached| cached.report)
                    .collect();
                if !recent.is_empty() {
                    batches.insert(uuid, recent);
                }
            }
            batches
        };

        let time = LocalTime::from_unix_timestamp(now_millis / 1000).unwrap_or_else(LocalTime::now);
        let mut summary = FlushSummary::default();
        for (uuid, reports) in batches {
            let aggregate = aggregate_reports(&uuid, &reports, time, self.top_percentage);
            save_report(db, &uuid, &aggregate)?;
            summary.clients += 1;
            summary.records += 1;
            summary.gpu_records += aggregate
                .gpu
                .as_ref()
                .map(|gpu| gpu.detailed_info.len())
                .unwrap_or(0);
        }
        Ok(summary)
    }
}
