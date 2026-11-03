//! Metric record repository (port of `database/records`).

use rusqlite::{Connection, params};

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::{GPURecord, Record};
use crate::time::LocalTime;

/// Which physical table to read or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordTable {
    /// `records` / `gpu_records`, the short-term (roughly four hour) table.
    Live,
    /// `records_long_term` / `gpu_records_long_term`, the compressed table.
    LongTerm,
}

impl RecordTable {
    pub const fn table(self) -> &'static str {
        match self {
            Self::Live => "records",
            Self::LongTerm => "records_long_term",
        }
    }

    pub const fn gpu_table(self) -> &'static str {
        match self {
            Self::Live => "gpu_records",
            Self::LongTerm => "gpu_records_long_term",
        }
    }
}

/// Numeric columns that can be aggregated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordMetric {
    Cpu,
    Gpu,
    Ram,
    RamTotal,
    Swap,
    SwapTotal,
    Load,
    Temp,
    Disk,
    DiskTotal,
    NetIn,
    NetOut,
    NetTotalUp,
    NetTotalDown,
    Process,
    Connections,
    ConnectionsUdp,
}

impl RecordMetric {
    pub const fn column(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Ram => "ram",
            Self::RamTotal => "ram_total",
            Self::Swap => "swap",
            Self::SwapTotal => "swap_total",
            Self::Load => "load",
            Self::Temp => "temp",
            Self::Disk => "disk",
            Self::DiskTotal => "disk_total",
            Self::NetIn => "net_in",
            Self::NetOut => "net_out",
            Self::NetTotalUp => "net_total_up",
            Self::NetTotalDown => "net_total_down",
            Self::Process => "process",
            Self::Connections => "connections",
            Self::ConnectionsUdp => "connections_udp",
        }
    }
}

/// Aggregation function applied to a [`RecordMetric`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateFn {
    Avg,
    Min,
    Max,
    Sum,
    Count,
}

impl AggregateFn {
    pub const fn sql(self) -> &'static str {
        match self {
            Self::Avg => "AVG",
            Self::Min => "MIN",
            Self::Max => "MAX",
            Self::Sum => "SUM",
            Self::Count => "COUNT",
        }
    }
}

const RECORD_COLUMNS: &str = "`client`, `time`, `cpu`, `gpu`, `ram`, `ram_total`, `swap`, \
     `swap_total`, `load`, `temp`, `disk`, `disk_total`, `net_in`, `net_out`, `net_total_up`, \
     `net_total_down`, `process`, `connections`, `connections_udp`";
const RECORD_PLACEHOLDERS: &str =
    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19";

const GPU_COLUMNS: &str = "`client`, `time`, `device_index`, `device_name`, `mem_total`, \
     `mem_used`, `utilization`, `temperature`";
const GPU_PLACEHOLDERS: &str = "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8";

/// Access to `records` / `records_long_term` and their GPU counterparts.
pub struct Records<'a> {
    pub(crate) db: &'a Db,
}

