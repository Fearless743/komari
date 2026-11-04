//! `GET /api/mjpeg_live` — live MJPEG status stream.
//!
//! Port of Go `api/public/mjpeg.go`. The Go handler renders a 1280 px wide
//! status table with the downloaded MiSans font, falling back to the
//! `basicfont` bitmap face when the font cannot be loaded. The wire format is
//! `multipart/x-mixed-replace; boundary=frame`, one JPEG frame per two
//! seconds. Dropping the response body cancels the render loop.
//!
//! Layout, text, colours, progress bars, node ordering and language selection
//! follow the Go implementation. Node data is read from `clients` plus the
//! hub's latest report cache (the Rust equivalent of the guest
//! `common:getNodes` / `common:getNodesLatestStatus` calls); the latest
//! `records` row is used as a fallback for report fields. Hidden clients are
//! always filtered, and IPv4/IPv6 follow the guest `send_ip_addr_to_guest`
//! policy.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Utc};
use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};
use komari_config::keys;
use komari_db::LocalTime;
use komari_db::models::Client;
use serde::Deserialize;

use crate::Report;
use crate::state::ApiState;

// ---------------------------------------------------------------------------
// Go constants
// ---------------------------------------------------------------------------

const FONT_URL: &str = "https://hyperos.mi.com/font-download/MiSans.zip";
const FONT_FILE: &str = "font.ttf";
const FONT_ZIP_ENTRY: &str = "MiSans/ttf/MiSans-Normal.ttf";
const FONT_SAVE_HINT: &str = "./data/font.ttf";

const IMAGE_WIDTH: u32 = 1280;
const HEADER_HEIGHT: u32 = 60;
const ROW_HEIGHT: u32 = 36;
const FOOTER_HEIGHT: u32 = 50;
const PADDING: u32 = 20;
const PROGRESS_BAR_W: u32 = 60;
const PROGRESS_BAR_H: u32 = 16;
const FONT_SIZE: f32 = 14.0;
const TITLE_FONT_SIZE: f32 = 24.0;
const FOOTER_FONT_SIZE: u32 = 12;
const BOUNDARY: &str = "frame";
const JPEG_QUALITY: u8 = 90;

const HEADER_COL_WIDTHS: [u32; 13] = [130, 70, 55, 60, 90, 50, 140, 140, 70, 70, 70, 110, 80];
const BASIC_COL_WIDTHS: [u32; 13] = [130, 50, 55, 60, 70, 50, 120, 110, 70, 70, 70, 110, 80];

const BLACK: Rgb<u8> = Rgb([0, 0, 0]);
const WHITE: Rgb<u8> = Rgb([255, 255, 255]);
const HEADER_BACKGROUND: Rgb<u8> = Rgb([245, 245, 245]);
const ALT_ROW_BACKGROUND: Rgb<u8> = Rgb([250, 250, 250]);
const HEADER_TEXT: Rgb<u8> = Rgb([100, 100, 100]);
const ONLINE_COLOR: Rgb<u8> = Rgb([0, 150, 0]);
const OFFLINE_COLOR: Rgb<u8> = Rgb([200, 0, 0]);
const GRAY_100: Rgb<u8> = Rgb([100, 100, 100]);
const GRAY_128: Rgb<u8> = Rgb([128, 128, 128]);
const GRAY_150: Rgb<u8> = Rgb([150, 150, 150]);
const GRAY_200: Rgb<u8> = Rgb([200, 200, 200]);
const WARNING_COLOR: Rgb<u8> = Rgb([200, 100, 0]);
const BAR_BACKGROUND: Rgb<u8> = Rgb([230, 230, 230]);
const BAR_GREEN: Rgb<u8> = Rgb([76, 175, 80]);
const BAR_YELLOW: Rgb<u8> = Rgb([255, 193, 7]);
const BAR_RED: Rgb<u8> = Rgb([244, 67, 54]);
const DOWNLOAD_BLUE: Rgb<u8> = Rgb([66, 133, 244]);

// ---------------------------------------------------------------------------
// Language packs (Go `langEN` / `langZH`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct LangPack {
    server: &'static str,
    network: &'static str,
    online: &'static str,
    location: &'static str,
    virtualization: &'static str,
    load: &'static str,
    traffic: &'static str,
    speed: &'static str,
    cpu: &'static str,
    memory: &'static str,
    disk: &'static str,
    price: &'static str,
    remaining: &'static str,
    offline: &'static str,
    dual_stack: &'static str,
    #[allow(dead_code)]
    day: &'static str,
    days: &'static str,
    #[allow(dead_code)]
    hour: &'static str,
    hours: &'static str,
    #[allow(dead_code)]
    minute: &'static str,
    minutes: &'static str,
    year: &'static str,
    year_plus: &'static str,
    month: &'static str,
    quarter: &'static str,
    half_year: &'static str,
    free: &'static str,
    free_one_time: &'static str,
    long_term: &'static str,
    expired: &'static str,
    last_update: &'static str,
    powered_by: &'static str,
}

const LANG_EN: LangPack = LangPack {
    server: "Server",
    network: "Network",
    online: "Uptime",
    location: "Loc",
    virtualization: "Virt",
    load: "Load",
    traffic: "Traffic",
    speed: "Speed",
    cpu: "CPU",
    memory: "Mem",
    disk: "Disk",
    price: "Price",
    remaining: "Expire",
    offline: "Offline",
    dual_stack: "Dual",
    day: "d",
    days: "d",
    hour: "h",
    hours: "h",
    minute: "m",
    minutes: "m",
    year: "y",
    year_plus: "y+",
    month: "mo",
    quarter: "q",
    half_year: "6mo",
    free: "Free",
    free_one_time: "Free/Once",
    long_term: "Long-term",
    expired: "Expired",
    last_update: "Last Update: ",
    powered_by: "Powered by Komari Monitor",
};

const LANG_ZH: LangPack = LangPack {
    server: "服务器",
    network: "网络",
    online: "在线",
    location: "位置",
    virtualization: "虚拟化",
    load: "负载",
    traffic: "流量",
    speed: "网速",
    cpu: "CPU",
    memory: "内存",
    disk: "磁盘",
    price: "价格",
    remaining: "剩余",
    offline: "离线",
    dual_stack: "双栈",
    day: "天",
    days: "天",
    hour: "时",
    hours: "时",
    minute: "分",
    minutes: "分",
    year: "年",
    year_plus: "年+",
    month: "月",
    quarter: "季",
    half_year: "半年",
    free: "免费",
    free_one_time: "免费/一次性",
    long_term: "长期",
    expired: "已过期",
    last_update: "最后更新：",
    powered_by: "Powered by Komari Monitor",
};

fn get_lang_pack(lang: &str) -> LangPack {
    match lang.to_lowercase().as_str() {
        "zh_cn" | "zh-cn" | "zh" => LANG_ZH,
        _ => LANG_EN,
    }
}

// ---------------------------------------------------------------------------
// HTTP handler and multipart framing
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub(crate) struct MjpegQuery {
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    tz_offset: Option<String>,
}

#[derive(Clone, Debug)]
struct FrameRequest {
    lang: LangPack,
    tz_offset: Option<i32>,
}

impl FrameRequest {
    fn from_query(query: &MjpegQuery) -> Self {
        let lang = query.lang.as_deref().unwrap_or("en");
        let tz_offset = query
            .tz_offset
            .as_deref()
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse::<i32>().ok());
        Self {
            lang: get_lang_pack(lang),
            tz_offset,
        }
    }
}

pub(crate) async fn mjpeg_live(
    State(state): State<ApiState>,
    Query(query): Query<MjpegQuery>,
) -> Response {
    let interval = state.config().mjpeg_frame_interval;
    let request = FrameRequest::from_query(&query);

    // `unfold` renders on demand: when the client disconnects the body is
    // dropped and the stream (including its pending sleep) is cancelled, so no
    // background task keeps rendering.
    let stream = futures_util::stream::unfold(
        (state, request, true),
        move |(state, request, first)| async move {
            if !first {
                tokio::time::sleep(interval).await;
            }
            let frame = frame_bytes(&state, &request);
            Some((Ok::<Bytes, std::io::Error>(frame), (state, request, false)))
        },
    );

    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("multipart/x-mixed-replace; boundary=frame"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    response
}

/// One multipart part containing a single JPEG frame.
fn frame_bytes(state: &ApiState, request: &FrameRequest) -> Bytes {
    let Some(jpeg) = render_jpeg(state, request) else {
        return Bytes::new();
    };
    let mut frame = Vec::with_capacity(jpeg.len() + 96);
    frame.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    frame.extend_from_slice(b"Content-Type: image/jpeg\r\nContent-Length: ");
    frame.extend_from_slice(jpeg.len().to_string().as_bytes());
    frame.extend_from_slice(b"\r\n\r\n");
    frame.extend_from_slice(&jpeg);
    frame.extend_from_slice(b"\r\n");
    Bytes::from(frame)
}

fn render_jpeg(state: &ApiState, request: &FrameRequest) -> Option<Vec<u8>> {
    let image = render_frame(state, request);
    encode_jpeg(&image)
}

fn encode_jpeg(image: &RgbImage) -> Option<Vec<u8>> {
    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY);
    encoder.encode_image(image).ok()?;
    Some(jpeg)
}

