//! `GET /api/mjpeg_live` — live MJPEG status stream.
//!
//! Go renders a status table from the JSON-RPC node snapshot and an externally
//! downloaded MiSans font. This port keeps the same wire format
//! (`multipart/x-mixed-replace; boundary=frame`, one JPEG per interval) but
//! reads the latest `records` rows directly and draws with a built-in 5x7
//! bitmap font, so no external font or RPC layer is required.

use axum::body::Body;
use axum::extract::State;
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};
use komari_db::LocalTime;
use komari_db::models::Record;

use crate::state::ApiState;

const WIDTH: u32 = 960;
const HEADER_HEIGHT: u32 = 56;
const ROW_HEIGHT: u32 = 40;
const FOOTER_HEIGHT: u32 = 28;
const PADDING: u32 = 12;
const MAX_ROWS: usize = 18;
const BOUNDARY: &str = "frame";

const NAME_X: u32 = 16;
const CPU_BAR_X: u32 = 220;
const RAM_BAR_X: u32 = 420;
const DISK_BAR_X: u32 = 620;
const BAR_WIDTH: u32 = 120;
const BAR_HEIGHT: u32 = 16;
const PERCENT_CPU_X: u32 = 346;
const PERCENT_RAM_X: u32 = 546;
const PERCENT_DISK_X: u32 = 746;
const STATE_X: u32 = 840;

const TEXT_COLOR: Rgb<u8> = Rgb([32, 32, 32]);
const MUTED_COLOR: Rgb<u8> = Rgb([110, 110, 110]);
const ONLINE_COLOR: Rgb<u8> = Rgb([0, 140, 60]);
const OFFLINE_COLOR: Rgb<u8> = Rgb([190, 40, 40]);
const BAR_BACKGROUND: Rgb<u8> = Rgb([228, 228, 228]);

struct NodeRow {
    name: String,
    online: bool,
    cpu: f32,
    ram: f32,
    disk: f32,
}

pub(crate) async fn mjpeg_live(State(state): State<ApiState>) -> Response {
    let interval = state.config().mjpeg_frame_interval;
    // `unfold` renders on demand: when the client disconnects the body is
    // dropped and the stream (including its pending sleep) is cancelled, so no
    // background task keeps rendering.
    let stream = futures_util::stream::unfold((state, true), move |(state, first)| async move {
        if !first {
            tokio::time::sleep(interval).await;
        }
        let frame = frame_bytes(&state);
        Some((Ok::<Bytes, std::io::Error>(frame), (state, false)))
    });

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
fn frame_bytes(state: &ApiState) -> Bytes {
    let jpeg = render_frame(state);
    let mut frame = Vec::with_capacity(jpeg.len() + 96);
    frame.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    frame.extend_from_slice(b"Content-Type: image/jpeg\r\nContent-Length: ");
    frame.extend_from_slice(jpeg.len().to_string().as_bytes());
    frame.extend_from_slice(b"\r\n\r\n");
    frame.extend_from_slice(&jpeg);
    frame.extend_from_slice(b"\r\n");
    Bytes::from(frame)
}

fn render_frame(state: &ApiState) -> Vec<u8> {
    let rows = collect_rows(state);
    let row_count = rows.len().max(1) as u32;
    let height = HEADER_HEIGHT + row_count * ROW_HEIGHT + FOOTER_HEIGHT + PADDING;
    let mut image = RgbImage::from_pixel(WIDTH, height, Rgb([255, 255, 255]));

    draw_text(
        &mut image,
        PADDING,
        PADDING,
        "KOMARI MONITOR",
        3,
        TEXT_COLOR,
    );
    draw_text(&mut image, NAME_X, 40, "NODE", 1, MUTED_COLOR);
    draw_text(&mut image, CPU_BAR_X, 40, "CPU", 1, MUTED_COLOR);
    draw_text(&mut image, RAM_BAR_X, 40, "RAM", 1, MUTED_COLOR);
    draw_text(&mut image, DISK_BAR_X, 40, "DISK", 1, MUTED_COLOR);
    draw_text(&mut image, STATE_X, 40, "STATE", 1, MUTED_COLOR);

    if rows.is_empty() {
        draw_text(
            &mut image,
            NAME_X,
            HEADER_HEIGHT + 12,
            "NO NODES",
            2,
            MUTED_COLOR,
        );
    } else {
        for (index, row) in rows.iter().enumerate() {
            let y = HEADER_HEIGHT + index as u32 * ROW_HEIGHT;
            draw_text(
                &mut image,
                NAME_X,
                y + 12,
                &truncate(&row.name, 16),
                2,
                TEXT_COLOR,
            );

            draw_bar(&mut image, CPU_BAR_X, y + 12, row.cpu);
            draw_bar(&mut image, RAM_BAR_X, y + 12, row.ram);
            draw_bar(&mut image, DISK_BAR_X, y + 12, row.disk);
            draw_text(
                &mut image,
                PERCENT_CPU_X,
                y + 16,
                &format!("{:.0}%", row.cpu),
                2,
                TEXT_COLOR,
            );
            draw_text(
                &mut image,
                PERCENT_RAM_X,
                y + 16,
                &format!("{:.0}%", row.ram),
                2,
                TEXT_COLOR,
            );
            draw_text(
                &mut image,
                PERCENT_DISK_X,
                y + 16,
                &format!("{:.0}%", row.disk),
                2,
                TEXT_COLOR,
            );
            let (label, color) = if row.online {
                ("ONLINE", ONLINE_COLOR)
            } else {
                ("OFFLINE", OFFLINE_COLOR)
            };
            draw_text(&mut image, STATE_X, y + 12, label, 2, color);
        }
    }

    let updated = LocalTime::now()
        .to_rfc3339()
        .unwrap_or_else(|| "UNKNOWN".to_owned());
    draw_text(
        &mut image,
        PADDING,
        HEADER_HEIGHT + row_count * ROW_HEIGHT + 8,
        &format!("UPDATED: {updated}"),
        1,
        MUTED_COLOR,
    );

    let mut jpeg = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, 85);
    if encoder.encode_image(&image).is_err() {
        return Vec::new();
    }
    jpeg
}

