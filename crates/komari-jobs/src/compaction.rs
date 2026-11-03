//! 15-minute record compaction (Go `records.CompactRecord`).
//!
//! `records` rows older than four hours are grouped per client and 15-minute
//! bucket and reduced with the same percentiles Go uses:
//! * most metrics use the 70th percentile,
//! * `net_in` / `net_out` use the 20th percentile,
//! * GPU rows additionally group by device index and keep the device name.

use std::collections::HashMap;

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::models::{FromRow, GPURecord, Record};
use komari_db::{Db, LocalTime, Result};
use rusqlite::{Connection, params};

/// Default number of rows read per batch.
pub const DEFAULT_BATCH_SIZE: usize = 1000;

/// How far back live rows are retained after they have been compacted.
///
/// Go compacts rows older than four hours, then deletes raw rows older than
/// `fourHoursAgo.Add(-1h)`, i.e. five hours before "now".
const RAW_DELETE_HOURS: i64 = 5;

const HIGH_PERCENTILE: f64 = 0.7;
const NETWORK_PERCENTILE: f64 = 0.2;
const BUCKET_SECONDS: i64 = 15 * 60;

const RECORD_COLUMNS: &str = "client, time, cpu, gpu, ram, ram_total, swap, swap_total, \
     load, temp, disk, disk_total, net_in, net_out, net_total_up, net_total_down, \
     process, connections, connections_udp";
const RECORD_PLACEHOLDERS: &str =
    "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19";
const RECORD_UPDATES: &str = "cpu = ?3, gpu = ?4, ram = ?5, ram_total = ?6, swap = ?7, \
     swap_total = ?8, load = ?9, temp = ?10, disk = ?11, disk_total = ?12, net_in = ?13, \
     net_out = ?14, net_total_up = ?15, net_total_down = ?16, process = ?17, \
     connections = ?18, connections_udp = ?19";

const GPU_COLUMNS: &str =
    "client, time, device_index, device_name, mem_total, mem_used, utilization, temperature";
const GPU_PLACEHOLDERS: &str = "?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8";
const GPU_UPDATES: &str =
    "device_name = ?4, mem_total = ?5, mem_used = ?6, utilization = ?7, temperature = ?8";

/// Options for one compaction pass.
#[derive(Clone, Debug)]
pub struct CompactionOptions {
    /// Injectable clock; rows older than `now - 4h` are compacted.
    pub now: DateTime<Utc>,
    /// Rows fetched per query batch.
    pub batch_size: usize,
    /// Run `VACUUM` afterwards (Go does this only for SQLite).
    pub vacuum: bool,
}

impl Default for CompactionOptions {
    fn default() -> Self {
        Self {
            now: Utc::now(),
            batch_size: DEFAULT_BATCH_SIZE,
            vacuum: false,
        }
    }
}

impl CompactionOptions {
    pub fn at(now: DateTime<Utc>) -> Self {
        Self {
            now,
            ..Self::default()
        }
    }
}

/// Result of one compaction pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Distinct (client, 15-minute bucket) rows written to `records_long_term`.
    pub record_groups: usize,
    /// Distinct (client, device, bucket) rows written to `gpu_records_long_term`.
    pub gpu_groups: usize,
    /// Raw `records` rows deleted after compaction.
    pub raw_records_deleted: usize,
    /// Raw `gpu_records` rows deleted after compaction.
    pub raw_gpu_records_deleted: usize,
}

/// Start of the 15-minute bucket containing `time`.
pub fn slot_start(time: LocalTime) -> LocalTime {
    let timestamp = time.to_naive().and_utc().timestamp();
    let slot = timestamp - timestamp.rem_euclid(BUCKET_SECONDS);
    let naive = DateTime::from_timestamp(slot, 0)
        .map(|value| value.naive_utc())
        .unwrap_or_else(|| time.to_naive());
    LocalTime::from_naive(naive)
}

/// Linear-interpolated percentile, identical to Go's `getPercentile`.
pub fn percentile(values: &[f64], percentile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let index = (sorted.len() - 1) as f64 * percentile;
    let lower = index as usize;
    if lower >= sorted.len() - 1 {
        return sorted[sorted.len() - 1];
    }
    let fraction = index - lower as f64;
    sorted[lower] + fraction * (sorted[lower + 1] - sorted[lower])
}

fn int_percentile(values: &[i64], percentile_value: f64) -> i64 {
    let floats = values.iter().map(|value| *value as f64).collect::<Vec<_>>();
    percentile(&floats, percentile_value) as i64
}

fn f32_percentile(values: &[f32], percentile_value: f64) -> f32 {
    let floats = values.iter().map(|value| *value as f64).collect::<Vec<_>>();
    percentile(&floats, percentile_value) as f32
}