// ---------------------------------------------------------------------------
// Font bootstrap: data/font.ttf → MiSans download → bitmap fallback
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FontEntry {
    status: FontStatus,
    progress: Option<DownloadProgress>,
    downloading: bool,
}

#[derive(Default)]
enum FontStatus {
    #[default]
    Unloaded,
    Ready(Arc<LoadedFont>),
    Failed(String),
}

struct LoadedFont {
    font: FontVec,
}

#[derive(Clone, Copy, Debug, Default)]
struct DownloadProgress {
    total: i64,
    downloaded: i64,
    speed: f64,
}

impl DownloadProgress {
    fn percentage(self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.downloaded as f64 / self.total as f64 * 100.0
        }
    }

    fn remaining_seconds(self) -> i64 {
        if self.speed <= 0.0 {
            0
        } else {
            ((self.total - self.downloaded) as f64 / self.speed) as i64
        }
    }
}

enum FontSnapshot {
    Ready(Arc<LoadedFont>),
    Downloading(DownloadProgress),
    Failed(String),
    Pending,
}

static FONT_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<FontEntry>>>>> = OnceLock::new();

fn font_registry() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<FontEntry>>>> {
    FONT_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn font_entry(path: &Path) -> Arc<Mutex<FontEntry>> {
    let mut registry = font_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(FontEntry::default())))
        .clone()
}

/// Resolve the font for `data_dir`, starting the Go-compatible background
/// download when the file is missing and downloads are enabled.
fn font_snapshot(data_dir: &Path, download_enabled: bool) -> FontSnapshot {
    let path = data_dir.join(FONT_FILE);
    let entry = font_entry(&path);
    let mut guard = entry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if matches!(&guard.status, FontStatus::Unloaded) && !guard.downloading {
        if path.is_file() {
            match load_font_file(&path) {
                Ok(font) => guard.status = FontStatus::Ready(Arc::new(font)),
                Err(error) => guard.status = FontStatus::Failed(error),
            }
        } else if download_enabled {
            guard.downloading = true;
            guard.progress = Some(DownloadProgress::default());
            drop(guard);
            spawn_font_download(path, entry);
            return FontSnapshot::Downloading(DownloadProgress::default());
        } else {
            guard.status = FontStatus::Failed("font file not found".to_owned());
        }
    }

    match &guard.status {
        FontStatus::Ready(font) => FontSnapshot::Ready(Arc::clone(font)),
        FontStatus::Failed(error) => FontSnapshot::Failed(error.clone()),
        FontStatus::Unloaded => match guard.progress {
            Some(progress) if guard.downloading => FontSnapshot::Downloading(progress),
            _ => FontSnapshot::Pending,
        },
    }
}

fn spawn_font_download(path: PathBuf, entry: Arc<Mutex<FontEntry>>) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        let mut guard = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.downloading = false;
        guard.progress = None;
        guard.status = FontStatus::Failed("no Tokio runtime for font download".to_owned());
        return;
    };

    handle.spawn(async move {
        let result = download_font(&path, &entry).await;
        let mut guard = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.downloading = false;
        guard.progress = None;
        match result {
            Ok(()) => match load_font_file(&path) {
                Ok(font) => {
                    guard.status = FontStatus::Ready(Arc::new(font));
                    tracing::info!("[MJPEG] Font loaded successfully");
                }
                Err(error) => {
                    tracing::warn!(%error, "[MJPEG] Failed to load downloaded font");
                    guard.status = FontStatus::Failed(error);
                }
            },
            Err(error) => {
                tracing::warn!(%error, "[MJPEG] Font download failed");
                guard.status = FontStatus::Failed(error);
            }
        }
    });
}

fn load_font_file(path: &Path) -> Result<LoadedFont, String> {
    let data = std::fs::read(path).map_err(|error| format!("failed to read font file: {error}"))?;
    let font =
        FontVec::try_from_vec(data).map_err(|error| format!("failed to parse font: {error}"))?;
    Ok(LoadedFont { font })
}

async fn download_font(path: &Path, entry: &Arc<Mutex<FontEntry>>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create directory: {error}"))?;
    }

    let mut response = reqwest::get(FONT_URL)
        .await
        .map_err(|error| format!("failed to download font: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "font download failed with status {}",
            response.status().as_u16()
        ));
    }

    let total = response.content_length().unwrap_or(0) as i64;
    {
        let mut guard = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.progress = Some(DownloadProgress {
            total,
            ..DownloadProgress::default()
        });
    }

    let mut bytes = Vec::with_capacity(total.max(0) as usize);
    let mut last_update = std::time::Instant::now();
    let mut last_bytes = 0i64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to download font data: {error}"))?
    {
        bytes.extend_from_slice(&chunk);
        let mut guard = entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(progress) = guard.progress.as_mut() {
            progress.downloaded += chunk.len() as i64;
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(last_update).as_secs_f64();
            if elapsed >= 1.0 {
                progress.speed = (progress.downloaded - last_bytes) as f64 / elapsed;
                last_update = now;
                last_bytes = progress.downloaded;
            }
        }
    }

    let font_data = extract_font_from_zip(&bytes)?;
    std::fs::write(path, font_data)
        .map_err(|error| format!("failed to save font file: {error}"))?;
    Ok(())
}

/// Extract `MiSans/ttf/MiSans-Normal.ttf` from the MiSans zip archive.
fn extract_font_from_zip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("failed to open zip: {error}"))?;

    let mut found = None;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| format!("failed to open font in zip: {error}"))?;
        if file.name() == FONT_ZIP_ENTRY || file.name().ends_with("MiSans-Normal.ttf") {
            found = Some(index);
            break;
        }
    }

    let index = found.ok_or_else(|| "font file not found in zip".to_owned())?;
    let mut file = archive
        .by_index(index)
        .map_err(|error| format!("failed to open font in zip: {error}"))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|error| format!("failed to read font from zip: {error}"))?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// Rendering pipeline
// ---------------------------------------------------------------------------

fn render_frame(state: &ApiState, request: &FrameRequest) -> RgbImage {
    let snapshot = font_snapshot(&state.data_dir(), state.config().mjpeg_font_download);
    match snapshot {
        FontSnapshot::Ready(font) => match load_table_data(state) {
            Ok(table) => render_status_table(
                &table.nodes,
                &table.statuses,
                &table.site_name,
                request,
                &font,
            ),
            Err(error) => render_error(Some(&error)),
        },
        FontSnapshot::Downloading(progress) => render_download_progress(progress),
        // Go falls back to the bitmap table (and keeps an English language
        // pack) when the custom font failed to load.
        FontSnapshot::Failed(error) => match load_table_data(state) {
            Ok(table) => {
                tracing::debug!(%error, "MJPEG using bitmap font fallback");
                render_status_table_basic(&table.nodes, &table.statuses, &table.site_name, request)
            }
            Err(error) => render_error(Some(&error)),
        },
        FontSnapshot::Pending => render_error(None),
    }
}

struct TableData {
    nodes: Vec<NodeInfo>,
    statuses: HashMap<String, NodeStatus>,
    site_name: String,
}

#[derive(Clone, Debug, Default)]
struct NodeInfo {
    uuid: String,
    name: String,
    ipv4: String,
    ipv6: String,
    region: String,
    virtualization: String,
    price: f64,
    billing_cycle: i64,
    currency: String,
    expired_at: LocalTime,
    auto_renewal: bool,
    weight: i64,
}

