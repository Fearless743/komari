//! Record retention (Go `records.DeleteRecordBefore` + `record_preserve_time`).

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::{Db, LocalTime, Result};
use rusqlite::params;

/// Tables purged by [`cleanup_records`], in the order Go deletes them.
pub const RECORD_TABLES: &[&str] = &[
    "records_long_term",
    "gpu_records_long_term",
    "gpu_records",
    "records",
];

/// Default retention from `config.Legacy.RecordPreserveTime`: 720 hours.
pub const DEFAULT_PRESERVE_HOURS: i64 = 720;

/// Result of one retention pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub preserve_hours: i64,
    pub deleted: usize,
}

/// Delete samples strictly older than `preserve_hours` before `now`.
///
/// The comparison is `< cutoff`, so a row exactly at the cutoff is kept, the
/// same boundary Go uses with `Where("time < ?", before)`.
pub fn cleanup_records(
    db: &Db,
    now: DateTime<Utc>,
    preserve_hours: i64,
    batch_size: usize,
) -> Result<CleanupReport> {
    let duration = TimeDelta::try_hours(preserve_hours).unwrap_or_default();
    let cutoff = LocalTime::from_instant(now - duration);
    let batch = batch_size.max(1) as i64;
    let mut deleted = 0;

    loop {
        let count = db.with_connection(|conn| {
            let mut count = 0;
            for table in RECORD_TABLES {
                count += conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN \
                         (SELECT rowid FROM {table} WHERE time < ?1 LIMIT ?2)"
                    ),
                    params![cutoff, batch],
                )?;
            }
            Ok(count)
        })?;
        deleted += count;
        if count == 0 {
            break;
        }
    }

    Ok(CleanupReport {
        preserve_hours,
        deleted,
    })
}

/// Read `record_preserve_time` (default 720) and run the retention pass.
pub fn cleanup_from_config(
    db: &Db,
    now: DateTime<Utc>,
    batch_size: usize,
) -> Result<CleanupReport> {
    let preserve_hours = db
        .config()
        .get_as::<i64>("record_preserve_time")?
        .unwrap_or(DEFAULT_PRESERVE_HOURS);
    cleanup_records(db, now, preserve_hours, batch_size)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use komari_db::models::GPURecord;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 10, 0, 0, 0).single().unwrap()
    }

    fn insert_at(db: &Db, time: DateTime<Utc>, cpu: f32) {
        let mut record = komari_db::models::Record::new("c1");
        record.time = LocalTime::from_instant(time);
        record.cpu = cpu;
        db.records().insert(&record).unwrap();
    }

    #[test]
    fn empty_database_is_a_no_op() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let report = cleanup_records(&db, now(), 720, 100).unwrap();
        assert_eq!(report.deleted, 0);
    }

    #[test]
    fn cutoff_boundary_is_strict() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        insert_at(&db, now() - TimeDelta::hours(720), 1.0);
        insert_at(
            &db,
            now() - TimeDelta::hours(720) - TimeDelta::seconds(1),
            2.0,
        );

        let report = cleanup_records(&db, now(), 720, 100).unwrap();
        assert_eq!(report.deleted, 1);
        let remaining = db.records().all_in(komari_db::RecordTable::Live).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].cpu, 1.0);
    }

    #[test]
    fn batches_cover_all_tables_and_rows() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        for index in 0..5 {
            insert_at(
                &db,
                now() - TimeDelta::hours(800) - TimeDelta::minutes(index),
                1.0,
            );
        }
        let mut gpu = GPURecord {
            client: "c1".to_owned(),
            device_index: 0,
            ..GPURecord::default()
        };
        gpu.time = LocalTime::from_instant(now() - TimeDelta::hours(800));
        db.records().insert_gpu(&gpu).unwrap();

        let report = cleanup_records(&db, now(), 720, 2).unwrap();
        assert_eq!(report.deleted, 6);
        assert_eq!(
            db.records()
                .all_in(komari_db::RecordTable::Live)
                .unwrap()
                .len(),
            0
        );
    }
}