#[derive(Default)]
struct RecordGroup {
    cpu: Vec<f32>,
    gpu: Vec<f32>,
    ram: Vec<i64>,
    ram_total: Vec<i64>,
    swap: Vec<i64>,
    swap_total: Vec<i64>,
    load: Vec<f32>,
    temp: Vec<f32>,
    disk: Vec<i64>,
    disk_total: Vec<i64>,
    net_in: Vec<i64>,
    net_out: Vec<i64>,
    net_total_up: Vec<i64>,
    net_total_down: Vec<i64>,
    process: Vec<i64>,
    connections: Vec<i64>,
    connections_udp: Vec<i64>,
}

impl RecordGroup {
    fn push(&mut self, record: &Record) {
        self.cpu.push(record.cpu);
        self.gpu.push(record.gpu);
        self.ram.push(record.ram);
        self.ram_total.push(record.ram_total);
        self.swap.push(record.swap);
        self.swap_total.push(record.swap_total);
        self.load.push(record.load);
        self.temp.push(record.temp);
        self.disk.push(record.disk);
        self.disk_total.push(record.disk_total);
        self.net_in.push(record.net_in);
        self.net_out.push(record.net_out);
        self.net_total_up.push(record.net_total_up);
        self.net_total_down.push(record.net_total_down);
        self.process.push(record.process);
        self.connections.push(record.connections);
        self.connections_udp.push(record.connections_udp);
    }

    fn compress(&self, client: &str, slot: LocalTime) -> Record {
        Record {
            client: client.to_owned(),
            time: slot,
            cpu: f32_percentile(&self.cpu, HIGH_PERCENTILE),
            gpu: f32_percentile(&self.gpu, HIGH_PERCENTILE),
            ram: int_percentile(&self.ram, HIGH_PERCENTILE),
            ram_total: int_percentile(&self.ram_total, HIGH_PERCENTILE),
            swap: int_percentile(&self.swap, HIGH_PERCENTILE),
            swap_total: int_percentile(&self.swap_total, HIGH_PERCENTILE),
            load: f32_percentile(&self.load, HIGH_PERCENTILE),
            temp: f32_percentile(&self.temp, HIGH_PERCENTILE),
            disk: int_percentile(&self.disk, HIGH_PERCENTILE),
            disk_total: int_percentile(&self.disk_total, HIGH_PERCENTILE),
            net_in: int_percentile(&self.net_in, NETWORK_PERCENTILE),
            net_out: int_percentile(&self.net_out, NETWORK_PERCENTILE),
            net_total_up: int_percentile(&self.net_total_up, HIGH_PERCENTILE),
            net_total_down: int_percentile(&self.net_total_down, HIGH_PERCENTILE),
            process: int_percentile(&self.process, HIGH_PERCENTILE),
            connections: int_percentile(&self.connections, HIGH_PERCENTILE),
            connections_udp: int_percentile(&self.connections_udp, HIGH_PERCENTILE),
        }
    }
}

#[derive(Default)]
struct GpuGroup {
    mem_total: Vec<i64>,
    mem_used: Vec<i64>,
    utilization: Vec<f32>,
    temperature: Vec<i64>,
}

impl GpuGroup {
    fn push(&mut self, record: &GPURecord) {
        self.mem_total.push(record.mem_total);
        self.mem_used.push(record.mem_used);
        self.utilization.push(record.utilization);
        self.temperature.push(record.temperature);
    }

    fn compress(&self, record: &GPURecord, slot: LocalTime) -> GPURecord {
        GPURecord {
            client: record.client.clone(),
            time: slot,
            device_index: record.device_index,
            device_name: record.device_name.clone(),
            mem_total: int_percentile(&self.mem_total, HIGH_PERCENTILE),
            mem_used: int_percentile(&self.mem_used, HIGH_PERCENTILE),
            utilization: f32_percentile(&self.utilization, HIGH_PERCENTILE),
            temperature: int_percentile(&self.temperature, HIGH_PERCENTILE),
        }
    }
}