impl Records<'_> {
    /// Insert one metric sample into `records`.
    pub fn insert(&self, record: &Record) -> Result<()> {
        self.insert_into(RecordTable::Live, record)
    }

    /// Insert one metric sample into the selected table.
    pub fn insert_into(&self, table: RecordTable, record: &Record) -> Result<()> {
        let conn = self.db.conn();
        insert_record(&conn, table.table(), record)
    }

    /// Insert one GPU sample into `gpu_records`.
    pub fn insert_gpu(&self, record: &GPURecord) -> Result<()> {
        self.insert_gpu_into(RecordTable::Live, record)
    }

    /// Insert one GPU sample into the selected table.
    pub fn insert_gpu_into(&self, table: RecordTable, record: &GPURecord) -> Result<()> {
        let conn = self.db.conn();
        insert_gpu_record(&conn, table.gpu_table(), record)
    }

    /// Insert many samples in a single transaction.
    pub fn insert_batch(&self, records: &[Record]) -> Result<()> {
        self.insert_batch_into(RecordTable::Live, records)
    }

    pub fn insert_batch_into(&self, table: RecordTable, records: &[Record]) -> Result<()> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        for record in records {
            insert_record(&tx, table.table(), record)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert one metric sample and its GPU rows atomically.
    ///
    /// The record is inserted first (matching Go's realtime order), then the
    /// GPU rows; any failure rolls the whole transaction back so a retry does
    /// not duplicate GPU samples.
    pub fn insert_with_gpu(&self, record: &Record, gpu_records: &[GPURecord]) -> Result<()> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        insert_record(&tx, RecordTable::Live.table(), record)?;
        for gpu in gpu_records {
            insert_gpu_record(&tx, RecordTable::Live.gpu_table(), gpu)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_gpu_batch(&self, records: &[GPURecord]) -> Result<()> {
        self.insert_gpu_batch_into(RecordTable::Live, records)
    }

    pub fn insert_gpu_batch_into(&self, table: RecordTable, records: &[GPURecord]) -> Result<()> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        for record in records {
            insert_gpu_record(&tx, table.gpu_table(), record)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Samples for one client in `[start, end]`, oldest first.
    ///
    /// This is the port of Go `GetRecordsByClientAndTime`: recent rows are
    /// read from `records` (clamped to the last four hours), long-term rows
    /// from `records_long_term`, and the recent rows are downsampled to one
    /// sample per 15-minute bucket when long-term rows exist.
    pub fn by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        self.combined_by_client_and_time(client, start, end)
    }

    /// Explicit alias for the combined live + long-term query.
    pub fn combined_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        let live = self.recent_records(client, start, end)?;
        let long_term = self.by_client_and_time_in(RecordTable::LongTerm, client, start, end)?;

        if long_term.is_empty() {
            // Go returns the raw recent rows when no long-term data exists.
            return Ok(live);
        }

        let mut records = downsample_recent_records(live);
        records.extend(long_term);
        Ok(records)
    }

    /// Live-table-only query, without long-term merging or downsampling.
    pub fn live_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        self.by_client_and_time_in(RecordTable::Live, client, start, end)
    }

    /// Query a single physical table.
    pub fn by_client_and_time_in(
        &self,
        table: RecordTable,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            &format!(
                "SELECT * FROM {} WHERE client = ?1 AND time >= ?2 AND time <= ?3 \
                 ORDER BY time ASC",
                table.table()
            ),
            params![client, start, end],
        )
    }

    /// Recent records clamped to the same window Go uses (`now - 4h - 1m`).
    fn recent_records(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        let four_hours_ago = recent_window_start(chrono::Utc::now());
        if end <= four_hours_ago {
            return Ok(Vec::new());
        }
        let recent_start = if start < four_hours_ago {
            four_hours_ago
        } else {
            start
        };
        self.by_client_and_time_in(RecordTable::Live, client, recent_start, end)
    }

    /// GPU samples for one client in `[start, end]`.
    ///
    /// This is the port of Go `GetGPURecordsByClientAndTime`: live samples
    /// from the last four hours plus long-term samples, ordered by time and
    /// device index. GPU records are not downsampled.
    pub fn gpu_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<GPURecord>> {
        self.combined_gpu_by_client_and_time(client, start, end)
    }

    /// Explicit alias for the combined GPU query.
    pub fn combined_gpu_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<GPURecord>> {
        let four_hours_ago = recent_window_start(chrono::Utc::now());

        let mut records = Vec::new();
        if end > four_hours_ago {
            let recent_start = if start < four_hours_ago {
                four_hours_ago
            } else {
                start
            };
            records.extend(self.gpu_by_client_and_time_in(
                RecordTable::Live,
                client,
                recent_start,
                end,
            )?);
        }
        records.extend(self.gpu_by_client_and_time_in(
            RecordTable::LongTerm,
            client,
            start,
            end,
        )?);
        Ok(records)
    }

    /// Live-table-only GPU query.
    pub fn live_gpu_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<GPURecord>> {
        self.gpu_by_client_and_time_in(RecordTable::Live, client, start, end)
    }

    /// Query a single physical GPU table.
    pub fn gpu_by_client_and_time_in(
        &self,
        table: RecordTable,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<GPURecord>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            &format!(
                "SELECT * FROM {} WHERE client = ?1 AND time >= ?2 AND time <= ?3 \
                 ORDER BY time ASC, device_index ASC",
                table.gpu_table()
            ),
            params![client, start, end],
        )
    }

    /// The most recent sample for a client.
    pub fn latest(&self, client: &str) -> Result<Option<Record>> {
        self.latest_in(RecordTable::Live, client)
    }

    pub fn latest_in(&self, table: RecordTable, client: &str) -> Result<Option<Record>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            &format!(
                "SELECT * FROM {} WHERE client = ?1 ORDER BY time DESC LIMIT 1",
                table.table()
            ),
            [client],
        )
    }

    /// The most recent GPU sample for a client/device pair.
    pub fn latest_gpu(&self, client: &str, device_index: i64) -> Result<Option<GPURecord>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT * FROM gpu_records WHERE client = ?1 AND device_index = ?2 \
             ORDER BY time DESC LIMIT 1",
            params![client, device_index],
        )
    }

    /// Aggregate one metric over a time range. Returns `None` when no rows
    /// match (or when every value is `NULL`).
    pub fn aggregate(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
        metric: RecordMetric,
        function: AggregateFn,
    ) -> Result<Option<f64>> {
        self.aggregate_in(RecordTable::Live, client, start, end, metric, function)
    }

    pub fn aggregate_in(
        &self,
        table: RecordTable,
        client: &str,
        start: LocalTime,
        end: LocalTime,
        metric: RecordMetric,
        function: AggregateFn,
    ) -> Result<Option<f64>> {
        let conn = self.db.conn();
        let sql = format!(
            "SELECT {}({}) FROM {} WHERE client = ?1 AND time >= ?2 AND time <= ?3",
            function.sql(),
            metric.column(),
            table.table()
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params![client, start, end])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<f64>>(0)?),
            None => Ok(None),
        }
    }

    /// Number of samples for a client in a range.
    pub fn count_in_range(&self, client: &str, start: LocalTime, end: LocalTime) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(
            &conn,
            "SELECT COUNT(*) FROM records WHERE client = ?1 AND time >= ?2 AND time <= ?3",
            params![client, start, end],
        )
    }

    /// Delete samples older than `before` from all four tables
    /// (Go `DeleteRecordBefore`).
    pub fn delete_before(&self, before: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        let mut deleted = 0;
        for table in [
            "records_long_term",
            "gpu_records_long_term",
            "gpu_records",
            "records",
        ] {
            deleted += conn.execute(&format!("DELETE FROM {table} WHERE time < ?1"), [before])?;
        }
        Ok(deleted)
    }

    /// Delete a client's samples from all four tables.
    pub fn delete_for_client(&self, client: &str) -> Result<usize> {
        let conn = self.db.conn();
        let mut deleted = 0;
        for table in [
            "records_long_term",
            "gpu_records_long_term",
            "gpu_records",
            "records",
        ] {
            deleted += conn.execute(&format!("DELETE FROM {table} WHERE client = ?1"), [client])?;
        }
        Ok(deleted)
    }

    /// Delete everything from all four record tables.
    pub fn delete_all(&self) -> Result<()> {
        let conn = self.db.conn();
        for table in [
            "records_long_term",
            "gpu_records_long_term",
            "gpu_records",
            "records",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), [])?;
        }
        Ok(())
    }

    /// All live samples, oldest first.
    pub fn all(&self) -> Result<Vec<Record>> {
        self.all_combined()
    }

    /// Every sample from `records` followed by every sample from
    /// `records_long_term`, matching Go `GetAllRecords`. Each table is ordered
    /// by time ascending; the two lists are concatenated without deduplication.
    pub fn all_combined(&self) -> Result<Vec<Record>> {
        let mut records = self.all_in(RecordTable::Live)?;
        records.extend(self.all_in(RecordTable::LongTerm)?);
        Ok(records)
    }

    pub fn all_in(&self, table: RecordTable) -> Result<Vec<Record>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            &format!("SELECT * FROM {} ORDER BY time ASC", table.table()),
            [],
        )
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM records", [])
    }
}