impl NodeInfo {
    fn from_client(client: &Client) -> Self {
        Self {
            uuid: client.uuid.clone(),
            name: client.name.clone(),
            ipv4: client.ipv4.clone(),
            ipv6: client.ipv6.clone(),
            region: client.region.clone(),
            virtualization: client.virtualization.clone(),
            price: client.price,
            billing_cycle: client.billing_cycle,
            currency: client.currency.clone(),
            expired_at: client.expired_at,
            auto_renewal: client.auto_renewal,
            weight: client.weight,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct NodeStatus {
    online: bool,
    uptime: i64,
    load: f32,
    cpu: f32,
    ram: i64,
    ram_total: i64,
    disk: i64,
    disk_total: i64,
    net_total_up: i64,
    net_total_down: i64,
    net_in: i64,
    net_out: i64,
}

impl NodeStatus {
    fn from_report(report: &Report, online: bool) -> Self {
        Self {
            online,
            uptime: report.uptime,
            load: report.load.load1 as f32,
            cpu: report.cpu.usage as f32,
            ram: report.ram.used,
            ram_total: report.ram.total,
            disk: report.disk.used,
            disk_total: report.disk.total,
            net_total_up: report.network.total_up,
            net_total_down: report.network.total_down,
            net_in: report.network.down,
            net_out: report.network.up,
        }
    }

    fn from_record(record: &komari_db::models::Record, online: bool) -> Self {
        Self {
            online,
            uptime: 0,
            load: record.load,
            cpu: record.cpu,
            ram: record.ram,
            ram_total: record.ram_total,
            disk: record.disk,
            disk_total: record.disk_total,
            net_total_up: record.net_total_up,
            net_total_down: record.net_total_down,
            net_in: record.net_in,
            net_out: record.net_out,
        }
    }
}

fn load_table_data(state: &ApiState) -> Result<TableData, String> {
    // Go's MJPEG handler calls `common:getNodes` as a guest. The guest path
    // hides private clients and only exposes addresses when
    // `send_ip_addr_to_guest` is enabled (masked).
    let send_ip = state
        .db
        .config()
        .get_as::<bool>(keys::SEND_IP_ADDR_TO_GUEST_KEY)
        .ok()
        .flatten()
        .unwrap_or(false);

    let clients = state
        .db
        .clients()
        .list()
        .map_err(|error| format!("Failed to retrieve client information: {error}"))?;

    let mut nodes = Vec::new();
    for mut client in clients {
        if client.hidden {
            continue;
        }
        if send_ip {
            client.ipv4 = mask_ipv4(&client.ipv4);
            client.ipv6 = mask_ipv6(&client.ipv6);
        } else {
            client.ipv4.clear();
            client.ipv6.clear();
        }
        nodes.push(NodeInfo::from_client(&client));
    }

    // Go sorts by weight (ascending) and then by name.
    nodes.sort_by(|left, right| {
        left.weight
            .cmp(&right.weight)
            .then_with(|| left.name.cmp(&right.name))
    });

    let statuses = fetch_statuses(state, &nodes);
    let site_name = state
        .db
        .config()
        .get_as::<String>(keys::SITENAME_KEY)
        .ok()
        .flatten()
        .unwrap_or_else(|| "Komari Monitor".to_owned());

    Ok(TableData {
        nodes,
        statuses,
        site_name,
    })
}

fn fetch_statuses(state: &ApiState, nodes: &[NodeInfo]) -> HashMap<String, NodeStatus> {
    // `common:getNodesLatestStatus` reads the hub's latest report cache and
    // the online UUID set. The `records` row is only a fallback for the same
    // fields; it never makes an offline node look online.
    let latest = state.hub.latest_reports();
    let mut statuses = HashMap::new();
    for node in nodes {
        if let Some(report) = latest.get(&node.uuid) {
            statuses.insert(
                node.uuid.clone(),
                NodeStatus::from_report(report, state.hub.is_online(&node.uuid)),
            );
        } else if let Ok(Some(record)) = state.db.records().latest(&node.uuid) {
            statuses.insert(node.uuid.clone(), NodeStatus::from_record(&record, false));
        }
    }
    statuses
}

fn mask_ipv4(ip: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    format!("{}.*.*.*", ip.split('.').next().unwrap_or(""))
}

fn mask_ipv6(ip: &str) -> String {
    if ip.is_empty() {
        return String::new();
    }
    format!("{}:*:*:*:*:*:*:*", ip.split(':').next().unwrap_or(""))
}

fn table_height(row_count: usize, basic_font: bool) -> u32 {
    let rows = row_count.max(1) as u32;
    let extra = if basic_font { 30 } else { 0 };
    HEADER_HEIGHT + ROW_HEIGHT + rows * ROW_HEIGHT + FOOTER_HEIGHT + PADDING * 2 + extra
}

fn render_status_table(
    nodes: &[NodeInfo],
    statuses: &HashMap<String, NodeStatus>,
    site_name: &str,
    request: &FrameRequest,
    font: &LoadedFont,
) -> RgbImage {
    let lp = request.lang;
    let height = table_height(nodes.len(), false);
    let mut image = RgbImage::from_pixel(IMAGE_WIDTH, height, WHITE);
    let mut y = PADDING;

    draw_ttf_text(
        &mut image,
        font,
        TITLE_FONT_SIZE,
        (PADDING + 5) as i32,
        (y + TITLE_FONT_SIZE as u32) as i32,
        site_name,
        BLACK,
    );
    y += HEADER_HEIGHT;

    let headers = [
        lp.server,
        lp.network,
        lp.online,
        lp.location,
        lp.virtualization,
        lp.load,
        lp.traffic,
        lp.speed,
        lp.cpu,
        lp.memory,
        lp.disk,
        lp.price,
        lp.remaining,
    ];

    fill_rect(
        &mut image,
        PADDING as i32,
        y as i32,
        IMAGE_WIDTH - PADDING * 2,
        ROW_HEIGHT,
        HEADER_BACKGROUND,
    );
    let mut x = PADDING + 5;
    for (index, header) in headers.iter().enumerate() {
        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            (y + ROW_HEIGHT / 2 + FONT_SIZE as u32 / 3) as i32,
            header,
            HEADER_TEXT,
        );
        x += HEADER_COL_WIDTHS[index];
    }
    y += ROW_HEIGHT;

    for (index, node) in nodes.iter().enumerate() {
        if index % 2 == 1 {
            fill_rect(
                &mut image,
                PADDING as i32,
                y as i32,
                IMAGE_WIDTH - PADDING * 2,
                ROW_HEIGHT,
                ALT_ROW_BACKGROUND,
            );
        }

        let status = statuses.get(&node.uuid);
        x = PADDING + 5;
        let text_y = (y + ROW_HEIGHT / 2 + FONT_SIZE as u32 / 3) as i32;

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &truncate_string(&node.name, 14),
            BLACK,
        );
        x += HEADER_COL_WIDTHS[0];

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &get_network_type(&node.ipv4, &node.ipv6, lp),
            BLACK,
        );
        x += HEADER_COL_WIDTHS[1];

        if let Some(status) = status.filter(|status| status.online) {
            draw_ttf_text(
                &mut image,
                font,
                FONT_SIZE,
                x as i32,
                text_y,
                &format_uptime(status.uptime, lp),
                ONLINE_COLOR,
            );
        } else {
            draw_ttf_text(
                &mut image,
                font,
                FONT_SIZE,
                x as i32,
                text_y,
                lp.offline,
                OFFLINE_COLOR,
            );
        }
        x += HEADER_COL_WIDTHS[2];

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &get_region_short(&node.region),
            BLACK,
        );
        x += HEADER_COL_WIDTHS[3];

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &truncate_string(&node.virtualization, 12),
            BLACK,
        );
        x += HEADER_COL_WIDTHS[4];

        if let Some(status) = status.filter(|status| status.online) {
            draw_ttf_text(
                &mut image,
                font,
                FONT_SIZE,
                x as i32,
                text_y,
                &format!("{:.2}", status.load),
                BLACK,
            );
            x += HEADER_COL_WIDTHS[5];

            draw_ttf_text(
                &mut image,
                font,
                FONT_SIZE,
                x as i32,
                text_y,
                &format!(
                    "↑{}/↓{}",
                    format_bytes(status.net_total_up),
                    format_bytes(status.net_total_down)
                ),
                BLACK,
            );
            x += HEADER_COL_WIDTHS[6];

            draw_ttf_text(
                &mut image,
                font,
                FONT_SIZE,
                x as i32,
                text_y,
                &format!(
                    "↑{}/↓{}",
                    format_bytes(status.net_out),
                    format_bytes(status.net_in)
                ),
                BLACK,
            );
            x += HEADER_COL_WIDTHS[7];

            draw_progress_bar(
                &mut image,
                Some(font),
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                status.cpu as f64,
            );
            x += HEADER_COL_WIDTHS[8];

            let memory_percent = if status.ram_total > 0 {
                status.ram as f64 / status.ram_total as f64 * 100.0
            } else {
                0.0
            };
            draw_progress_bar(
                &mut image,
                Some(font),
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                memory_percent,
            );
            x += HEADER_COL_WIDTHS[9];

            let disk_percent = if status.disk_total > 0 {
                status.disk as f64 / status.disk_total as f64 * 100.0
            } else {
                0.0
            };
            draw_progress_bar(
                &mut image,
                Some(font),
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                disk_percent,
            );
            x += HEADER_COL_WIDTHS[10];
        } else {
            for width in &HEADER_COL_WIDTHS[5..11] {
                draw_ttf_text(&mut image, font, FONT_SIZE, x as i32, text_y, "-", GRAY_150);
                x += width;
            }
        }

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &format_price(node.price, node.billing_cycle, &node.currency, lp),
            BLACK,
        );
        x += HEADER_COL_WIDTHS[11];

        draw_ttf_text(
            &mut image,
            font,
            FONT_SIZE,
            x as i32,
            text_y,
            &format_remaining(node.expired_at, node.auto_renewal, Utc::now(), lp),
            BLACK,
        );

        y += ROW_HEIGHT;
    }

    y += 10;
    let updated = format_time_at(Utc::now(), request.tz_offset);
    draw_centered_ttf_text(
        &mut image,
        font,
        FONT_SIZE,
        (IMAGE_WIDTH / 2) as i32,
        (y + FOOTER_FONT_SIZE) as i32,
        &format!("{}{}", lp.last_update, updated),
        GRAY_100,
    );

    y += 25;
    draw_centered_ttf_text(
        &mut image,
        font,
        FONT_SIZE,
        (IMAGE_WIDTH / 2) as i32,
        (y + FOOTER_FONT_SIZE) as i32,
        lp.powered_by,
        GRAY_150,
    );

    image
}

