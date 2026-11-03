//! Compaction and scheduler integration tests.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use komari_db::models::{GPURecord, Record};
use komari_db::{Db, LocalTime, RecordTable};
use komari_jobs::compaction::{CompactionOptions, compact, percentile, slot_start};
use komari_jobs::{CancelToken, JobOptions, run_once, run_once_with_options, spawn};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).single().unwrap()
}

fn db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db
}

fn insert(db: &Db, time: LocalTime, values: &[f32]) -> LocalTime {
    for (index, value) in values.iter().enumerate() {
        let mut record = Record::new("c1");
        record.time = time.add_seconds(index as i64 * 60);
        record.cpu = *value;
        record.net_in = *value as i64;
        record.net_total_up = *value as i64 * 100;
        db.records().insert(&record).unwrap();
    }
    time
}

#[test]
fn percentile_interpolates_like_go() {
    assert_eq!(percentile(&[], 0.7), 0.0);
    assert_eq!(percentile(&[10.0], 0.7), 10.0);
    // Floating-point interpolation matches Go to the last bit; integer
    // aggregates truncate the resulting value (3099.999... -> 3099).
    assert!((percentile(&[10.0, 20.0, 30.0, 40.0], 0.7) - 31.0).abs() < 1e-9);
    assert!((percentile(&[10.0, 20.0, 30.0, 40.0], 0.2) - 16.0).abs() < 1e-9);
    assert_eq!(percentile(&[1.0, 2.0], 0.0), 1.0);
    assert_eq!(percentile(&[1.0, 2.0], 1.0), 2.0);
}

#[test]
fn compaction_groups_per_client_and_fifteen_minutes() {
    let db = db();
    let base = slot_start(LocalTime::from_instant(now() - TimeDelta::hours(6)));
    insert(&db, base, &[10.0, 20.0, 30.0, 40.0]);
    // A second bucket, 20 minutes later.
    insert(&db, base.add_seconds(20 * 60), &[50.0, 60.0]);

    let report = compact(&db, &CompactionOptions::at(now())).unwrap();
    assert_eq!(report.record_groups, 2);

    let long_term = db.records().all_in(RecordTable::LongTerm).unwrap();
    assert_eq!(long_term.len(), 2);
    let first = long_term
        .iter()
        .find(|record| record.time == base)
        .expect("first bucket");
    assert_eq!(first.cpu, 31.0);
    assert_eq!(first.net_in, 16);
    assert_eq!(first.net_total_up, 3099);
}

#[test]
fn compaction_is_idempotent() {
    let db = db();
    let base = slot_start(LocalTime::from_instant(now() - TimeDelta::hours(6)));
    insert(&db, base, &[10.0, 20.0, 30.0, 40.0]);

    compact(&db, &CompactionOptions::at(now())).unwrap();
    let first = db.records().all_in(RecordTable::LongTerm).unwrap();
    compact(&db, &CompactionOptions::at(now())).unwrap();
    let second = db.records().all_in(RecordTable::LongTerm).unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0].cpu, second[0].cpu);
    assert_eq!(first[0].time, second[0].time);
}

#[test]
fn compaction_groups_gpu_by_device() {
    let db = db();
    let base = slot_start(LocalTime::from_instant(now() - TimeDelta::hours(6)));
    for (device, utilization) in [(0, 10.0), (0, 30.0), (1, 50.0)] {
        let gpu = GPURecord {
            client: "c1".to_owned(),
            time: base.add_seconds(device * 60),
            device_index: device,
            device_name: format!("gpu{device}"),
            mem_total: 1000,
            mem_used: utilization as i64 * 10,
            utilization,
            temperature: 50 + device,
        };
        db.records().insert_gpu(&gpu).unwrap();
    }

    let report = compact(&db, &CompactionOptions::at(now())).unwrap();
    assert_eq!(report.gpu_groups, 2);
    let rows = db.records().all_in(RecordTable::LongTerm).unwrap();
    assert!(rows.is_empty());

    let gpu_rows = db
        .records()
        .gpu_by_client_and_time_in(RecordTable::LongTerm, "c1", base, base.add_seconds(3600))
        .unwrap();
    assert_eq!(gpu_rows.len(), 2);
    let device_zero = gpu_rows
        .iter()
        .find(|record| record.device_index == 0)
        .unwrap();
    assert_eq!(device_zero.utilization, 24.0);
    assert_eq!(device_zero.device_name, "gpu0");
}

#[test]
fn compaction_deletes_only_raw_rows_older_than_five_hours() {
    let db = db();
    let very_old = LocalTime::from_instant(now() - TimeDelta::hours(6));
    let still_recent =
        LocalTime::from_instant(now() - TimeDelta::hours(4) - TimeDelta::minutes(30));
    let live = LocalTime::from_instant(now() - TimeDelta::minutes(5));

    for time in [very_old, still_recent, live] {
        let mut record = Record::new("c1");
        record.time = time;
        record.cpu = 10.0;
        db.records().insert(&record).unwrap();
    }

    let report = compact(&db, &CompactionOptions::at(now())).unwrap();
    assert_eq!(report.raw_records_deleted, 1);
    let remaining = db.records().all_in(RecordTable::Live).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|record| record.time != very_old));
}

#[test]
fn run_once_is_repeatable_and_applies_retention() {
    let db = db();
    db.config().set("record_preserve_time", &720i64).unwrap();
    let mut old = Record::new("c1");
    old.time = LocalTime::from_instant(now() - TimeDelta::hours(800));
    db.records().insert(&old).unwrap();

    let first = run_once(&db, now()).unwrap();
    assert_eq!(first.cleanup.preserve_hours, 720);
    assert_eq!(first.cleanup.deleted, 1);

    let second = run_once(&db, now()).unwrap();
    assert_eq!(second.cleanup.deleted, 0);
}

#[test]
fn run_once_honours_explicit_preserve_hours_and_batch_size() {
    let db = db();
    for index in 0..5 {
        let mut record = Record::new("c1");
        record.time =
            LocalTime::from_instant(now() - TimeDelta::hours(10) - TimeDelta::minutes(index));
        db.records().insert(&record).unwrap();
    }

    let report = run_once_with_options(
        &db,
        now(),
        &JobOptions {
            preserve_hours: Some(1),
            batch_size: 2,
            vacuum: false,
        },
    )
    .unwrap();
    assert_eq!(report.cleanup.deleted, 5);
}

#[tokio::test]
async fn spawn_runs_and_stops_on_cancellation() {
    let db = Arc::new(db());
    let cancel = CancelToken::new();
    let handle = spawn(db.clone(), Duration::from_millis(10), cancel.clone());

    tokio::time::sleep(Duration::from_millis(30)).await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("scheduler did not stop after cancellation")
        .expect("scheduler task panicked");
    assert!(cancel.is_cancelled());
}
