//! Traffic limit notifications (Go `utils/notifier/traffic.go`).

use std::collections::HashMap;

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::models::{Client, EventMessage, message_event};
use komari_db::{Db, LocalTime};

use crate::error::Result;

/// Latest reported traffic totals for one client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrafficUsage {
    pub total_up: i64,
    pub total_down: i64,
}

/// Go caches traffic steps for 30 days.
pub const TRAFFIC_CACHE_TTL: TimeDelta = TimeDelta::days(30);

#[derive(Clone, Copy, Debug)]
struct TrafficEntry {
    step: i64,
    updated_at: DateTime<Utc>,
}

/// In-memory threshold history, keyed by client UUID.
///
/// Matches Go's `go-cache` configured with a 30-day expiration: entries are
/// dropped lazily once they are 30 days old, so a client can be alerted again
/// after the cache entry expires.
#[derive(Default)]
pub struct TrafficTracker {
    steps: HashMap<String, TrafficEntry>,
}

impl TrafficTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn last_step(&self, client_id: &str) -> Option<i64> {
        self.steps.get(client_id).map(|entry| entry.step)
    }

    /// Drop entries whose 30-day TTL elapsed.
    pub fn purge_expired(&mut self, now: DateTime<Utc>) {
        self.steps
            .retain(|_, entry| now.signed_duration_since(entry.updated_at) < TRAFFIC_CACHE_TTL);
    }

    /// Return `true` when this step should be announced.
    ///
    /// A step lower than the recorded one means the traffic counter was reset,
    /// so the baseline drops to zero before the comparison.
    pub fn should_notify(
        &mut self,
        client_id: &str,
        current_step: i64,
        now: DateTime<Utc>,
    ) -> bool {
        self.purge_expired(now);
        let mut last_step = self
            .steps
            .get(client_id)
            .map(|entry| entry.step)
            .unwrap_or_default();
        if current_step < last_step {
            last_step = 0;
        }
        if current_step > last_step {
            self.steps.insert(
                client_id.to_owned(),
                TrafficEntry {
                    step: current_step,
                    updated_at: now,
                },
            );
            true
        } else {
            false
        }
    }
}

/// Compute the configured usage figure (`computeUsedByType`).
pub fn compute_used_by_type(limit_type: &str, up: i64, down: i64) -> i64 {
    match limit_type {
        "up" => up,
        "down" => down,
        "sum" => up + down,
        "min" => up.min(down),
        "max" => up.max(down),
        _ => up.max(down),
    }
}

/// Human-readable byte count (`humanBytes`), using 1024-based units.
pub fn human_bytes(bytes: i64) -> String {
    const UNIT: i64 = 1024;
    if bytes < UNIT {
        return format!("{bytes} B");
    }
    let mut divisor = UNIT;
    let mut exponent = 0usize;
    let mut value = bytes / UNIT;
    while value >= UNIT {
        divisor *= UNIT;
        exponent += 1;
        value /= UNIT;
    }
    let prefixes = ["K", "M", "G", "T", "P", "E"];
    let prefix = prefixes[exponent.min(prefixes.len() - 1)];
    format!("{:.2} {prefix}B", bytes as f64 / divisor as f64)
}

/// One traffic alert, including the event to deliver.
#[derive(Clone, Debug)]
pub struct TrafficAlert {
    pub client: Client,
    pub step: i64,
    pub used: i64,
    pub limit: i64,
    pub limit_type: String,
    pub message: String,
    pub event: EventMessage,
}