/// Go `renderStatusTableWithBasicFont`: English-only bitmap fallback used when
/// the custom font could not be loaded. The layout is 30 px taller to fit the
/// warning line.
fn render_status_table_basic(
    nodes: &[NodeInfo],
    statuses: &HashMap<String, NodeStatus>,
    site_name: &str,
    request: &FrameRequest,
) -> RgbImage {
    let lp = LANG_EN;
    let height = table_height(nodes.len(), true);
    let mut image = RgbImage::from_pixel(IMAGE_WIDTH, height, WHITE);
    let mut y = PADDING;

    draw_basic_text(
        &mut image,
        (PADDING + 5) as i32,
        (y + TITLE_FONT_SIZE as u32) as i32,
        site_name,
        BLACK,
    );
    y += HEADER_HEIGHT;

    let headers = [
        lp.server,
        lp.network,
        lp.online,
        lp.location,
        lp.virtualization,
        lp.load,
        lp.traffic,
        lp.speed,
        lp.cpu,
        lp.memory,
        lp.disk,
        lp.price,
        lp.remaining,
    ];

    fill_rect(
        &mut image,
        PADDING as i32,
        y as i32,
        IMAGE_WIDTH - PADDING * 2,
        ROW_HEIGHT,
        HEADER_BACKGROUND,
    );
    let mut x = PADDING + 5;
    for (index, header) in headers.iter().enumerate() {
        draw_basic_text(
            &mut image,
            x as i32,
            (y + ROW_HEIGHT / 2 + FONT_SIZE as u32 / 3) as i32,
            header,
            HEADER_TEXT,
        );
        x += BASIC_COL_WIDTHS[index];
    }
    y += ROW_HEIGHT;

    for (index, node) in nodes.iter().enumerate() {
        if index % 2 == 1 {
            fill_rect(
                &mut image,
                PADDING as i32,
                y as i32,
                IMAGE_WIDTH - PADDING * 2,
                ROW_HEIGHT,
                ALT_ROW_BACKGROUND,
            );
        }

        let status = statuses.get(&node.uuid);
        x = PADDING + 5;
        let text_y = (y + ROW_HEIGHT / 2 + FONT_SIZE as u32 / 3) as i32;

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &truncate_string(&node.name, 14),
            BLACK,
        );
        x += BASIC_COL_WIDTHS[0];

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &get_network_type(&node.ipv4, &node.ipv6, lp),
            BLACK,
        );
        x += BASIC_COL_WIDTHS[1];

        if let Some(status) = status.filter(|status| status.online) {
            draw_basic_text(
                &mut image,
                x as i32,
                text_y,
                &format_uptime(status.uptime, lp),
                ONLINE_COLOR,
            );
        } else {
            draw_basic_text(&mut image, x as i32, text_y, lp.offline, OFFLINE_COLOR);
        }
        x += BASIC_COL_WIDTHS[2];

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &get_region_short(&node.region),
            BLACK,
        );
        x += BASIC_COL_WIDTHS[3];

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &truncate_string(&node.virtualization, 8),
            BLACK,
        );
        x += BASIC_COL_WIDTHS[4];

        if let Some(status) = status.filter(|status| status.online) {
            draw_basic_text(
                &mut image,
                x as i32,
                text_y,
                &format!("{:.2}", status.load),
                BLACK,
            );
            x += BASIC_COL_WIDTHS[5];

            draw_basic_text(
                &mut image,
                x as i32,
                text_y,
                &format!(
                    "u{}/d{}",
                    format_bytes(status.net_total_up),
                    format_bytes(status.net_total_down)
                ),
                BLACK,
            );
            x += BASIC_COL_WIDTHS[6];

            draw_basic_text(
                &mut image,
                x as i32,
                text_y,
                &format!(
                    "u{}/d{}",
                    format_bytes(status.net_out),
                    format_bytes(status.net_in)
                ),
                BLACK,
            );
            x += BASIC_COL_WIDTHS[7];

            draw_progress_bar(
                &mut image,
                None,
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                status.cpu as f64,
            );
            x += BASIC_COL_WIDTHS[8];

            let memory_percent = if status.ram_total > 0 {
                status.ram as f64 / status.ram_total as f64 * 100.0
            } else {
                0.0
            };
            draw_progress_bar(
                &mut image,
                None,
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                memory_percent,
            );
            x += BASIC_COL_WIDTHS[9];

            let disk_percent = if status.disk_total > 0 {
                status.disk as f64 / status.disk_total as f64 * 100.0
            } else {
                0.0
            };
            draw_progress_bar(
                &mut image,
                None,
                x as i32,
                (y + (ROW_HEIGHT - PROGRESS_BAR_H) / 2) as i32,
                PROGRESS_BAR_W,
                PROGRESS_BAR_H,
                disk_percent,
            );
            x += BASIC_COL_WIDTHS[10];
        } else {
            for width in &BASIC_COL_WIDTHS[5..11] {
                draw_basic_text(&mut image, x as i32, text_y, "-", GRAY_150);
                x += width;
            }
        }

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &format_price(node.price, node.billing_cycle, &node.currency, lp),
            BLACK,
        );
        x += BASIC_COL_WIDTHS[11];

        draw_basic_text(
            &mut image,
            x as i32,
            text_y,
            &format_remaining(node.expired_at, node.auto_renewal, Utc::now(), lp),
            BLACK,
        );

        y += ROW_HEIGHT;
    }

    y += 10;
    let updated = format_time_at(Utc::now(), request.tz_offset);
    draw_centered_basic_text(
        &mut image,
        (IMAGE_WIDTH / 2) as i32,
        (y + FOOTER_FONT_SIZE) as i32,
        &format!("{}{}", lp.last_update, updated),
        GRAY_100,
    );

    y += 25;
    draw_centered_basic_text(
        &mut image,
        (IMAGE_WIDTH / 2) as i32,
        (y + FOOTER_FONT_SIZE) as i32,
        lp.powered_by,
        GRAY_150,
    );

    y += 20;
    let warning = format!("Warning: Failed to load custom font. Save TTF to {FONT_SAVE_HINT}");
    draw_centered_basic_text(
        &mut image,
        (IMAGE_WIDTH / 2) as i32,
        (y + FOOTER_FONT_SIZE) as i32,
        &warning,
        WARNING_COLOR,
    );

    image
}

