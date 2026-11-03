//! Backup download and upload (`/api/admin/{download,upload}/backup`).
//!
//! * Download uses [`komari_db::Db::export_backup`], which produces the same
//!   `komari.db` + `data/**` + `komari-backup-markup` ZIP as the Go backend.
//! * Upload streams the multipart field to a temporary file (so large backups
//!   are not buffered in memory), validates the marker file and stages the
//!   archive as `<data_dir>/backup.zip`, where the server assembly restores it
//!   on the next start with [`komari_db::restore_pending_backup`] (Go exits and
//!   lets the startup hook apply it).

use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use komari_auth::RequireAdmin;
use komari_db::backup::{BACKUP_MARKUP_NAME, BackupOptions};
use serde_json::json;

use super::{audit, bad_request, internal};
use crate::response;
use crate::state::ApiState;

/// Hard cap for an uploaded archive (2 GiB, matching `BackupOptions`).
const MAX_BACKUP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/admin/download/backup", get(download_backup))
        .route(
            "/api/admin/upload/backup",
            // Axum's default body limit is 2 MB; real backups are much larger.
            post(upload_backup).layer(DefaultBodyLimit::disable()),
        )
}

/// `GET /api/admin/download/backup` — Go `admin.DownloadBackup`.
pub(crate) async fn download_backup(
    State(state): State<ApiState>,
    _admin: RequireAdmin,
) -> Response {
    let db = Arc::clone(&state.db);
    let options = BackupOptions::new(state.data_dir());
    let archive = match tokio::task::spawn_blocking(move || db.export_backup(&options)).await {
        Ok(Ok(archive)) => archive,
        Ok(Err(error)) => {
            return internal(format!("Error backing up database: {error}"));
        }
        Err(error) => {
            return internal(format!("Error backing up database: {error}"));
        }
    };

    let file_name = format!("backup-{}.zip", chrono::Utc::now().timestamp_micros());
    let mut response = Response::new(axum::body::Body::from(archive));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, "application/zip".parse().unwrap());
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"")
            .parse()
            .unwrap(),
    );
    response
}

/// `POST /api/admin/upload/backup` — Go `admin.UploadBackup`.
pub(crate) async fn upload_backup(
    State(state): State<ApiState>,
    admin: RequireAdmin,
    mut multipart: Multipart,
) -> Response {
    let data_dir = state.data_dir();
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        return internal(format!("Error creating data directory: {error}"));
    }

    let temporary = data_dir.join(format!(
        ".backup-upload-{}-{}.zip",
        std::process::id(),
        chrono::Utc::now().timestamp_micros()
    ));
    let mut received = false;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return bad_request(format!("Error getting uploaded file: {error}"));
            }
        };
        if field.name() != Some("backup") {
            continue;
        }
        let file_name = field.file_name().unwrap_or_default().to_owned();
        if !file_name.to_ascii_lowercase().ends_with(".zip") {
            let _ = std::fs::remove_file(&temporary);
            return bad_request("Uploaded file must be a ZIP archive");
        }

        let mut file = match File::create(&temporary) {
            Ok(file) => file,
            Err(error) => {
                return internal(format!("Error saving uploaded file: {error}"));
            }
        };
        let mut size = 0u64;
        let mut field = field;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    size = size.saturating_add(chunk.len() as u64);
                    if size > MAX_BACKUP_BYTES {
                        let _ = std::fs::remove_file(&temporary);
                        return response::error_status(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "Uploaded backup is too large",
                        );
                    }
                    if let Err(error) = file.write_all(&chunk) {
                        let _ = std::fs::remove_file(&temporary);
                        return internal(format!("Error saving uploaded file: {error}"));
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return bad_request(format!("Error getting uploaded file: {error}"));
                }
            }
        }
        if let Err(error) = file.sync_all() {
            let _ = std::fs::remove_file(&temporary);
            return internal(format!("Error saving uploaded file: {error}"));
        }
        received = true;
        break;
    }

    if !received {
        return bad_request("Error getting uploaded file: missing backup field");
    }

    let mut archive = match File::open(&temporary)
        .map_err(|error| error.to_string())
        .and_then(|file| zip::ZipArchive::new(file).map_err(|error| error.to_string()))
    {
        Ok(archive) => archive,
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return internal(format!("Error opening zip file: {error}"));
        }
    };
    let has_markup = (0..archive.len()).any(|index| {
        archive
            .by_index(index)
            .map(|entry| entry.name() == BACKUP_MARKUP_NAME)
            .unwrap_or(false)
    });
    if !has_markup {
        let _ = std::fs::remove_file(&temporary);
        return bad_request("Invalid backup file: missing komari-backup-markup file");
    }
    drop(archive);

    let path = data_dir.join("backup.zip");
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        return internal(format!("Error saving uploaded file: {error}"));
    }

    audit(&state, &admin, "upload backup", "warn");
    // Return the real data-directory path; `--database`/custom data dirs may
    // move it away from Go's fixed `./data/backup.zip` spelling.
    axum::Json(json!({
        "status": "success",
        "message": "Backup uploaded successfully. The service will restart and apply the backup.",
        "path": path.to_string_lossy(),
    }))
    .into_response()
}