/// Evaluate every client against the configured traffic percentage.
///
/// `reports` is the latest report data (in Go, `ws.GetLatestReport()`).
pub fn check_traffic(
    db: &Db,
    reports: &HashMap<String, TrafficUsage>,
    tracker: &mut TrafficTracker,
    now: DateTime<Utc>,
) -> Result<Vec<TrafficAlert>> {
    if reports.is_empty() {
        return Ok(Vec::new());
    }

    let start_threshold = db
        .config()
        .get_as::<f64>("traffic_limit_percentage")?
        .unwrap_or(80.0);
    if start_threshold <= 0.0 {
        return Ok(Vec::new());
    }

    let mut base_step = if start_threshold < 0.0 {
        0
    } else {
        (start_threshold / 5.0).ceil() as i64 * 5
    };
    if base_step > 100 {
        base_step = 100;
    }

    let mut alerts = Vec::new();
    for client in db.clients().list()? {
        if client.traffic_limit <= 0 {
            continue;
        }
        let Some(report) = reports.get(&client.uuid) else {
            continue;
        };

        let limit_type = client.traffic_limit_type.to_lowercase();
        let used = compute_used_by_type(&limit_type, report.total_up, report.total_down);
        if used <= 0 {
            continue;
        }

        let percentage = used as f64 / client.traffic_limit as f64 * 100.0;
        if percentage < start_threshold {
            continue;
        }

        let mut current_step = (percentage / 5.0).floor() as i64 * 5;
        if current_step < base_step {
            current_step = base_step;
        }
        if !tracker.should_notify(&client.uuid, current_step, now) {
            continue;
        }

        let message = format!(
            "used {current_step}% ({used} / {limit}), type={limit_type}",
            used = human_bytes(used),
            limit = human_bytes(client.traffic_limit),
        );
        let event = EventMessage {
            event: message_event::TRAFFIC.to_owned(),
            clients: vec![client.clone()],
            time: LocalTime::from_instant(now),
            message: message.clone(),
            emoji: "⚠️".to_owned(),
        };
        alerts.push(TrafficAlert {
            limit: client.traffic_limit,
            client,
            step: current_step,
            used,
            limit_type,
            message,
            event,
        });
    }
    Ok(alerts)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
    }

    fn db_with_client(limit: i64, limit_type: &str) -> (Db, String) {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let mut client = Client::new("traffic");
        client.traffic_limit = limit;
        client.traffic_limit_type = limit_type.to_owned();
        db.clients().create(&client).unwrap();
        db.config().set("traffic_limit_percentage", &80.0).unwrap();
        (db, client.uuid)
    }

    #[test]
    fn steps_expire_after_thirty_days() {
        let (_, uuid) = db_with_client(1000, "sum");
        let mut tracker = TrafficTracker::new();
        assert!(tracker.should_notify(&uuid, 80, now()));
        assert!(!tracker.should_notify(&uuid, 80, now() + TimeDelta::days(29)));
        assert!(tracker.should_notify(&uuid, 80, now() + TimeDelta::days(31)));
    }

    #[test]
    fn usage_types_match_go() {
        assert_eq!(compute_used_by_type("up", 10, 20), 10);
        assert_eq!(compute_used_by_type("down", 10, 20), 20);
        assert_eq!(compute_used_by_type("sum", 10, 20), 30);
        assert_eq!(compute_used_by_type("min", 10, 20), 10);
        assert_eq!(compute_used_by_type("max", 10, 20), 20);
        assert_eq!(compute_used_by_type("unknown", 10, 20), 20);
    }

    #[test]
    fn human_bytes_matches_go_formatting() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.00 KB");
        assert_eq!(human_bytes(1536), "1.50 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MB");
    }

    #[test]
    fn below_start_threshold_is_ignored() {
        let (db, uuid) = db_with_client(1000, "sum");
        let mut reports = HashMap::new();
        reports.insert(
            uuid,
            TrafficUsage {
                total_up: 400,
                total_down: 399,
            },
        );
        let mut tracker = TrafficTracker::new();
        assert!(
            check_traffic(&db, &reports, &mut tracker, now())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn alerts_once_per_five_percent_step_and_after_reset() {
        let (db, uuid) = db_with_client(1000, "sum");
        let mut tracker = TrafficTracker::new();
        let mut reports = HashMap::new();
        reports.insert(
            uuid.clone(),
            TrafficUsage {
                total_up: 400,
                total_down: 400,
            },
        );
        let alerts = check_traffic(&db, &reports, &mut tracker, now()).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].step, 80);

        // Same step is deduplicated.
        assert!(
            check_traffic(&db, &reports, &mut tracker, now())
                .unwrap()
                .is_empty()
        );

        // 85% enters the next step.
        reports.insert(
            uuid.clone(),
            TrafficUsage {
                total_up: 425,
                total_down: 425,
            },
        );
        let alerts = check_traffic(&db, &reports, &mut tracker, now()).unwrap();
        assert_eq!(alerts[0].step, 85);

        // Counter reset drops the baseline and can notify at 80 again.
        reports.insert(
            uuid,
            TrafficUsage {
                total_up: 400,
                total_down: 400,
            },
        );
        let alerts = check_traffic(&db, &reports, &mut tracker, now()).unwrap();
        assert_eq!(alerts[0].step, 80);
    }
}