fn render_download_progress(progress: DownloadProgress) -> RgbImage {
    let width = 600;
    let height = 200;
    let mut image = RgbImage::from_pixel(width, height, WHITE);

    let mut y = 60;
    draw_centered_basic_text(
        &mut image,
        (width / 2) as i32,
        y,
        "Preparing MJPEG Stream",
        BLACK,
    );

    y += 40;
    draw_centered_basic_text(
        &mut image,
        (width / 2) as i32,
        y,
        &format!(
            "Downloading font {} / {}",
            format_bytes(progress.downloaded),
            format_bytes(progress.total)
        ),
        GRAY_128,
    );

    y += 30;
    draw_centered_basic_text(
        &mut image,
        (width / 2) as i32,
        y,
        &format!(
            "Speed {}/s, {}s remaining",
            format_bytes(progress.speed as i64),
            progress.remaining_seconds()
        ),
        GRAY_128,
    );

    let bar_width = 400;
    let bar_height = 20;
    let bar_x = (width - bar_width) / 2;
    let bar_y = y + 20;
    fill_rect(
        &mut image,
        bar_x as i32,
        bar_y,
        bar_width,
        bar_height,
        GRAY_200,
    );
    let fill = (bar_width as f64 * progress.percentage() / 100.0) as i64;
    if fill > 0 {
        fill_rect(
            &mut image,
            bar_x as i32,
            bar_y,
            fill.min(bar_width as i64) as u32,
            bar_height,
            DOWNLOAD_BLUE,
        );
    }

    image
}

fn render_error(error: Option<&str>) -> RgbImage {
    let width = 800;
    let height = 250;
    let mut image = RgbImage::from_pixel(width, height, WHITE);

    let mut y = 60;
    draw_centered_basic_text(
        &mut image,
        (width / 2) as i32,
        y,
        "Preparing MJPEG Stream...",
        BLACK,
    );

    if let Some(error) = error {
        y += 40;
        draw_centered_basic_text(
            &mut image,
            (width / 2) as i32,
            y,
            "Error:",
            Rgb([200, 0, 0]),
        );
        y += 25;
        let message = truncate_error(error);
        draw_centered_basic_text(
            &mut image,
            (width / 2) as i32,
            y,
            &message,
            Rgb([150, 0, 0]),
        );
        y += 35;
        draw_centered_basic_text(
            &mut image,
            (width / 2) as i32,
            y,
            "Please check ./data/font.ttf or server logs",
            GRAY_100,
        );
    }

    image
}