/// Compact old live rows into the long-term tables.
pub fn compact(db: &Db, options: &CompactionOptions) -> Result<CompactionReport> {
    let four_hours_ago = LocalTime::from_instant(options.now - TimeDelta::hours(4));
    let five_hours_ago = LocalTime::from_instant(options.now - TimeDelta::hours(RAW_DELETE_HOURS));
    let batch_size = options.batch_size.max(1) as i64;

    let mut record_groups: HashMap<(String, LocalTime), RecordGroup> = HashMap::new();
    let mut gpu_groups: HashMap<(String, i64, String, LocalTime), GpuGroup> = HashMap::new();

    let mut offset = 0;
    loop {
        let batch = db.with_connection(|conn| {
            Ok(read_records_before(
                conn,
                four_hours_ago,
                batch_size,
                offset,
            )?)
        })?;
        if batch.is_empty() {
            break;
        }
        for record in &batch {
            let slot = slot_start(record.time);
            let key = (record.client.clone(), slot);
            record_groups.entry(key).or_default().push(record);
        }
        offset += batch.len() as i64;
        if batch.len() < batch_size as usize {
            break;
        }
    }

    offset = 0;
    loop {
        let batch = db.with_connection(|conn| {
            Ok(read_gpu_records_before(
                conn,
                four_hours_ago,
                batch_size,
                offset,
            )?)
        })?;
        if batch.is_empty() {
            break;
        }
        for record in &batch {
            let slot = slot_start(record.time);
            let key = (
                record.client.clone(),
                record.device_index,
                record.device_name.clone(),
                slot,
            );
            gpu_groups.entry(key).or_default().push(record);
        }
        offset += batch.len() as i64;
        if batch.len() < batch_size as usize {
            break;
        }
    }

    let mut compressed_records = Vec::with_capacity(record_groups.len());
    for ((client, slot), group) in &record_groups {
        compressed_records.push(group.compress(client, *slot));
    }

    let mut compressed_gpus = Vec::with_capacity(gpu_groups.len());
    for (key, group) in &gpu_groups {
        let sample = GPURecord {
            client: key.0.clone(),
            device_index: key.1,
            device_name: key.2.clone(),
            ..GPURecord::default()
        };
        compressed_gpus.push(group.compress(&sample, key.3));
    }

    let (raw_records_deleted, raw_gpu_records_deleted) =
        write_records(db, &compressed_records, &compressed_gpus, five_hours_ago)?;

    if options.vacuum {
        db.vacuum()?;
    }

    Ok(CompactionReport {
        record_groups: record_groups.len(),
        gpu_groups: gpu_groups.len(),
        raw_records_deleted,
        raw_gpu_records_deleted,
    })
}

fn read_records_before(
    conn: &Connection,
    before: LocalTime,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<Record>> {
    let mut statement = conn.prepare(
        "SELECT * FROM records WHERE time < ?1 ORDER BY time ASC, rowid ASC LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(params![before, limit, offset], Record::from_row)?;
    rows.collect()
}

fn read_gpu_records_before(
    conn: &Connection,
    before: LocalTime,
    limit: i64,
    offset: i64,
) -> rusqlite::Result<Vec<GPURecord>> {
    let mut statement = conn.prepare(
        "SELECT * FROM gpu_records WHERE time < ?1 \
         ORDER BY time ASC, device_index ASC, rowid ASC LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(params![before, limit, offset], GPURecord::from_row)?;
    rows.collect()
}

/// Upsert compressed rows and delete raw rows in two transactions.
fn write_records(
    db: &Db,
    records: &[Record],
    gpus: &[GPURecord],
    five_hours_ago: LocalTime,
) -> Result<(usize, usize)> {
    let raw_records_deleted = db.with_connection(|conn| {
        let transaction = conn.unchecked_transaction()?;
        for record in records {
            upsert_record(&transaction, record)?;
        }
        let deleted =
            transaction.execute("DELETE FROM records WHERE time < ?1", [five_hours_ago])?;
        transaction.commit()?;
        Ok(deleted)
    })?;

    let raw_gpu_records_deleted = db.with_connection(|conn| {
        let transaction = conn.unchecked_transaction()?;
        for gpu in gpus {
            upsert_gpu_record(&transaction, gpu)?;
        }
        let deleted =
            transaction.execute("DELETE FROM gpu_records WHERE time < ?1", [five_hours_ago])?;
        transaction.commit()?;
        Ok(deleted)
    })?;

    Ok((raw_records_deleted, raw_gpu_records_deleted))
}

fn upsert_record(transaction: &Connection, record: &Record) -> Result<()> {
    let existing: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM records_long_term WHERE client = ?1 AND time = ?2",
        params![record.client, record.time],
        |row| row.get(0),
    )?;

    if existing > 0 {
        transaction.execute(
            &format!(
                "UPDATE records_long_term SET {RECORD_UPDATES} WHERE client = ?1 AND time = ?2"
            ),
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
    } else {
        transaction.execute(
            &format!(
                "INSERT INTO records_long_term ({RECORD_COLUMNS}) VALUES ({RECORD_PLACEHOLDERS})"
            ),
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
    }
    Ok(())
}

fn upsert_gpu_record(transaction: &Connection, record: &GPURecord) -> Result<()> {
    let existing: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM gpu_records_long_term \
         WHERE client = ?1 AND device_index = ?2 AND time = ?3",
        params![record.client, record.device_index, record.time],
        |row| row.get(0),
    )?;

    if existing > 0 {
        transaction.execute(
            &format!(
                "UPDATE gpu_records_long_term SET {GPU_UPDATES} \
                 WHERE client = ?1 AND device_index = ?2 AND time = ?3"
            ),
            params![
                record.client,
                record.device_index,
                record.time,
                record.device_name,
                record.mem_total,
                record.mem_used,
                record.utilization,
                record.temperature,
            ],
        )?;
    } else {
        transaction.execute(
            &format!(
                "INSERT INTO gpu_records_long_term ({GPU_COLUMNS}) VALUES ({GPU_PLACEHOLDERS})"
            ),
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
    }
    Ok(())
}
