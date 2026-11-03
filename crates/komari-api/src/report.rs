//! Agent report model, validation and persistence.
//!
//! Field names mirror `common.Report` in the Go backend. Missing sections
//! deserialize to zero values, exactly like `encoding/json` into a struct.

use chrono::{SecondsFormat, Utc};
use komari_db::models::{GPURecord, Record};
use komari_db::{Db, LocalTime};
use serde::{Deserialize, Serialize};

/// One agent sample.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uuid: String,
    pub cpu: CpuReport,
    pub ram: RamReport,
    pub swap: RamReport,
    pub load: LoadReport,
    pub disk: DiskReport,
    pub network: NetworkReport,
    pub connections: ConnectionsReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuDetailReport>,
    pub uptime: i64,
    pub process: i64,
    pub message: String,
    pub method: String,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuReport {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub cores: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub arch: String,
    pub usage: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RamReport {
    pub total: i64,
    pub used: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadReport {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskReport {
    pub total: i64,
    pub used: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkReport {
    pub up: i64,
    pub down: i64,
    #[serde(rename = "totalUp")]
    pub total_up: i64,
    #[serde(rename = "totalDown")]
    pub total_down: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionsReport {
    pub tcp: i64,
    pub udp: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuDetailReport {
    pub count: i64,
    pub average_usage: f64,
    pub detailed_info: Vec<GpuDeviceInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuDeviceInfo {
    pub name: String,
    pub memory_total: i64,
    pub memory_used: i64,
    pub utilization: f64,
    pub temperature: i64,
}

impl Report {
    /// Stamp the report with the current time (Go sets `UpdatedAt` before
    /// caching/saving).
    pub fn mark_updated(&mut self) {
        self.updated_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
    }
}

/// Validate ranges so malformed agents cannot corrupt aggregate queries.
///
/// Mirrors `database/clients.ReportVerify`.
pub fn verify_report(report: &Report) -> Result<(), String> {
    if !(0.0..=100.0).contains(&report.cpu.usage) {
        return Err("CPU.Usage must be between 0 and 100".to_owned());
    }
    if !(0.0..=1000.0).contains(&report.load.load1) {
        return Err(format!(
            "Load.Load1 must be non-negative, got {:.2}",
            report.load.load1
        ));
    }
    for (name, value) in [
        ("CPU.Usage", report.cpu.usage),
        ("Load.Load1", report.load.load1),
    ] {
        if !value.is_finite() {
            return Err(format!("{name} value exceeds float64 range: {value}"));
        }
    }

    for (name, value) in [
        ("Ram.Used", report.ram.used),
        ("Ram.Total", report.ram.total),
        ("Swap.Used", report.swap.used),
        ("Swap.Total", report.swap.total),
        ("Disk.Used", report.disk.used),
        ("Disk.Total", report.disk.total),
        ("Network.Up", report.network.up),
        ("Network.Down", report.network.down),
        ("Network.TotalUp", report.network.total_up),
        ("Network.TotalDown", report.network.total_down),
    ] {
        if value < 0 {
            return Err(format!("{name} must be non-negative, got {value}"));
        }
    }
    if report.process < 0 {
        return Err(format!("Process must be non-negative: {}", report.process));
    }
    if report.connections.tcp < 0 {
        return Err(format!(
            "Connections.TCP must be non-negative: {}",
            report.connections.tcp
        ));
    }
    if report.connections.udp < 0 {
        return Err(format!(
            "Connections.UDP must be non-negative: {}",
            report.connections.udp
        ));
    }
    Ok(())
}

/// Convert a report into the row stored in `records` plus the optional
/// `gpu_records` rows, matching `SaveClientReport`.
fn to_record(client: &str, report: &Report, time: LocalTime) -> Record {
    let gpu = report
        .gpu
        .as_ref()
        .filter(|gpu| !gpu.detailed_info.is_empty())
        .map(|gpu| gpu.average_usage as f32)
        .unwrap_or(0.0);
    Record {
        client: client.to_owned(),
        time,
        cpu: report.cpu.usage as f32,
        gpu,
        ram: report.ram.used,
        ram_total: report.ram.total,
        swap: report.swap.used,
        swap_total: report.swap.total,
        load: report.load.load1 as f32,
        temp: 0.0,
        disk: report.disk.used,
        disk_total: report.disk.total,
        net_in: report.network.down,
        net_out: report.network.up,
        net_total_up: report.network.total_up,
        net_total_down: report.network.total_down,
        process: report.process,
        connections: report.connections.tcp,
        connections_udp: report.connections.udp,
    }
}

fn to_gpu_records(client: &str, report: &Report, time: LocalTime) -> Vec<GPURecord> {
    let Some(gpu) = report.gpu.as_ref() else {
        return Vec::new();
    };
    gpu.detailed_info
        .iter()
        .enumerate()
        .map(|(index, device)| GPURecord {
            client: client.to_owned(),
            time,
            device_index: index as i64,
            device_name: device.name.clone(),
            mem_total: device.memory_total,
            mem_used: device.memory_used,
            utilization: device.utilization as f32,
            temperature: device.temperature,
        })
        .collect()
}

/// Validate and persist a report, returning Go-compatible error text.
pub fn save_report(db: &Db, client: &str, report: &Report) -> Result<(), String> {
    verify_report(report).map_err(|error| format!("failed to save Record: {error}"))?;

    let now = LocalTime::now();
    let gpu_records = to_gpu_records(client, report, now);
    if !gpu_records.is_empty() {
        db.records()
            .insert_gpu_batch(&gpu_records)
            .map_err(|error| format!("failed to save GPU record: {error}"))?;
    }

    let record = to_record(client, report, now);
    db.records()
        .insert(&record)
        .map_err(|error| format!("failed to save Record: {error}"))
}

/// Normalise a report before caching: clamp tiny CPU usage like the Go
/// `SaveClientReport` helper and refresh the timestamp.
pub fn cache_report(mut report: Report) -> Report {
    if report.cpu.usage < 0.01 {
        report.cpu.usage = 0.01;
    }
    report.mark_updated();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_sections_decode_to_zero() {
        let report: Report = serde_json::from_str("{}").unwrap();
        assert_eq!(report, Report::default());
        assert!(verify_report(&report).is_ok());
    }

    #[test]
    fn camel_case_network_fields_are_accepted() {
        let report: Report =
            serde_json::from_str(r#"{"network":{"up":1,"down":2,"totalUp":3,"totalDown":4}}"#)
                .unwrap();
        assert_eq!(report.network.total_up, 3);
        assert_eq!(report.network.total_down, 4);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["network"]["totalUp"],
            serde_json::json!(3)
        );
    }

    #[test]
    fn invalid_ranges_are_rejected() {
        let report = Report {
            cpu: CpuReport {
                usage: 101.0,
                ..CpuReport::default()
            },
            ..Report::default()
        };
        assert!(verify_report(&report).is_err());

        let report = Report {
            cpu: CpuReport {
                usage: 50.0,
                ..CpuReport::default()
            },
            connections: ConnectionsReport {
                tcp: -1,
                ..ConnectionsReport::default()
            },
            ..Report::default()
        };
        assert!(verify_report(&report).is_err());
    }

    #[test]
    fn caching_clamps_tiny_cpu_usage() {
        let cached = cache_report(Report::default());
        assert_eq!(cached.cpu.usage, 0.01);
        assert!(cached.updated_at.is_some());
    }
}
