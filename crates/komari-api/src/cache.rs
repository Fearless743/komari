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
use crate::report::{Report, aggregate_reports, save_report, verify_report};

/// Counts produced by one flush.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushSummary {
    pub clients: usize,
    pub records: usize,
    pub gpu_records: usize,
    pub discarded_clients: usize,
    pub failed_clients: usize,
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

    /// Append one report to the aggregation buffer.
    ///
    /// Go clamps `cpu.usage < 0.01` before appending to `api.Records`; the
    /// separate latest-report cache keeps the raw value.
    pub fn record(&self, uuid: &str, mut report: Report) {
        if report.cpu.usage < 0.01 {
            report.cpu.usage = 0.01;
        }
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

    /// Recent in-memory reports for one client, mirroring Go
    /// `api.Records.Get(uuid)` used by `/api/recent/:uuid`.
    pub fn recent_for(&self, uuid: &str) -> Option<Vec<Report>> {
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(uuid)
            .map(|cached| cached.iter().map(|cached| cached.report.clone()).collect())
    }

    /// Snapshot of the recent in-memory reports, mirroring the data source
    /// Go's JSON-RPC `getRecords` reads from `api.Records`.
    pub fn recent_reports(&self) -> HashMap<String, Vec<Report>> {
        self.reports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(uuid, cached)| {
                (
                    uuid.clone(),
                    cached.iter().map(|cached| cached.report.clone()).collect(),
                )
            })
            .collect()
    }

    /// Aggregate every buffered client and persist one record per client.
    ///
    /// Each client is processed independently:
    /// * validation failures are logged and discarded;
    /// * database failures keep the samples buffered for the next flush;
    /// * successful/recent samples stay in the cache for the retention window,
    ///   matching Go's `Records.Set(filtered, ...)` and giving later RPC work a
    ///   recent in-memory data source.
    ///
    /// All clients are attempted even when some fail; the returned error is a
    /// summary of the database failures.
    pub fn flush(&self, db: &Db) -> Result<FlushSummary, (FlushSummary, String)> {
        let now_millis = self.clock.now_millis();
        let cutoff = now_millis.saturating_sub(self.retention.as_millis() as i64);
        let batches: Vec<(String, Vec<CachedReport>)> = {
            let mut reports = self
                .reports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            reports.drain().collect()
        };

        let time = LocalTime::from_unix_timestamp(now_millis / 1000).unwrap_or_else(LocalTime::now);
        let mut summary = FlushSummary::default();
        let mut retained: Vec<(String, Vec<CachedReport>)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for (uuid, cached) in batches {
            let recent: Vec<CachedReport> = cached
                .into_iter()
                .filter(|cached| cached.received_at_millis >= cutoff)
                .collect();
            if recent.is_empty() {
                continue;
            }
            let reports: Vec<Report> = recent.iter().map(|cached| cached.report.clone()).collect();
            let mut aggregate = aggregate_reports(&uuid, &reports, time, self.top_percentage);
            if aggregate.cpu.usage < 0.01 {
                aggregate.cpu.usage = 0.01;
            }
            if let Err(error) = verify_report(&aggregate) {
                tracing::warn!(client = %uuid, %error, "discarding invalid aggregated report");
                summary.discarded_clients += 1;
                continue;
            }

            match save_report(db, &uuid, &aggregate) {
                Ok(()) => {
                    summary.clients += 1;
                    summary.records += 1;
                    summary.gpu_records += aggregate
                        .gpu
                        .as_ref()
                        .map(|gpu| gpu.detailed_info.len())
                        .unwrap_or(0);
                    retained.push((uuid, recent));
                }
                Err(error) => {
                    summary.failed_clients += 1;
                    errors.push(format!("{uuid}: {error}"));
                    retained.push((uuid, recent));
                }
            }
        }

        {
            let mut reports = self
                .reports
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (uuid, cached) in retained {
                reports.entry(uuid).or_default().extend(cached);
            }
        }

        if errors.is_empty() {
            Ok(summary)
        } else {
            Err((
                summary,
                format!(
                    "{} client(s) failed to flush: {}",
                    errors.len(),
                    errors.join("; ")
                ),
            ))
        }
    }
}
