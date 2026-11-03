//! Load-based notification rules (Go `utils/notifier/load.go`).

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::models::{Client, EventMessage, LoadNotification, Record, message_event};
use komari_db::{Db, LocalTime};

use crate::error::Result;

/// Go `shouldSkipNotification`: the cooldown equals the rule interval.
pub fn should_skip_notification(task: &LoadNotification, now: DateTime<Utc>) -> bool {
    if task.last_notified.is_zero() {
        return false;
    }
    let Some(last) = task.last_notified.timestamp() else {
        return false;
    };
    now.timestamp() - last < task.interval * 60
}

/// `.NET`-style metric lookup used by `getMetricValue`.
pub fn get_metric_value(record: &Record, metric: &str, client: Option<&Client>) -> f32 {
    match metric {
        "cpu" | "Cpu" => record.cpu,
        "gpu" | "Gpu" => record.gpu,
        "net_in" | "netin" | "NetIn" => bytes_per_second_to_mbps(record.net_in),
        "net_out" | "netout" | "NetOut" => bytes_per_second_to_mbps(record.net_out),
        "ram" | "Ram" => client.map_or(0.0, |client| {
            percentage(record.ram, record.ram_total, client.mem_total)
        }),
        "swap" | "Swap" => client.map_or(0.0, |client| {
            percentage(record.swap, record.swap_total, client.swap_total)
        }),
        "disk" | "Disk" => client.map_or(0.0, |client| {
            percentage(record.disk, record.disk_total, client.disk_total)
        }),
        "load" | "Load" => record.load,
        "temp" | "Temp" => record.temp,
        "ram_total" | "RamTotal" => record.ram_total as f32,
        "swap_total" | "SwapTotal" => record.swap_total as f32,
        "disk_total" | "DiskTotal" => record.disk_total as f32,
        "net_total_up" | "NetTotalUp" => record.net_total_up as f32,
        "net_total_down" | "NetTotalDown" => record.net_total_down as f32,
        "process" | "Process" => record.process as f32,
        "connections" | "Connections" => record.connections as f32,
        "connections_udp" | "ConnectionsUdp" => record.connections_udp as f32,
        // Go falls back to reflection over the record's exported fields.
        _ => 0.0,
    }
}

fn percentage(used: i64, record_total: i64, client_total: i64) -> f32 {
    // Go only checks `record.*Total > 0`, then divides by the client's total.
    if record_total > 0 {
        // Dividing by a zero client total yields infinity, exactly like Go.
        used as f32 / client_total as f32 * 100.0
    } else {
        0.0
    }
}

fn bytes_per_second_to_mbps(bytes_per_second: i64) -> f32 {
    if bytes_per_second <= 0 {
        return 0.0;
    }
    (bytes_per_second as f64 * 8.0 / 1_000_000.0) as f32
}

/// Go `checkMetricThreshold`: a rule fires when at least
/// `int(len(records) * ratio)` samples (minimum one) reach the threshold.
pub fn check_metric_threshold(
    records: &[Record],
    task: &LoadNotification,
    client: Option<&Client>,
) -> bool {
    if records.is_empty() {
        return false;
    }

    let mut minimum = (records.len() as f32 * task.ratio) as i64;
    if minimum == 0 {
        minimum = 1;
    }

    let exceeded = records
        .iter()
        .filter(|record| get_metric_value(record, &task.metric, client) >= task.threshold)
        .count() as i64;
    exceeded >= minimum
}

