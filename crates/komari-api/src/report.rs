//! Agent report model, validation and persistence.
//!
//! Field names mirror `common.Report` in the Go backend. Missing sections
//! deserialize to zero values, exactly like `encoding/json` into a struct.

use std::collections::BTreeMap;

use chrono::{Local, SecondsFormat};
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub method: String,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuReport {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub cores: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub arch: String,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub usage: f64,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
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
    /// Stamp the report with the current local time in Go's RFC3339Nano
    /// layout (trailing fractional zeros removed).
    pub fn mark_updated(&mut self) {
        let mut text = Local::now().to_rfc3339_opts(SecondsFormat::Nanos, false);
        if let Some(dot) = text.find('.') {
            let fraction_end = text[dot + 1..]
                .find(|character: char| !character.is_ascii_digit())
                .map(|offset| dot + 1 + offset)
                .unwrap_or(text.len());
            let trimmed = text[dot + 1..fraction_end].trim_end_matches('0').to_owned();
            if trimmed.is_empty() {
                text.replace_range(dot..fraction_end, "");
            } else {
                text.replace_range(dot + 1..fraction_end, &trimmed);
            }
        }
        self.updated_at = Some(text);
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
///
/// Go clamps `cpu.usage < 0.01` to `0.01` *before* `ReportVerify`, so tiny or
/// negative CPU readings never reach the database unchanged.
pub fn save_report(db: &Db, client: &str, report: &Report) -> Result<(), String> {
    let mut report = report.clone();
    if report.cpu.usage < 0.01 {
        report.cpu.usage = 0.01;
    }
    verify_report(&report).map_err(|error| format!("failed to save Record: {error}"))?;

    let now = LocalTime::now();
    let gpu_records = to_gpu_records(client, &report, now);
    if !gpu_records.is_empty() {
        db.records()
            .insert_gpu_batch(&gpu_records)
            .map_err(|error| format!("failed to save GPU record: {error}"))?;
    }

    let record = to_record(client, &report, now);
    db.records()
        .insert(&record)
        .map_err(|error| format!("failed to save Record: {error}"))
}

/// Aggregate buffered reports with the Go `AverageReport` / `AverageGPUReports`
/// top-percentage semantics (0.3 by default).
pub fn aggregate_reports(
    client: &str,
    reports: &[Report],
    time: LocalTime,
    top_percentage: f64,
) -> Report {
    if reports.is_empty() {
        return Report {
            uuid: client.to_owned(),
            ..Report::default()
        };
    }

    let top = |values: &mut Vec<f64>| top_average_f64(values, top_percentage);
    let top_i64 = |values: &mut Vec<i64>| top_average_i64(values, top_percentage);

    let cpu = top(&mut reports.iter().map(|report| report.cpu.usage).collect());
    let load = top(&mut reports.iter().map(|report| report.load.load1).collect());
    let gpu = top(&mut reports
        .iter()
        .map(|report| {
            report
                .gpu
                .as_ref()
                .map(|gpu| gpu.average_usage)
                .unwrap_or(0.0)
        })
        .collect());
    let ram = top_i64(&mut reports.iter().map(|report| report.ram.used).collect());
    let swap = top_i64(&mut reports.iter().map(|report| report.swap.used).collect());
    let disk = top_i64(&mut reports.iter().map(|report| report.disk.used).collect());
    let net_in = top_i64(&mut reports.iter().map(|report| report.network.down).collect());
    let net_out = top_i64(&mut reports.iter().map(|report| report.network.up).collect());
    let net_total_up = top_i64(
        &mut reports
            .iter()
            .map(|report| report.network.total_up)
            .collect(),
    );
    let net_total_down = top_i64(
        &mut reports
            .iter()
            .map(|report| report.network.total_down)
            .collect(),
    );
    let process = top_i64(&mut reports.iter().map(|report| report.process).collect());
    let connections = top_i64(
        &mut reports
            .iter()
            .map(|report| report.connections.tcp)
            .collect(),
    );
    let connections_udp = top_i64(
        &mut reports
            .iter()
            .map(|report| report.connections.udp)
            .collect(),
    );

    // Totals are static capacities; Go reads them from the first element of
    // the (repeatedly sorted) slice. Use the newest sample for deterministic
    // output.
    let latest = reports.last().cloned().unwrap_or_default();
    let gpu_detail = aggregate_gpu_reports(reports, top_percentage).map(|mut detail| {
        detail.average_usage = gpu;
        detail
    });

    Report {
        uuid: client.to_owned(),
        cpu: CpuReport {
            name: latest.cpu.name,
            cores: latest.cpu.cores,
            arch: latest.cpu.arch,
            usage: cpu,
        },
        ram: RamReport {
            total: latest.ram.total,
            used: ram,
        },
        swap: RamReport {
            total: latest.swap.total,
            used: swap,
        },
        load: LoadReport {
            load1: load,
            load5: latest.load.load5,
            load15: latest.load.load15,
        },
        disk: DiskReport {
            total: latest.disk.total,
            used: disk,
        },
        network: NetworkReport {
            up: net_out,
            down: net_in,
            total_up: net_total_up,
            total_down: net_total_down,
        },
        connections: ConnectionsReport {
            tcp: connections,
            udp: connections_udp,
        },
        gpu: gpu_detail,
        uptime: latest.uptime,
        process,
        message: String::new(),
        method: String::new(),
        updated_at: Some(time.to_rfc3339().unwrap_or_default()),
    }
}

fn top_count(len: usize, top_percentage: f64) -> usize {
    if !(0.0..=1.0).contains(&top_percentage) || top_percentage == 0.0 {
        return len;
    }
    let count = (len as f64 * top_percentage) as usize;
    count.max(1)
}

fn top_average_f64(values: &mut [f64], top_percentage: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let count = top_count(values.len(), top_percentage);
    values.sort_by(|left, right| right.total_cmp(left));
    values.iter().take(count).sum::<f64>() / count as f64
}

fn top_average_i64(values: &mut [i64], top_percentage: f64) -> i64 {
    if values.is_empty() {
        return 0;
    }
    let count = top_count(values.len(), top_percentage);
    values.sort_unstable_by(|left, right| right.cmp(left));
    values.iter().take(count).sum::<i64>() / count as i64
}

#[derive(Default)]
struct GpuAggregate {
    name: String,
    memory_total: Vec<i64>,
    memory_used: Vec<i64>,
    utilization: Vec<f64>,
    temperature: Vec<i64>,
}

fn aggregate_gpu_reports(reports: &[Report], top_percentage: f64) -> Option<GpuDetailReport> {
    let mut devices: BTreeMap<usize, GpuAggregate> = BTreeMap::new();
    for report in reports {
        let Some(gpu) = report.gpu.as_ref() else {
            continue;
        };
        for (index, device) in gpu.detailed_info.iter().enumerate() {
            let aggregate = devices.entry(index).or_default();
            if aggregate.name.is_empty() {
                aggregate.name = device.name.clone();
            }
            aggregate.memory_total.push(device.memory_total);
            aggregate.memory_used.push(device.memory_used);
            aggregate.utilization.push(device.utilization);
            aggregate.temperature.push(device.temperature);
        }
    }
    if devices.is_empty() {
        return None;
    }

    let detailed_info = devices
        .into_values()
        .map(|mut device| GpuDeviceInfo {
            name: device.name,
            memory_total: top_average_i64(&mut device.memory_total, top_percentage),
            memory_used: top_average_i64(&mut device.memory_used, top_percentage),
            utilization: top_average_f64(&mut device.utilization, top_percentage),
            temperature: top_average_i64(&mut device.temperature, top_percentage),
        })
        .collect::<Vec<_>>();

    Some(GpuDetailReport {
        count: detailed_info.len() as i64,
        average_usage: detailed_info
            .iter()
            .map(|device| device.utilization)
            .sum::<f64>()
            / detailed_info.len() as f64,
        detailed_info,
    })
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
    fn aggregation_uses_the_top_thirty_percent() {
        let reports = vec![
            Report {
                cpu: CpuReport {
                    usage: 10.0,
                    ..CpuReport::default()
                },
                ..Report::default()
            },
            Report {
                cpu: CpuReport {
                    usage: 20.0,
                    ..CpuReport::default()
                },
                ..Report::default()
            },
            Report {
                cpu: CpuReport {
                    usage: 90.0,
                    ..CpuReport::default()
                },
                ..Report::default()
            },
        ];
        let aggregate = aggregate_reports("client", &reports, LocalTime::now(), 0.3);
        // int(3 * 0.3) == 0, clamped to one sample: the highest CPU value.
        assert!((aggregate.cpu.usage - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn mark_updated_uses_local_rfc3339_nano() {
        let mut report = Report::default();
        report.mark_updated();
        let updated = report.updated_at.as_deref().unwrap();
        assert!(updated.contains('T'));
        assert!(updated.ends_with('Z') || updated.contains('+') || updated.contains('-'));
    }
}