fn insert_record(conn: &Connection, table: &str, record: &Record) -> Result<()> {
    if record.client.is_empty() {
        return Err(DbError::InvalidData("record client is empty".to_owned()));
    }
    conn.execute(
        &format!("INSERT INTO {table} ({RECORD_COLUMNS}) VALUES ({RECORD_PLACEHOLDERS})"),
        params![
            record.client,
            record.time,
            record.cpu,
            record.gpu,
            record.ram,
            record.ram_total,
            record.swap,
            record.swap_total,
            record.load,
            record.temp,
            record.disk,
            record.disk_total,
            record.net_in,
            record.net_out,
            record.net_total_up,
            record.net_total_down,
            record.process,
            record.connections,
            record.connections_udp,
        ],
    )?;
    Ok(())
}

fn insert_gpu_record(conn: &Connection, table: &str, record: &GPURecord) -> Result<()> {
    if record.client.is_empty() {
        return Err(DbError::InvalidData(
            "GPU record client is empty".to_owned(),
        ));
    }
    conn.execute(
        &format!("INSERT INTO {table} ({GPU_COLUMNS}) VALUES ({GPU_PLACEHOLDERS})"),
        params![
            record.client,
            record.time,
            record.device_index,
            record.device_name,
            record.mem_total,
            record.mem_used,
            record.utilization,
            record.temperature,
        ],
    )?;
    Ok(())
}

/// Port of the recent-record grouping in Go `GetRecordsByClientAndTime`: when
/// long-term data exists, keep only the newest sample in each 15-minute bucket
/// and return the buckets in ascending time order.
fn downsample_recent_records(records: Vec<Record>) -> Vec<Record> {
    use std::collections::HashMap;

    let mut grouped: HashMap<i64, Record> = HashMap::new();
    for record in records {
        let bucket = record
            .time
            .timestamp()
            .unwrap_or_default()
            .div_euclid(15 * 60);
        match grouped.get(&bucket) {
            Some(existing) if existing.time >= record.time => {}
            _ => {
                grouped.insert(bucket, record);
            }
        }
    }

    let mut result: Vec<Record> = grouped.into_values().collect();
    result.sort_by_key(|record| record.time);
    result
}

/// Start of the "recent" window: the absolute instant `now - (4h + 1m)`
/// converted to the application timezone. Computing on the absolute instant
/// (rather than subtracting from a naive wall clock) keeps the window correct
/// across DST transitions, matching Go's `time.Now().Add(-4h-1m)`.
fn recent_window_start(now: chrono::DateTime<chrono::Utc>) -> LocalTime {
    LocalTime::from_instant(now - chrono::TimeDelta::minutes(4 * 60 + 1))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn recent_window_is_an_absolute_duration() {
        // 2026-11-01 06:30 UTC is 01:30 EST after the US fall-back.
        let now = Utc
            .with_ymd_and_hms(2026, 11, 1, 6, 30, 0)
            .single()
            .unwrap();
        let start = recent_window_start(now);
        assert_eq!(
            start.timestamp().unwrap(),
            now.timestamp() - (4 * 60 + 1) * 60
        );
    }
}