/// Execute one load rule, updating `last_notified` like Go's
/// `executeLoadNotificationTask`.
pub fn execute_load_task(
    db: &Db,
    task: &mut LoadNotification,
    now: DateTime<Utc>,
) -> Result<Option<EventMessage>> {
    if should_skip_notification(task, now) {
        return Ok(None);
    }

    let window_start = LocalTime::from_instant(now - TimeDelta::minutes(task.interval));
    let window_end = LocalTime::from_instant(now);

    let mut overloaded = Vec::new();
    for client_id in task.clients.as_slice() {
        let records = db
            .records()
            .by_client_and_time(client_id, window_start, window_end)?;
        let client = db.clients().get(client_id)?;
        if check_metric_threshold(&records, task, client.as_ref()) {
            if let Some(client) = client {
                overloaded.push(client);
            }
        }
    }

    // Go updates last_notified even when no client is overloaded, and a
    // failed update only logs; it must not suppress the event.
    task.last_notified = window_end;
    let _ = db
        .notifications()
        .load_set_last_notified(task.id, window_end);

    if overloaded.is_empty() {
        return Ok(None);
    }

    Ok(Some(EventMessage {
        event: message_event::ALERT.to_owned(),
        clients: overloaded,
        time: window_end,
        message: task.name.clone(),
        emoji: "⚠️".to_owned(),
    }))
}

/// Evaluate every stored load rule once.
///
/// Go's `LoadNotificationService` owns per-interval tickers, `Reload` and a
/// stop channel. This crate exposes only the one-shot evaluation; the caller
/// owns periodic scheduling, consistently with the offline/traffic/expiry
/// loops.
pub fn run_all_load_tasks(db: &Db, now: DateTime<Utc>) -> Result<Vec<EventMessage>> {
    let mut events = Vec::new();
    for mut task in db.notifications().load_list()? {
        if let Some(event) = execute_load_task(db, &mut task, now)? {
            events.push(event);
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
    }

    fn record(cpu: f32) -> Record {
        Record {
            cpu,
            ..Record::default()
        }
    }

    fn task(ratio: f32) -> LoadNotification {
        LoadNotification {
            name: "high cpu".to_owned(),
            metric: "cpu".to_owned(),
            threshold: 80.0,
            ratio,
            interval: 15,
            ..LoadNotification::default()
        }
    }

    #[test]
    fn ratio_requires_enough_samples() {
        let records = [record(90.0), record(70.0), record(70.0), record(70.0)];
        // int(4 * 0.5) = 2, only one sample exceeds.
        assert!(!check_metric_threshold(&records, &task(0.5), None));
        // int(4 * 0.25) = 1.
        assert!(check_metric_threshold(&records, &task(0.25), None));
    }

    #[test]
    fn ratio_zero_still_requires_one_sample() {
        let records = [record(70.0)];
        assert!(!check_metric_threshold(&records, &task(0.0), None));
    }

    #[test]
    fn no_records_never_fires() {
        assert!(!check_metric_threshold(&[], &task(0.8), None));
    }

    #[test]
    fn cooldown_uses_the_rule_interval() {
        let mut rule = task(0.8);
        assert!(!should_skip_notification(&rule, now()));
        rule.last_notified = LocalTime::from_instant(now() - TimeDelta::minutes(14));
        assert!(should_skip_notification(&rule, now()));
        rule.last_notified = LocalTime::from_instant(now() - TimeDelta::minutes(15));
        assert!(!should_skip_notification(&rule, now()));
    }

    #[test]
    fn percentage_metrics_use_client_totals() {
        let mut client = Client::new("mem");
        client.mem_total = 1000;
        let record = Record {
            ram: 500,
            ram_total: 1,
            ..Record::default()
        };
        assert_eq!(get_metric_value(&record, "ram", Some(&client)), 50.0);
        assert_eq!(get_metric_value(&record, "Ram", Some(&client)), 50.0);
        assert_eq!(get_metric_value(&record, "ram", None), 0.0);
        assert_eq!(get_metric_value(&record, "swap", None), 0.0);
        assert_eq!(get_metric_value(&record, "disk", None), 0.0);
        assert_eq!(get_metric_value(&record, "unknown", Some(&client)), 0.0);
    }

    #[test]
    fn network_values_are_decimal_mbps() {
        let record = Record {
            net_in: 1_000_000,
            ..Record::default()
        };
        assert_eq!(get_metric_value(&record, "net_in", None), 8.0);
        assert_eq!(get_metric_value(&record, "netin", None), 8.0);
        assert_eq!(get_metric_value(&record, "NetIn", None), 8.0);
    }
}
