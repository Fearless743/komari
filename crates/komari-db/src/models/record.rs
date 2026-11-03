use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;

/// A client metric sample (`records` / `records_long_term`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    pub client: String,
    pub time: LocalTime,
    pub cpu: f32,
    pub gpu: f32,
    pub ram: i64,
    pub ram_total: i64,
    pub swap: i64,
    pub swap_total: i64,
    pub load: f32,
    pub temp: f32,
    pub disk: i64,
    pub disk_total: i64,
    pub net_in: i64,
    pub net_out: i64,
    pub net_total_up: i64,
    pub net_total_down: i64,
    pub process: i64,
    pub connections: i64,
    pub connections_udp: i64,
}

impl Record {
    pub fn new(client: impl Into<String>) -> Self {
        Self {
            client: client.into(),
            time: LocalTime::now(),
            ..Self::default()
        }
    }
}

impl FromRow for Record {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            client: row.get("client")?,
            time: row.get("time")?,
            cpu: row.get("cpu")?,
            gpu: row.get("gpu")?,
            ram: row.get("ram")?,
            ram_total: row.get("ram_total")?,
            swap: row.get("swap")?,
            swap_total: row.get("swap_total")?,
            load: row.get("load")?,
            temp: row.get("temp")?,
            disk: row.get("disk")?,
            disk_total: row.get("disk_total")?,
            net_in: row.get("net_in")?,
            net_out: row.get("net_out")?,
            net_total_up: row.get("net_total_up")?,
            net_total_down: row.get("net_total_down")?,
            process: row.get("process")?,
            connections: row.get("connections")?,
            connections_udp: row.get("connections_udp")?,
        })
    }
}

/// A per-device GPU metric sample (`gpu_records` / `gpu_records_long_term`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GPURecord {
    pub client: String,
    pub time: LocalTime,
    pub device_index: i64,
    pub device_name: String,
    pub mem_total: i64,
    pub mem_used: i64,
    pub utilization: f32,
    pub temperature: i64,
}

impl FromRow for GPURecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            client: row.get("client")?,
            time: row.get("time")?,
            device_index: row.get("device_index")?,
            device_name: row.get("device_name")?,
            mem_total: row.get("mem_total")?,
            mem_used: row.get("mem_used")?,
            utilization: row.get("utilization")?,
            temperature: row.get("temperature")?,
        })
    }
}
