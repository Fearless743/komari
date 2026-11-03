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

    /// Samples for one client in `[start, end]`, oldest first (Go
    /// `GetRecordsByClientAndTime`).
    pub fn by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<Record>> {
        self.by_client_and_time_in(RecordTable::Live, client, start, end)
    }

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

    /// GPU samples for one client in `[start, end]`, ordered by time then
    /// device index.
    pub fn gpu_by_client_and_time(
        &self,
        client: &str,
        start: LocalTime,
        end: LocalTime,
    ) -> Result<Vec<GPURecord>> {
        self.gpu_by_client_and_time_in(RecordTable::Live, client, start, end)
    }

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
        self.all_in(RecordTable::Live)
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