fn truncate_error(message: &str) -> String {
    let runes: Vec<char> = message.chars().collect();
    if runes.len() > 80 {
        let mut truncated: String = runes[..77].iter().collect();
        truncated.push_str("...");
        truncated
    } else {
        message.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

fn ttf_text_width(font: &LoadedFont, size: f32, text: &str) -> i32 {
    let scaled = font.font.as_scaled(PxScale::from(size));
    let mut width = 0.0f32;
    let mut previous = None;
    for character in text.chars() {
        let glyph = scaled.glyph_id(character);
        if let Some(previous) = previous {
            width += scaled.kern(previous, glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(glyph);
    }
    width.ceil() as i32
}

fn draw_ttf_text(
    image: &mut RgbImage,
    font: &LoadedFont,
    size: f32,
    x: i32,
    baseline_y: i32,
    text: &str,
    color: Rgb<u8>,
) {
    let scaled = font.font.as_scaled(PxScale::from(size));
    let mut cursor = x as f32;
    let mut previous = None;
    for character in text.chars() {
        let glyph_id = scaled.glyph_id(character);
        if let Some(previous) = previous {
            cursor += scaled.kern(previous, glyph_id);
        }
        let glyph = glyph_id.with_scale_and_position(size, point(cursor, baseline_y as f32));
        if let Some(outline) = font.font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, coverage| {
                blend_pixel(
                    image,
                    bounds.min.x as i32 + gx as i32,
                    bounds.min.y as i32 + gy as i32,
                    color,
                    coverage,
                );
            });
        }
        cursor += scaled.h_advance(glyph_id);
        previous = Some(glyph_id);
    }
}

fn draw_centered_ttf_text(
    image: &mut RgbImage,
    font: &LoadedFont,
    size: f32,
    center_x: i32,
    baseline_y: i32,
    text: &str,
    color: Rgb<u8>,
) {
    let width = ttf_text_width(font, size, text);
    draw_ttf_text(
        image,
        font,
        size,
        center_x - width / 2,
        baseline_y,
        text,
        color,
    );
}

/// `basicfont.Face7x13` equivalent: 5x7 glyphs in a 7 px cell, baseline 11 px
/// below the top of the cell.
const BASIC_ADVANCE: i32 = 7;
const BASIC_ASCENT: i32 = 11;

fn basic_text_width(text: &str) -> i32 {
    text.chars().count() as i32 * BASIC_ADVANCE
}

fn draw_basic_text(image: &mut RgbImage, x: i32, baseline_y: i32, text: &str, color: Rgb<u8>) {
    let top = baseline_y - BASIC_ASCENT;
    let mut cursor = x;
    for character in text.chars() {
        let rows = glyph_rows(character.to_ascii_uppercase());
        for (row, bits) in rows.iter().enumerate() {
            for (column, bit) in bits.chars().enumerate() {
                if bit == '#' {
                    fill_rect(image, cursor + column as i32, top + row as i32, 1, 1, color);
                }
            }
        }
        cursor += BASIC_ADVANCE;
    }
}

fn draw_centered_basic_text(
    image: &mut RgbImage,
    center_x: i32,
    baseline_y: i32,
    text: &str,
    color: Rgb<u8>,
) {
    let width = basic_text_width(text);
    draw_basic_text(image, center_x - width / 2, baseline_y, text, color);
}

fn bar_color(percent: f64) -> Rgb<u8> {
    if percent < 50.0 {
        BAR_GREEN
    } else if percent < 80.0 {
        BAR_YELLOW
    } else {
        BAR_RED
    }
}

fn bar_fill_width(width: u32, percent: f64) -> u32 {
    let fill = (width as f64 * percent / 100.0) as i64;
    fill.clamp(0, width as i64) as u32
}

fn draw_progress_bar(
    image: &mut RgbImage,
    font: Option<&LoadedFont>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    percent: f64,
) {
    fill_rect(image, x, y, width, height, BAR_BACKGROUND);
    let fill = bar_fill_width(width, percent);
    if fill > 0 {
        fill_rect(image, x, y, fill, height, bar_color(percent));
    }

    let text_y = y + height as i32 / 2 + 4;
    if let Some(font) = font {
        let text = format!("{percent:.1}%");
        let text_width = ttf_text_width(font, FONT_SIZE, &text);
        draw_ttf_text(
            image,
            font,
            FONT_SIZE,
            x + (width as i32 - text_width) / 2,
            text_y,
            &text,
            BLACK,
        );
    } else {
        let text = format!("{percent:.0}%");
        let text_width = basic_text_width(&text);
        draw_basic_text(
            image,
            x + (width as i32 - text_width) / 2,
            text_y,
            &text,
            BLACK,
        );
    }
}

fn blend_pixel(image: &mut RgbImage, x: i32, y: i32, color: Rgb<u8>, coverage: f32) {
    if x < 0 || y < 0 {
        return;
    }
    let (width, height) = image.dimensions();
    let (x, y) = (x as u32, y as u32);
    if x >= width || y >= height {
        return;
    }
    let coverage = coverage.clamp(0.0, 1.0);
    let pixel = image.get_pixel_mut(x, y);
    for channel in 0..3 {
        let destination = pixel.0[channel] as f32;
        pixel.0[channel] = (destination * (1.0 - coverage) + color.0[channel] as f32 * coverage)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
}

fn fill_rect(image: &mut RgbImage, x: i32, y: i32, width: u32, height: u32, color: Rgb<u8>) {
    if width == 0 || height == 0 {
        return;
    }
    let (image_width, image_height) = image.dimensions();
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x as i64 + width as i64).clamp(0, image_width as i64) as u32;
    let y1 = (y as i64 + height as i64).clamp(0, image_height as i64) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for py in y0..y1 {
        for px in x0..x1 {
            image.put_pixel(px, py, color);
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers (Go `format*` / `get*` functions)
// ---------------------------------------------------------------------------

fn format_bytes(bytes: i64) -> String {
    const UNIT: i64 = 1024;
    if bytes < UNIT {
        return format!("{bytes}B");
    }

    let mut divisor = UNIT;
    let mut exponent = 0usize;
    let mut value = bytes / UNIT;
    while value >= UNIT {
        divisor *= UNIT;
        exponent += 1;
        value /= UNIT;
    }

    let value = bytes as f64 / divisor as f64;
    let units = ["K", "M", "G", "T", "P"];
    let exponent = exponent.min(units.len() - 1);
    if value >= 100.0 {
        format!("{value:.0}{}", units[exponent])
    } else if value >= 10.0 {
        format!("{value:.1}{}", units[exponent])
    } else {
        format!("{value:.2}{}", units[exponent])
    }
}

fn format_uptime(seconds: i64, lp: LangPack) -> String {
    let days = seconds / 86400;
    if days > 0 {
        return format!("{days}{}", lp.days);
    }
    let hours = seconds / 3600;
    if hours > 0 {
        return format!("{hours}{}", lp.hours);
    }
    format!("{}{}", seconds / 60, lp.minutes)
}

fn format_price(price: f64, cycle: i64, currency: &str, lp: LangPack) -> String {
    if price < 0.0 {
        return lp.free_one_time.to_owned();
    }
    if price == 0.0 {
        return lp.free.to_owned();
    }

    let currency = if currency.is_empty() { "$" } else { currency };
    let cycle = match cycle {
        30 => lp.month.to_owned(),
        90 => lp.quarter.to_owned(),
        180 => lp.half_year.to_owned(),
        360 | 365 | -1 => lp.year.to_owned(),
        value if value > 0 => format!("{value}{}", lp.days),
        _ => lp.year.to_owned(),
    };

    if price == price.trunc() {
        format!("{currency}{}/{cycle}", price as i64)
    } else {
        format!("{currency}{price:.2}/{cycle}")
    }
}

fn format_remaining(
    expired_at: LocalTime,
    auto_renewal: bool,
    now: DateTime<Utc>,
    lp: LangPack,
) -> String {
    if auto_renewal {
        return lp.long_term.to_owned();
    }
    if expired_at.is_zero() || expired_at.to_naive().year() > 2200 {
        return lp.long_term.to_owned();
    }
    let Some(expired) = expired_at.timestamp() else {
        return lp.long_term.to_owned();
    };

    let diff = expired - now.timestamp();
    if diff <= 0 {
        return lp.expired.to_owned();
    }

    let days = diff / 86400;
    if days > 365 {
        let years = days / 365;
        return format!("{years}{}", lp.year_plus);
    }
    format!("{days}{}", lp.days)
}

fn get_network_type(ipv4: &str, ipv6: &str, lp: LangPack) -> String {
    let has_ipv4 = !ipv4.is_empty();
    let has_ipv6 = !ipv6.is_empty();
    if has_ipv4 && has_ipv6 {
        lp.dual_stack.to_owned()
    } else if has_ipv4 {
        "IPv4".to_owned()
    } else if has_ipv6 {
        "IPv6".to_owned()
    } else {
        "-".to_owned()
    }
}

const REGION_MAP: [(&str, &str); 15] = [
    ("🇭🇰", "HK"),
    ("🇨🇳", "CN"),
    ("🇺🇸", "US"),
    ("🇯🇵", "JP"),
    ("🇸🇬", "SG"),
    ("🇰🇷", "KR"),
    ("🇬🇧", "UK"),
    ("🇩🇪", "DE"),
    ("🇫🇷", "FR"),
    ("🇳🇱", "NL"),
    ("🇦🇺", "AU"),
    ("🇨🇦", "CA"),
    ("🇷🇺", "RU"),
    ("🇮🇳", "IN"),
    ("🇹🇼", "TW"),
];

fn get_region_short(region: &str) -> String {
    if region.is_empty() {
        return "-".to_owned();
    }
    for (flag, code) in REGION_MAP {
        if region.contains(flag) {
            return code.to_owned();
        }
    }
    if region.len() <= 4 {
        return region.to_owned();
    }
    let runes: Vec<char> = region.chars().collect();
    if runes.len() > 4 {
        return runes[..4].iter().collect();
    }
    region.to_owned()
}

fn truncate_string(value: &str, max_length: usize) -> String {
    let runes: Vec<char> = value.chars().collect();
    if runes.len() <= max_length {
        return value.to_owned();
    }
    let keep = max_length.saturating_sub(1);
    let mut truncated: String = runes[..keep].iter().collect();
    truncated.push('…');
    truncated
}

fn format_time_at(now: DateTime<Utc>, tz_offset: Option<i32>) -> String {
    if let Some(offset) = tz_offset {
        let shifted = now + ChronoDuration::hours(offset as i64);
        let timezone = if offset >= 0 {
            format!("GMT+{offset}")
        } else {
            format!("GMT{offset}")
        };
        format!("{} {}", shifted.format("%Y-%m-%d %H:%M:%S"), timezone)
    } else {
        let local = now.with_timezone(&chrono::Local);
        format!(
            "{} {}",
            local.format("%Y-%m-%d %H:%M:%S"),
            local.format("%Z")
        )
    }
}

// ---------------------------------------------------------------------------
// Built-in 5x7 bitmap font (basicfont fallback)
// ---------------------------------------------------------------------------

fn glyph_rows(character: char) -> [&'static str; 7] {
    match character {
        'A' => [
            ".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'B' => [
            "####.", "#...#", "#...#", "####.", "#...#", "#...#", "####.",
        ],
        'C' => [
            ".###.", "#...#", "#....", "#....", "#....", "#...#", ".###.",
        ],
        'D' => [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
        'E' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'F' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#....",
        ],
        'G' => [
            ".###.", "#...#", "#....", "#.###", "#...#", "#...#", ".###.",
        ],
        'H' => [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'I' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "#####",
        ],
        'J' => [
            "..###", "...#.", "...#.", "...#.", "...#.", "#..#.", ".##..",
        ],
        'K' => [
            "#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#",
        ],
        'L' => [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
        'M' => [
            "#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#",
        ],
        'N' => [
            "#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#",
        ],
        'O' => [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'P' => [
            "####.", "#...#", "#...#", "####.", "#....", "#....", "#....",
        ],
        'Q' => [
            ".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#",
        ],
        'R' => [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'S' => [
            ".####", "#....", "#....", ".###.", "....#", "....#", "####.",
        ],
        'T' => [
            "#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#..",
        ],
        'U' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'V' => [
            "#...#", "#...#", "#...#", "#...#", "#...#", ".#.#.", "..#..",
        ],
        'W' => [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
        'X' => [
            "#...#", "#...#", ".#.#.", "..#..", ".#.#.", "#...#", "#...#",
        ],
        'Y' => [
            "#...#", "#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..",
        ],
        'Z' => [
            "#####", "....#", "...#.", "..#..", ".#...", "#....", "#####",
        ],
        '0' => [
            ".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###.",
        ],
        '1' => [
            "..#..", ".##..", "..#..", "..#..", "..#..", "..#..", ".###.",
        ],
        '2' => [
            ".###.", "#...#", "....#", "...#.", "..#..", ".#...", "#####",
        ],
        '3' => [
            "####.", "....#", "....#", ".###.", "....#", "....#", "####.",
        ],
        '4' => [
            "...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#.",
        ],
        '5' => [
            "#####", "#....", "####.", "....#", "....#", "#...#", ".###.",
        ],
        '6' => [
            ".###.", "#....", "#....", "####.", "#...#", "#...#", ".###.",
        ],
        '7' => [
            "#####", "....#", "...#.", "..#..", ".#...", ".#...", ".#...",
        ],
        '8' => [
            ".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###.",
        ],
        '9' => [
            ".###.", "#...#", "#...#", ".####", "....#", "....#", ".###.",
        ],
        ' ' => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
        '.' => [
            ".....", ".....", ".....", ".....", ".....", "..##.", "..##.",
        ],
        ':' => [
            ".....", "..##.", "..##.", ".....", "..##.", "..##.", ".....",
        ],
        '-' => [
            ".....", ".....", ".....", "#####", ".....", ".....", ".....",
        ],
        '/' => [
            "....#", "...#.", "...#.", "..#..", ".#...", ".#...", "#....",
        ],
        '%' => [
            "##..#", "##.#.", "..#..", ".#...", "#.##.", "..##.", ".....",
        ],
        '+' => [
            ".....", "..#..", "..#..", "#####", "..#..", "..#..", ".....",
        ],
        '↑' => [
            ".....", "..#..", ".###.", "#.#.#", "..#..", "..#..", ".....",
        ],
        '↓' => [
            ".....", "..#..", "..#..", "#.#.#", ".###.", "..#..", ".....",
        ],
        _ => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use image::GenericImageView;
    use komari_db::{ClientPatch, Db};
    use std::io::Write;
    use std::time::Duration;

    fn test_state() -> ApiState {
        let db = Db::open_in_memory().expect("open in-memory database");
        db.migrate().expect("migrate database");
        let config = crate::ApiConfig {
            presence_sweep_interval: Duration::ZERO,
            report_flush_interval: Duration::ZERO,
            mjpeg_font_download: false,
            ..crate::ApiConfig::default()
        };
        ApiState::with_config(db, config)
    }

    fn node(uuid: &str, name: &str, weight: i64) -> NodeInfo {
        NodeInfo {
            uuid: uuid.to_owned(),
            name: name.to_owned(),
            weight,
            ..NodeInfo::default()
        }
    }

    fn online_status() -> NodeStatus {
        NodeStatus {
            online: true,
            uptime: 3 * 86400 + 5 * 3600,
            load: 0.5,
            cpu: 12.5,
            ram: 512,
            ram_total: 1024,
            disk: 100,
            disk_total: 200,
            net_total_up: 1024 * 1024,
            net_total_down: 2 * 1024 * 1024,
            net_in: 1024,
            net_out: 2048,
        }
    }

    fn count_color(image: &RgbImage, color: Rgb<u8>) -> usize {
        image.pixels().filter(|pixel| **pixel == color).count()
    }

    #[test]
    fn lang_pack_selection_matches_go() {
        assert_eq!(get_lang_pack("en").server, "Server");
        assert_eq!(get_lang_pack("zh_cn").server, "服务器");
        assert_eq!(get_lang_pack("ZH-CN").server, "服务器");
        assert_eq!(get_lang_pack("zh").remaining, "剩余");
        assert_eq!(get_lang_pack("fr").server, "Server");
        assert_eq!(get_lang_pack("").server, "Server");
    }

    #[test]
    fn format_bytes_matches_go_rounding() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(1023), "1023B");
        assert_eq!(format_bytes(1024), "1.00K");
        assert_eq!(format_bytes(1536), "1.50K");
        assert_eq!(format_bytes(10 * 1024), "10.0K");
        assert_eq!(format_bytes(100 * 1024), "100K");
        assert_eq!(format_bytes(1024 * 1024), "1.00M");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00G");
    }

    #[test]
    fn format_price_localizes_cycles() {
        let en = get_lang_pack("en");
        let zh = get_lang_pack("zh");
        assert_eq!(format_price(-1.0, 30, "", en), "Free/Once");
        assert_eq!(format_price(0.0, 30, "", en), "Free");
        assert_eq!(format_price(5.0, 30, "", en), "$5/mo");
        assert_eq!(format_price(5.5, 90, "¥", zh), "¥5.50/季");
        assert_eq!(format_price(1.0, 365, "", en), "$1/y");
        assert_eq!(format_price(1.0, 45, "", en), "$1/45d");
        assert_eq!(format_price(1.0, 0, "", zh), "$1/年");
        assert_eq!(format_price(2.0, 180, "", zh), "$2/半年");
    }

    #[test]
    fn format_remaining_handles_all_states() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let en = get_lang_pack("en");
        let zh = get_lang_pack("zh");
        let now_seconds = now.timestamp();

        assert_eq!(
            format_remaining(LocalTime::zero(), false, now, en),
            "Long-term"
        );
        let expired = LocalTime::from_unix_timestamp(now_seconds - 10).unwrap();
        assert_eq!(format_remaining(expired, false, now, en), "Expired");
        let ten_days = LocalTime::from_unix_timestamp(now_seconds + 10 * 86400 + 3600).unwrap();
        assert_eq!(format_remaining(ten_days, false, now, en), "10d");
        assert_eq!(format_remaining(ten_days, false, now, zh), "10天");
        let two_years = LocalTime::from_unix_timestamp(now_seconds + 800 * 86400).unwrap();
        assert_eq!(format_remaining(two_years, false, now, en), "2y+");
        assert_eq!(format_remaining(ten_days, true, now, en), "Long-term");
    }

    #[test]
    fn progress_bar_boundaries_and_colors() {
        assert_eq!(bar_fill_width(60, -5.0), 0);
        assert_eq!(bar_fill_width(60, 0.0), 0);
        assert_eq!(bar_fill_width(60, 49.9), 29);
        assert_eq!(bar_fill_width(60, 50.0), 30);
        assert_eq!(bar_fill_width(60, 100.0), 60);
        assert_eq!(bar_fill_width(60, 150.0), 60);
        assert_eq!(bar_color(0.0), BAR_GREEN);
        assert_eq!(bar_color(49.9), BAR_GREEN);
        assert_eq!(bar_color(50.0), BAR_YELLOW);
        assert_eq!(bar_color(79.9), BAR_YELLOW);
        assert_eq!(bar_color(80.0), BAR_RED);
        assert_eq!(bar_color(100.0), BAR_RED);
    }

    #[test]
    fn network_and_region_formatting() {
        let en = get_lang_pack("en");
        assert_eq!(get_network_type("1.1.1.1", "::1", en), "Dual");
        assert_eq!(get_network_type("1.1.1.1", "", en), "IPv4");
        assert_eq!(get_network_type("", "::1", en), "IPv6");
        assert_eq!(get_network_type("", "", en), "-");
        assert_eq!(get_region_short("🇭🇰 Hong Kong"), "HK");
        assert_eq!(get_region_short("🇺🇸"), "US");
        assert_eq!(get_region_short(""), "-");
        assert_eq!(get_region_short("US"), "US");
        assert_eq!(get_region_short("ABCDE"), "ABCD");
    }

    #[test]
    fn truncation_uses_runes_and_ellipsis() {
        assert_eq!(truncate_string("short", 14), "short");
        assert_eq!(truncate_string("abcdefghijklmnop", 14), "abcdefghijklm…");
        assert_eq!(truncate_string("服务器名称非常长", 4), "服务器…");
    }

    #[test]
    fn table_height_matches_go_layout() {
        assert_eq!(table_height(0, false), 222);
        assert_eq!(table_height(1, false), 222);
        assert_eq!(table_height(2, false), 258);
        assert_eq!(table_height(1, true), 252);
        assert_eq!(table_height(2, true), 288);
    }

    #[test]
    fn basic_table_renders_online_and_offline_states() {
        let request = FrameRequest {
            lang: LANG_EN,
            tz_offset: None,
        };
        let nodes = vec![node("online", "online-node", 0)];
        let mut statuses = HashMap::new();
        statuses.insert("online".to_owned(), online_status());

        let online = render_status_table_basic(&nodes, &statuses, "Komari", &request);
        assert_eq!(online.dimensions(), (IMAGE_WIDTH, 252));
        assert!(count_color(&online, ONLINE_COLOR) > 0);
        assert_eq!(count_color(&online, OFFLINE_COLOR), 0);

        statuses.insert(
            "online".to_owned(),
            NodeStatus {
                online: false,
                ..online_status()
            },
        );
        let offline = render_status_table_basic(&nodes, &statuses, "Komari", &request);
        assert!(count_color(&offline, OFFLINE_COLOR) > 0);
        assert_eq!(count_color(&offline, ONLINE_COLOR), 0);
    }

    #[test]
    fn basic_table_hides_metrics_for_offline_nodes() {
        let request = FrameRequest {
            lang: LANG_EN,
            tz_offset: None,
        };
        let nodes = vec![node("offline", "offline-node", 0)];
        let mut statuses = HashMap::new();
        statuses.insert(
            "offline".to_owned(),
            NodeStatus {
                online: false,
                ..online_status()
            },
        );

        let image = render_status_table_basic(&nodes, &statuses, "Komari", &request);
        // The offline path draws six "-" placeholders in gray and never fills
        // the CPU/RAM/disk bars with a status colour.
        assert!(count_color(&image, GRAY_150) > 0);
        assert_eq!(count_color(&image, BAR_GREEN), 0);
        assert_eq!(count_color(&image, BAR_YELLOW), 0);
        assert_eq!(count_color(&image, BAR_RED), 0);
    }

    #[test]
    fn download_progress_frame_is_jpeg_compatible() {
        let image = render_download_progress(DownloadProgress {
            total: 1000,
            downloaded: 500,
            speed: 100.0,
        });
        assert_eq!(image.dimensions(), (600, 200));
        let jpeg = encode_jpeg(&image).expect("encode progress frame");
        assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF]);
        let decoded = image::load_from_memory(&jpeg).expect("decode progress frame");
        assert_eq!(decoded.dimensions(), (600, 200));
    }

    #[test]
    fn error_frame_dimensions_and_text() {
        let image = render_error(Some("boom"));
        assert_eq!(image.dimensions(), (800, 250));
        assert!(count_color(&image, Rgb([200, 0, 0])) > 0);
        let truncated = truncate_error(&"x".repeat(100));
        assert_eq!(truncated.chars().count(), 80);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn query_parses_lang_and_tz_offset() {
        let request = FrameRequest::from_query(&MjpegQuery::default());
        assert_eq!(request.lang.server, "Server");
        assert_eq!(request.tz_offset, None);

        let request = FrameRequest::from_query(&MjpegQuery {
            lang: Some("zh_cn".to_owned()),
            tz_offset: Some("8".to_owned()),
        });
        assert_eq!(request.lang.server, "服务器");
        assert_eq!(request.tz_offset, Some(8));

        let request = FrameRequest::from_query(&MjpegQuery {
            lang: None,
            tz_offset: Some("+8".to_owned()),
        });
        assert_eq!(request.tz_offset, Some(8));
        let request = FrameRequest::from_query(&MjpegQuery {
            lang: None,
            tz_offset: Some("-5".to_owned()),
        });
        assert_eq!(request.tz_offset, Some(-5));
        let request = FrameRequest::from_query(&MjpegQuery {
            lang: None,
            tz_offset: Some("abc".to_owned()),
        });
        assert_eq!(request.tz_offset, None);
        let request = FrameRequest::from_query(&MjpegQuery {
            lang: None,
            tz_offset: Some(String::new()),
        });
        assert_eq!(request.tz_offset, None);
    }

    #[test]
    fn time_formatting_uses_gmt_suffix_for_explicit_offsets() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(format_time_at(now, Some(8)), "2026-01-01 08:00:00 GMT+8");
        assert_eq!(format_time_at(now, Some(-5)), "2025-12-31 19:00:00 GMT-5");
        let local = format_time_at(now, None);
        assert!(local.contains(' '));
        assert!(!local.trim().is_empty());
    }

    #[test]
    fn zip_extraction_finds_misans_entry() {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            writer
                .start_file(FONT_ZIP_ENTRY, zip::write::SimpleFileOptions::default())
                .expect("start font entry");
            writer.write_all(b"fake-font").expect("write font");
            writer.finish().expect("finish zip");
        }
        let extracted = extract_font_from_zip(&buffer.into_inner()).expect("extract font");
        assert_eq!(extracted, b"fake-font");
    }

    #[test]
    fn zip_extraction_reports_missing_entry() {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            writer
                .start_file("other.txt", zip::write::SimpleFileOptions::default())
                .expect("start other entry");
            writer.write_all(b"nope").expect("write other");
            writer.finish().expect("finish zip");
        }
        let error = extract_font_from_zip(&buffer.into_inner()).expect_err("missing font");
        assert!(error.contains("font file not found in zip"));
    }

    #[test]
    fn missing_font_file_falls_back_to_basic_table() {
        let directory = tempfile::tempdir().expect("temp dir");
        let snapshot = font_snapshot(directory.path(), false);
        assert!(matches!(snapshot, FontSnapshot::Failed(_)));

        let request = FrameRequest {
            lang: get_lang_pack("zh"),
            tz_offset: None,
        };
        let nodes = vec![node("node", "node", 0)];
        let image = render_status_table_basic(&nodes, &HashMap::new(), "Komari", &request);
        // The bitmap fallback is English-only and 30 px taller than the TTF
        // table, matching Go's `renderStatusTableWithBasicFont`.
        assert_eq!(image.dimensions(), (IMAGE_WIDTH, 252));
        assert!(count_color(&image, WARNING_COLOR) > 0);
    }

    #[test]
    fn corrupt_font_file_reports_failure() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(FONT_FILE);
        std::fs::write(&path, b"not a font").expect("write corrupt font");
        let snapshot = font_snapshot(directory.path(), false);
        match snapshot {
            FontSnapshot::Failed(error) => assert!(error.contains("failed to parse font")),
            _ => panic!("corrupt font should fail"),
        }
    }

    #[test]
    fn table_data_filters_hidden_and_sorts_by_weight_then_name() {
        let state = test_state();
        let alpha = state.db.clients().create_named("alpha").expect("alpha");
        let beta = state.db.clients().create_named("beta").expect("beta");
        let hidden = state.db.clients().create_named("hidden").expect("hidden");
        state
            .db
            .clients()
            .patch(
                &alpha.uuid,
                &ClientPatch {
                    weight: Some(10),
                    ..ClientPatch::default()
                },
            )
            .expect("patch alpha");
        state
            .db
            .clients()
            .patch(
                &beta.uuid,
                &ClientPatch {
                    weight: Some(1),
                    ..ClientPatch::default()
                },
            )
            .expect("patch beta");
        state
            .db
            .clients()
            .patch(
                &hidden.uuid,
                &ClientPatch {
                    hidden: Some(true),
                    ..ClientPatch::default()
                },
            )
            .expect("patch hidden");

        let table = load_table_data(&state).expect("table data");
        assert_eq!(
            table
                .nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta", "alpha"]
        );
    }

    #[test]
    fn guest_address_policy_clears_or_masks_ips() {
        let state = test_state();
        let client = state
            .db
            .clients()
            .create_named("address-node")
            .expect("client");
        state
            .db
            .clients()
            .patch(
                &client.uuid,
                &ClientPatch {
                    ipv4: Some("203.0.113.7".to_owned()),
                    ipv6: Some("2001:db8::1".to_owned()),
                    ..ClientPatch::default()
                },
            )
            .expect("patch addresses");

        let table = load_table_data(&state).expect("table data");
        assert_eq!(table.nodes[0].ipv4, "");
        assert_eq!(table.nodes[0].ipv6, "");
        assert_eq!(
            get_network_type(&table.nodes[0].ipv4, &table.nodes[0].ipv6, LANG_EN),
            "-"
        );

        state
            .db
            .config()
            .set(keys::SEND_IP_ADDR_TO_GUEST_KEY, &true)
            .expect("enable guest ip");
        let table = load_table_data(&state).expect("table data");
        assert_eq!(table.nodes[0].ipv4, "203.*.*.*");
        assert_eq!(table.nodes[0].ipv6, "2001:*:*:*:*:*:*:*");
        assert_eq!(
            get_network_type(&table.nodes[0].ipv4, &table.nodes[0].ipv6, LANG_EN),
            "Dual"
        );
    }

    #[test]
    fn status_fetch_prefers_hub_report_and_marks_online() {
        let state = test_state();
        let client = state.db.clients().create_named("hub-node").expect("client");
        let nodes = vec![NodeInfo::from_client(&client)];
        let report = Report {
            uptime: 1234,
            cpu: crate::CpuReport {
                usage: 42.0,
                ..crate::CpuReport::default()
            },
            ram: crate::RamReport {
                used: 256,
                total: 512,
            },
            ..Report::default()
        };
        state.hub.set_latest_report(&client.uuid, report);

        let statuses = fetch_statuses(&state, &nodes);
        let status = statuses.get(&client.uuid).expect("status");
        assert!(!status.online, "no live connection/presence");
        assert_eq!(status.uptime, 1234);
        assert_eq!(status.cpu, 42.0);
    }

    #[test]
    fn client_status_fallback_never_marks_node_online() {
        let state = test_state();
        let client = state
            .db
            .clients()
            .create_named("record-node")
            .expect("client");
        let mut record = komari_db::models::Record::new(&client.uuid);
        record.cpu = 12.5;
        record.ram = 128;
        record.ram_total = 256;
        state.db.records().insert(&record).expect("insert record");

        let nodes = vec![NodeInfo::from_client(&client)];
        let statuses = fetch_statuses(&state, &nodes);
        let status = statuses.get(&client.uuid).expect("fallback status");
        assert!(!status.online);
        assert_eq!(status.cpu, 12.5);
        assert_eq!(status.ram_total, 256);
    }

    #[test]
    fn title_and_footer_constants_match_go() {
        assert_eq!(LANG_EN.powered_by, "Powered by Komari Monitor");
        assert_eq!(LANG_ZH.powered_by, "Powered by Komari Monitor");
        assert_eq!(LANG_EN.last_update, "Last Update: ");
        assert_eq!(LANG_ZH.last_update, "最后更新：");
        assert_eq!(IMAGE_WIDTH, 1280);
        assert_eq!(HEADER_HEIGHT, 60);
        assert_eq!(ROW_HEIGHT, 36);
        assert_eq!(FOOTER_HEIGHT, 50);
        assert_eq!(PADDING, 20);
        assert_eq!(PROGRESS_BAR_W, 60);
        assert_eq!(PROGRESS_BAR_H, 16);
    }
}