fn collect_rows(state: &ApiState) -> Vec<NodeRow> {
    let clients = state.db.clients().list().unwrap_or_default();
    clients
        .into_iter()
        // Go's MJPEG handler reads `common:getNodes` as a guest and then drops
        // hidden clients.
        .filter(|client| !client.hidden)
        .take(MAX_ROWS)
        .map(|client| {
            let record = state.db.records().latest(&client.uuid).ok().flatten();
            let online =
                state.hub.is_online(&client.uuid) || record.as_ref().is_some_and(is_recent);
            NodeRow {
                name: client.name,
                online,
                cpu: record.as_ref().map(|record| record.cpu).unwrap_or(0.0),
                ram: record
                    .as_ref()
                    .filter(|record| record.ram_total > 0)
                    .map(|record| record.ram as f32 / record.ram_total as f32 * 100.0)
                    .unwrap_or(0.0),
                disk: record
                    .as_ref()
                    .filter(|record| record.disk_total > 0)
                    .map(|record| record.disk as f32 / record.disk_total as f32 * 100.0)
                    .unwrap_or(0.0),
            }
        })
        .collect()
}

fn is_recent(record: &Record) -> bool {
    let Some(time) = record.time.timestamp() else {
        return false;
    };
    let Some(now) = LocalTime::now().timestamp() else {
        return false;
    };
    (now - time).unsigned_abs() <= 60
}

fn draw_bar(image: &mut RgbImage, x: u32, y: u32, percent: f32) {
    fill_rect(image, x, y, BAR_WIDTH, BAR_HEIGHT, BAR_BACKGROUND);
    let percent = percent.clamp(0.0, 100.0);
    let fill = (BAR_WIDTH as f32 * percent / 100.0).round() as u32;
    let color = if percent < 50.0 {
        Rgb([76, 175, 80])
    } else if percent < 80.0 {
        Rgb([255, 193, 7])
    } else {
        Rgb([244, 67, 54])
    };
    if fill > 0 {
        fill_rect(image, x, y, fill, BAR_HEIGHT, color);
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn draw_text(image: &mut RgbImage, x: u32, y: u32, text: &str, scale: u32, color: Rgb<u8>) {
    let mut cursor = x;
    for character in text.chars() {
        let rows = glyph_rows(character.to_ascii_uppercase());
        for (row, bits) in rows.iter().enumerate() {
            for (column, bit) in bits.chars().enumerate() {
                if bit == '#' {
                    fill_rect(
                        image,
                        cursor + column as u32 * scale,
                        y + row as u32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
        cursor += 6 * scale;
    }
}

fn fill_rect(image: &mut RgbImage, x: u32, y: u32, width: u32, height: u32, color: Rgb<u8>) {
    let (image_width, image_height) = image.dimensions();
    for py in y..(y + height).min(image_height) {
        for px in x..(x + width).min(image_width) {
            image.put_pixel(px, py, color);
        }
    }
}

/// Built-in 5x7 bitmap font (no external font file).
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
        _ => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    }
}
