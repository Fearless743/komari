//! Backup export and import compatible with the Go backend's `backup.zip`.
//!
//! The Go backend exports a ZIP archive that contains:
//!
//! * `komari.db` – a consistent SQLite snapshot created with `VACUUM INTO`;
//! * every file below `./data` except SQLite files (`*.db`, `*.db-wal`,
//!   `*.db-shm`), at the same relative path;
//! * `komari-backup-markup` – a marker file at the archive root.
//!
//! [`Db::export_backup`] and [`Db::export_backup_to`] produce the same layout
//! from the Rust connection, and [`import_backup`] (plus its byte/file
//! variants) validates and atomically restores it. The API is independent of
//! HTTP so the future admin endpoints can simply stream the bytes.
//!
//! Import safety:
//!
//! * ZIP Slip is rejected (absolute paths, `..`, backslashes and symlinks);
//! * entry count, per-entry size, total size and compression ratio are capped;
//! * the SQLite database must pass `PRAGMA integrity_check` and contain the
//!   expected tables;
//! * the archive is extracted into a sibling staging directory and only then
//!   swapped into place with `rename(2)`, so a failed import leaves the
//!   existing data directory untouched;
//! * imports into the same data directory are serialized, while independent
//!   data directories can be restored in parallel.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use chrono::{Datelike, Timelike};
use parking_lot::{ArcMutexGuard, Mutex, RawMutex};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::db::Db;
use crate::error::{DbError, Result};
use crate::migrate::TABLES;

/// Name of the SQLite snapshot inside a backup archive (Go parity).
pub const BACKUP_DB_NAME: &str = "komari.db";
/// Name of the marker file at the archive root (Go parity).
pub const BACKUP_MARKUP_NAME: &str = "komari-backup-markup";
/// Default data directory used by the Go backend.
pub const DEFAULT_DATA_DIR: &str = "./data";

/// Tables that every historical Komari database must contain. Used when
/// [`BackupOptions::allow_legacy_schema`] is enabled.
const LEGACY_REQUIRED_TABLES: &[&str] = &["users", "clients", "configs"];

/// Options for exporting and importing a backup.
///
/// The defaults mirror the Go backend: data lives in `./data`, the database is
/// `./data/komari.db`, and archives must contain `komari-backup-markup`.
#[derive(Clone, Debug)]
pub struct BackupOptions {
    /// Directory whose contents are archived (default `./data`).
    pub data_dir: PathBuf,
    /// SQLite file used for exclusions and as the import target. Defaults to
    /// `<data_dir>/komari.db`.
    ///
    /// Export always snapshots the [`Db`] handle's own connection; this path
    /// only tells the data walk which file to skip. Import currently requires
    /// it to resolve to `<data_dir>/komari.db` because the whole data
    /// directory is swapped atomically.
    pub database_path: Option<PathBuf>,
    /// Include data-directory files. Set to `false` for a database-only backup.
    pub include_data: bool,
    /// Additional absolute or relative paths to skip during export.
    pub exclude: Vec<PathBuf>,
    /// Maximum number of ZIP entries (default 10 000).
    pub max_entries: usize,
    /// Maximum total uncompressed size in bytes (default 2 GiB).
    pub max_total_uncompressed_bytes: u64,
    /// Maximum uncompressed size of a single entry (default 512 MiB).
    pub max_entry_uncompressed_bytes: u64,
    /// Maximum uncompressed/compressed ratio per entry (default 200).
    /// `0` disables the ratio check.
    pub max_compression_ratio: u64,
    /// Maximum size of the uploaded ZIP itself (default 2 GiB).
    pub max_archive_bytes: u64,
    /// Require `komari-backup-markup` at the archive root (default `true`).
    pub require_markup: bool,
    /// Accept databases that predate the current schema. Missing tables can
    /// then be added by [`Db::migrate`] after the import (default `false`).
    pub allow_legacy_schema: bool,
    /// Optional directory for a best-effort pre-restore archive, mirroring the
    /// Go backend's `./backup/{timestamp}.zip`. `None` (the default) disables
    /// the extra archive so the library does not write outside `data_dir`
    /// unless asked to.
    pub pre_restore_backup_dir: Option<PathBuf>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            database_path: None,
            include_data: true,
            exclude: Vec::new(),
            max_entries: 10_000,
            max_total_uncompressed_bytes: 2 * 1024 * 1024 * 1024,
            max_entry_uncompressed_bytes: 512 * 1024 * 1024,
            max_compression_ratio: 200,
            max_archive_bytes: 2 * 1024 * 1024 * 1024,
            require_markup: true,
            allow_legacy_schema: false,
            pre_restore_backup_dir: None,
        }
    }
}

impl BackupOptions {
    /// Options for `data_dir`, with the default database path
    /// `<data_dir>/komari.db`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            ..Self::default()
        }
    }

    /// Set the database path (used for exclusions and as the import target).
    pub fn with_database_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(path.into());
        self
    }

    /// Allow importing a database that lacks newer tables.
    pub fn allow_legacy_schema(mut self, allow: bool) -> Self {
        self.allow_legacy_schema = allow;
        self
    }

    /// Require (or stop requiring) the Go marker file.
    pub fn require_markup(mut self, require: bool) -> Self {
        self.require_markup = require;
        self
    }

    fn resolved_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join(BACKUP_DB_NAME))
    }
}

/// Summary returned by export and import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupReport {
    /// Size of the SQLite snapshot.
    pub database_bytes: u64,
    /// Number of regular files restored/exported from the data directory.
    pub data_entries: usize,
    /// Total uncompressed bytes in the archive.
    pub total_uncompressed_bytes: u64,
    /// Whether the imported database lacks some current tables.
    pub legacy_schema: bool,
    /// Whether an existing data directory was replaced.
    pub replaced_existing: bool,
    /// Best-effort pre-restore archive, when
    /// [`BackupOptions::pre_restore_backup_dir`] was configured.
    pub pre_restore_archive: Option<PathBuf>,
}

/// One entry discovered while walking the data directory.
struct DataEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

/// Metadata collected from a ZIP entry before extraction.
struct EntryMeta {
    index: usize,
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    compressed_size: u64,
}

/// One import lock per target data directory. Importing into the same
/// directory concurrently returns [`DbError::BackupInProgress`]; independent
/// directories can be restored in parallel.
static IMPORT_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Db {
    /// Export a backup into memory.
    ///
    /// Prefer [`Db::export_backup_to`] for large installations.
    pub fn export_backup(&self, options: &BackupOptions) -> Result<Vec<u8>> {
        let mut cursor = io::Cursor::new(Vec::new());
        self.write_backup(options, &mut cursor, &[], false)?;
        Ok(cursor.into_inner())
    }

    /// Export a backup to `destination`.
    ///
    /// The archive is written to a temporary file next to `destination` and
    /// renamed into place, so readers never observe a partial archive. The
    /// destination itself is excluded from the data walk when it lives below
    /// [`BackupOptions::data_dir`].
    pub fn export_backup_to(
        &self,
        options: &BackupOptions,
        destination: impl AsRef<Path>,
    ) -> Result<BackupReport> {
        self.write_backup_to(options, destination.as_ref(), false)
    }

    fn write_backup_to(
        &self,
        options: &BackupOptions,
        destination: &Path,
        pre_restore: bool,
    ) -> Result<BackupReport> {
        let parent = non_empty_parent(destination);
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
        // Resolve symlinked parents after they exist so `link/komari.db`
        // cannot be used to overwrite the live database through `link -> data`.
        ensure_safe_destination(self, options, destination)?;

        let temp_zip = unique_path(parent, ".komari-backup-export");
        let mut guard = TempFileGuard::new(temp_zip.clone());
        let file = File::create(&temp_zip)?;

        let report = self.write_backup(
            options,
            file,
            &[normalize_path(destination), normalize_path(&temp_zip)],
            pre_restore,
        )?;

        match fs::rename(&temp_zip, destination) {
            Ok(()) => {}
            Err(first_error) => {
                // Windows cannot rename over an existing file. Fall back to
                // replacing it explicitly; Unix never reaches this branch.
                if destination.exists() {
                    fs::remove_file(destination)?;
                    fs::rename(&temp_zip, destination)?;
                } else {
                    return Err(first_error.into());
                }
            }
        }
        guard.disarm();
        Ok(report)
    }

    fn write_backup<W: Write + Seek>(
        &self,
        options: &BackupOptions,
        writer: W,
        extra_excludes: &[PathBuf],
        pre_restore: bool,
    ) -> Result<BackupReport> {
        validate_common_options(options)?;

        let snapshot = TempFileGuard::new(unique_path(&std::env::temp_dir(), ".komari-backup-db"));
        let database_bytes = self.snapshot_database(&snapshot.path)?;

        let mut excludes: HashSet<PathBuf> = options
            .exclude
            .iter()
            .map(|path| normalize_path(path))
            .collect();
        excludes.insert(normalize_path(&options.resolved_database_path()));
        excludes.insert(normalize_path(&options.data_dir.join(BACKUP_DB_NAME)));
        for path in extra_excludes {
            excludes.insert(normalize_path(path));
        }

        let mut zip = ZipWriter::new(writer);
        add_file_from_disk(&mut zip, BACKUP_DB_NAME, &snapshot.path, 0o644)?;

        let mut data_entries = 0usize;
        let mut total_uncompressed_bytes = database_bytes;
        if options.include_data && options.data_dir.exists() {
            let entries = collect_data_entries(options, &excludes, pre_restore)?;
            for entry in entries {
                if entry.is_dir {
                    add_directory(&mut zip, &entry.name, 0o755)?;
                } else {
                    add_file_from_disk(&mut zip, &entry.name, &entry.path, 0o644)?;
                    data_entries += 1;
                    total_uncompressed_bytes = total_uncompressed_bytes.saturating_add(entry.size);
                }
            }
        }

        add_bytes(
            &mut zip,
            BACKUP_MARKUP_NAME,
            markup_content().as_bytes(),
            0o644,
        )?;
        zip.finish()?;

        Ok(BackupReport {
            database_bytes,
            data_entries,
            total_uncompressed_bytes,
            legacy_schema: false,
            replaced_existing: false,
            pre_restore_archive: None,
        })
    }

    fn snapshot_database(&self, destination: &Path) -> Result<u64> {
        if destination.exists() {
            fs::remove_file(destination)?;
        }
        let escaped = destination.to_string_lossy().replace('\'', "''");
        self.with_connection(|conn| {
            conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;
            Ok(())
        })?;
        Ok(fs::metadata(destination)?.len())
    }
}

/// Import a backup from any seekable reader.
///
/// The archive is fully validated and extracted to a staging directory before
/// the existing data directory is replaced. On error the existing directory
/// is left untouched.
pub fn import_backup<R: Read + Seek>(
    options: &BackupOptions,
    mut reader: R,
) -> Result<BackupReport> {
    let _guard = import_guard(options)?;
    validate_common_options(options)?;
    validate_import_paths(options)?;

    let archive_len = reader.seek(SeekFrom::End(0))?;
    if options.max_archive_bytes > 0 && archive_len > options.max_archive_bytes {
        return Err(DbError::BackupLimit(format!(
            "archive is {archive_len} bytes, limit is {}",
            options.max_archive_bytes
        )));
    }
    reader.seek(SeekFrom::Start(0))?;

    let mut archive = ZipArchive::new(reader)?;
    let metas = inspect_archive(&mut archive, options)?;

    let staging_parent = non_empty_parent(&options.data_dir);
    fs::create_dir_all(staging_parent)?;
    let mut staging = TempDirGuard::new(unique_temp_dir(staging_parent, ".komari-backup-import")?);
    extract_archive(&mut archive, &metas, &staging.path)?;

    let database_file = staging.path.join(BACKUP_DB_NAME);
    if !database_file.is_file() {
        return Err(DbError::InvalidBackup(
            "archive does not contain a komari.db file".to_owned(),
        ));
    }

    let legacy_schema = validate_database(&database_file, options)?;
    let pre_restore_archive = create_pre_restore_archive(options);
    let replaced_existing = replace_data_dir(&staging.path, &options.data_dir)?;
    staging.disarm();

    let data_entries = metas
        .iter()
        .filter(|meta| {
            !meta.is_dir && meta.name != BACKUP_MARKUP_NAME && meta.name != BACKUP_DB_NAME
        })
        .count();
    let total_uncompressed_bytes = metas.iter().map(|meta| meta.size).sum();

    Ok(BackupReport {
        database_bytes: fs::metadata(options.data_dir.join(BACKUP_DB_NAME))
            .map(|meta| meta.len())
            .unwrap_or(0),
        data_entries,
        total_uncompressed_bytes,
        legacy_schema,
        replaced_existing,
        pre_restore_archive,
    })
}

/// Mirror the Go startup restore, which archives the current data directory to
/// `./backup/{timestamp}.zip` before deleting it. Failures are ignored because
/// a broken old database must not prevent an operator from restoring a good
/// backup.
fn create_pre_restore_archive(options: &BackupOptions) -> Option<PathBuf> {
    let directory = options.pre_restore_backup_dir.as_ref()?;
    let database = options.resolved_database_path();
    if !options.data_dir.is_dir() || !database.is_file() {
        return None;
    }

    if normalize_path(directory).starts_with(normalize_path(&options.data_dir)) {
        eprintln!(
            "komari-db backup: pre_restore_backup_dir {} is inside data_dir {}; skipping pre-restore archive",
            directory.display(),
            options.data_dir.display()
        );
        return None;
    }

    fs::create_dir_all(directory).ok()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut archive = directory.join(format!("{stamp}.zip"));
    let mut counter = 1;
    while archive.exists() {
        archive = directory.join(format!("{stamp}-{counter}.zip"));
        counter += 1;
    }

    let db = Db::open_read_only(&database).ok()?;
    let mut export_options = options.clone();
    // The pre-restore snapshot must contain the whole old data directory even
    // when the caller requested a database-only backup; otherwise the swap
    // would delete data that was never archived.
    export_options.include_data = true;
    export_options.exclude.clear();
    export_options.pre_restore_backup_dir = None;
    db.write_backup_to(&export_options, &archive, true).ok()?;
    Some(archive)
}

/// Import a backup from an in-memory ZIP.
pub fn import_backup_bytes(options: &BackupOptions, bytes: &[u8]) -> Result<BackupReport> {
    import_backup(options, io::Cursor::new(bytes))
}

/// Import a backup from a ZIP file on disk.
pub fn import_backup_file(options: &BackupOptions, path: impl AsRef<Path>) -> Result<BackupReport> {
    let path = path.as_ref();
    if options.max_archive_bytes > 0 {
        let len = fs::metadata(path)?.len();
        if len > options.max_archive_bytes {
            return Err(DbError::BackupLimit(format!(
                "archive is {len} bytes, limit is {}",
                options.max_archive_bytes
            )));
        }
    }
    import_backup(options, File::open(path)?)
}

fn ensure_safe_destination(db: &Db, options: &BackupOptions, destination: &Path) -> Result<()> {
    let destination = canonical_candidate(destination);
    let mut protected = Vec::new();
    if let Some(path) = db.path() {
        protected.push(path.to_path_buf());
    }
    protected.push(options.resolved_database_path());
    protected.push(options.data_dir.join(BACKUP_DB_NAME));

    for path in protected {
        for candidate in [
            path.clone(),
            append_suffix(&path, "-wal"),
            append_suffix(&path, "-shm"),
        ] {
            if canonical_candidate(&candidate) == destination {
                return Err(DbError::InvalidData(format!(
                    "backup destination {} would overwrite the live database",
                    destination.display()
                )));
            }
        }
    }
    Ok(())
}

/// Resolve a path for comparison: canonicalize it when it exists (following a
/// final symlink), otherwise canonicalize its parent and keep the file name.
fn canonical_candidate(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).unwrap_or_else(|_| normalize_path(parent));
    match path.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

/// Restore `<data_dir>/backup.zip` when present, mirroring the Go backend's
/// startup restore. Returns `None` when no pending archive exists.
///
/// The archive is copied to a sibling temporary file outside `data_dir` before
/// the import, so the restore streams from disk instead of loading the whole
/// ZIP into memory and does not depend on the old directory (which contains
/// the pending archive) surviving the replacement. The pending file is removed
/// after a successful restore, including when the archive itself contained a
/// `backup.zip` entry.
pub fn restore_pending_backup(options: &BackupOptions) -> Result<Option<BackupReport>> {
    let pending = options.data_dir.join("backup.zip");
    if !pending.is_file() {
        return Ok(None);
    }

    let archive_len = fs::metadata(&pending)?.len();
    if options.max_archive_bytes > 0 && archive_len > options.max_archive_bytes {
        return Err(DbError::BackupLimit(format!(
            "pending backup is {archive_len} bytes, limit is {}",
            options.max_archive_bytes
        )));
    }

    let temporary = TempFileGuard::new(unique_path(
        non_empty_parent(&options.data_dir),
        ".komari-pending-backup",
    ));
    fs::copy(&pending, &temporary.path)?;
    let report = import_backup_file(options, &temporary.path)?;

    // A `backup.zip` entry inside the archive is now part of the restored data
    // directory; remove it so the next startup does not restore it again.
    let restored_pending = options.data_dir.join("backup.zip");
    if restored_pending.is_file() {
        fs::remove_file(&restored_pending)?;
    }
    Ok(Some(report))
}

fn import_guard(options: &BackupOptions) -> Result<ArcMutexGuard<RawMutex, ()>> {
    let registry = IMPORT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let lock = {
        let mut registry = registry.lock();
        registry
            .entry(normalize_path(&options.data_dir))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    lock.try_lock_arc().ok_or(DbError::BackupInProgress)
}

fn validate_common_options(options: &BackupOptions) -> Result<()> {
    if options.data_dir.as_os_str().is_empty() {
        return Err(DbError::InvalidData(
            "backup data_dir must not be empty".to_owned(),
        ));
    }
    if options.data_dir.parent().is_none() {
        return Err(DbError::InvalidData(
            "backup data_dir must not be a filesystem root".to_owned(),
        ));
    }
    Ok(())
}

fn validate_import_paths(options: &BackupOptions) -> Result<()> {
    let expected = normalize_path(&options.data_dir.join(BACKUP_DB_NAME));
    let configured = normalize_path(&options.resolved_database_path());
    if configured != expected {
        return Err(DbError::InvalidData(format!(
            "import_backup requires database_path to be {} (got {})",
            expected.display(),
            configured.display()
        )));
    }
    if let Ok(metadata) = options.data_dir.symlink_metadata() {
        if metadata.file_type().is_symlink() {
            return Err(DbError::InvalidData(format!(
                "{} is a symbolic link; restore into a real directory",
                options.data_dir.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(DbError::InvalidData(format!(
                "{} is not a directory",
                options.data_dir.display()
            )));
        }
    }
    Ok(())
}

fn inspect_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    options: &BackupOptions,
) -> Result<Vec<EntryMeta>> {
    if archive.len() > options.max_entries {
        return Err(DbError::BackupLimit(format!(
            "archive has {} entries, limit is {}",
            archive.len(),
            options.max_entries
        )));
    }

    let mut metas = Vec::with_capacity(archive.len());
    let mut total = 0u64;
    let mut has_markup = false;
    let mut has_database = false;

    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name().to_owned();
        if file.encrypted() {
            return Err(DbError::InvalidBackup(format!(
                "entry {name:?} is encrypted"
            )));
        }
        if let Some(mode) = file.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(DbError::UnsafeBackupPath(format!(
                    "symbolic link entry {name:?} is not allowed"
                )));
            }
        }

        // `zip` de-duplicates names while parsing the central directory, so a
        // separate duplicate check here would be unreachable.
        let path = sanitize_zip_name(&name)?;

        let is_dir = file.is_dir();
        let size = file.size();
        let compressed_size = file.compressed_size();

        if size > options.max_entry_uncompressed_bytes {
            return Err(DbError::BackupLimit(format!(
                "entry {name:?} is {size} bytes, per-entry limit is {}",
                options.max_entry_uncompressed_bytes
            )));
        }
        if options.max_compression_ratio > 0
            && size > 0
            && (compressed_size == 0
                || size > compressed_size.saturating_mul(options.max_compression_ratio))
        {
            return Err(DbError::BackupLimit(format!(
                "entry {name:?} has a suspicious compression ratio ({size}/{compressed_size})"
            )));
        }
        total = total.saturating_add(size);
        if options.max_total_uncompressed_bytes > 0 && total > options.max_total_uncompressed_bytes
        {
            return Err(DbError::BackupLimit(format!(
                "archive expands to at least {total} bytes, limit is {}",
                options.max_total_uncompressed_bytes
            )));
        }

        if name == BACKUP_MARKUP_NAME {
            if is_dir {
                return Err(DbError::InvalidBackup(
                    "komari-backup-markup must be a file".to_owned(),
                ));
            }
            has_markup = true;
        }
        if name == BACKUP_DB_NAME {
            if is_dir {
                return Err(DbError::InvalidBackup(
                    "komari.db must be a file".to_owned(),
                ));
            }
            has_database = true;
        }

        metas.push(EntryMeta {
            index,
            name,
            path,
            is_dir,
            size,
            compressed_size,
        });
    }

    if options.require_markup && !has_markup {
        return Err(DbError::InvalidBackup(format!(
            "missing {BACKUP_MARKUP_NAME} file"
        )));
    }
    if !has_database {
        return Err(DbError::InvalidBackup(
            "archive does not contain komari.db".to_owned(),
        ));
    }

    Ok(metas)
}

fn extract_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    metas: &[EntryMeta],
    staging: &Path,
) -> Result<()> {
    fs::create_dir_all(staging)?;

    for meta in metas {
        if meta.name == BACKUP_MARKUP_NAME {
            continue;
        }
        let target = staging.join(&meta.path);
        if meta.is_dir {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut entry = archive.by_index(meta.index)?;
        if entry.size() != meta.size || entry.compressed_size() != meta.compressed_size {
            return Err(DbError::InvalidBackup(format!(
                "entry {:?} changed while reading",
                meta.name
            )));
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        // Never let a lying header make us write more than the validated size.
        let mut limited = (&mut entry).take(meta.size.saturating_add(1));
        let copied = io::copy(&mut limited, &mut output)?;
        if copied != meta.size {
            return Err(DbError::InvalidBackup(format!(
                "entry {:?} yielded {copied} bytes, expected {}",
                meta.name, meta.size
            )));
        }
        output.flush()?;
    }
    Ok(())
}

fn validate_database(path: &Path, options: &BackupOptions) -> Result<bool> {
    let db = Db::open_read_only(path).map_err(|error| {
        DbError::BackupIntegrity(format!("cannot open SQLite database: {error}"))
    })?;

    let report = db
        .integrity_check()
        .map_err(|error| DbError::BackupIntegrity(error.to_string()))?;
    if report.len() != 1 || report[0] != "ok" {
        return Err(DbError::BackupIntegrity(report.join("; ")));
    }

    let tables = db
        .table_names()
        .map_err(|error| DbError::BackupIntegrity(error.to_string()))?;
    let present: HashSet<&str> = tables.iter().map(String::as_str).collect();
    let required: &[&str] = if options.allow_legacy_schema {
        LEGACY_REQUIRED_TABLES
    } else {
        TABLES
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|table| !present.contains(table))
        .collect();
    if !missing.is_empty() {
        return Err(DbError::BackupSchema(format!(
            "missing required tables: {}",
            missing.join(", ")
        )));
    }

    let legacy_schema = TABLES.iter().any(|table| !present.contains(table));
    Ok(legacy_schema)
}

fn replace_data_dir(staging: &Path, data_dir: &Path) -> Result<bool> {
    let parent = non_empty_parent(data_dir);
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent)?;
    }

    if !data_dir.exists() {
        fs::rename(staging, data_dir)?;
        return Ok(false);
    }

    // Linux can swap the two directory entries atomically, so there is never a
    // window where `data_dir` does not exist. Other platforms fall back to the
    // two-rename sequence below.
    if exchange_directories(staging, data_dir).is_ok() {
        // `staging` now contains the old data directory.
        if let Err(error) = fs::remove_dir_all(staging) {
            eprintln!(
                "komari-db backup: could not remove previous data at {}: {error}",
                staging.display()
            );
        }
        return Ok(true);
    }

    let previous = unique_path(parent, ".komari-backup-previous");
    fs::rename(data_dir, &previous)?;
    match fs::rename(staging, data_dir) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&previous);
            Ok(true)
        }
        Err(error) => {
            // Put the old directory back; the caller can keep running.
            let _ = fs::rename(&previous, data_dir);
            let _ = fs::remove_dir_all(staging);
            Err(error.into())
        }
    }
}

#[cfg(target_os = "linux")]
fn exchange_directories(left: &Path, right: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let right = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;

    // SAFETY: both pointers are valid NUL-terminated strings for the duration
    // of the call and `AT_FDCWD` is a valid dirfd.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn exchange_directories(_left: &Path, _right: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic directory exchange is only available on Linux",
    ))
}

fn collect_data_entries(
    options: &BackupOptions,
    excludes: &HashSet<PathBuf>,
    pre_restore: bool,
) -> Result<Vec<DataEntry>> {
    let mut entries = Vec::new();
    walk_data_dir(
        &options.data_dir,
        &options.data_dir,
        excludes,
        pre_restore,
        &mut entries,
    )?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn walk_data_dir(
    root: &Path,
    dir: &Path,
    excludes: &HashSet<PathBuf>,
    pre_restore: bool,
    entries: &mut Vec<DataEntry>,
) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for child in children {
        let path = child.path();
        let file_type = child.file_type()?;
        if file_type.is_symlink() {
            // Never follow links outside the data directory.
            continue;
        }

        let normalized = normalize_path(&path);
        if excludes.contains(&normalized) {
            continue;
        }

        let name = child.file_name().to_string_lossy().into_owned();
        let relative = path.strip_prefix(root).map_err(|_| {
            DbError::InvalidData(format!("{} escaped the data dir", path.display()))
        })?;
        let Some(zip_name) = path_to_zip_name(relative)? else {
            eprintln!(
                "komari-db backup: skipping non-UTF-8 path {}",
                path.display()
            );
            continue;
        };

        if file_type.is_dir() {
            // Go's Walk skips a directory entry whose name looks like a
            // database file but still descends into it. Do the same so data
            // below e.g. `archive.db/` is not lost.
            if !is_database_name(&name) {
                entries.push(DataEntry {
                    path: path.clone(),
                    name: zip_name,
                    is_dir: true,
                    size: 0,
                });
            }
            walk_data_dir(root, &path, excludes, pre_restore, entries)?;
        } else if file_type.is_file() {
            if is_skipped_file_name(&name, pre_restore) {
                continue;
            }
            entries.push(DataEntry {
                path: path.clone(),
                name: zip_name,
                is_dir: false,
                size: child.metadata()?.len(),
            });
        }
        // Sockets, FIFOs and device nodes are intentionally skipped.
    }
    Ok(())
}

fn is_database_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".db") || lower.ends_with(".db-wal") || lower.ends_with(".db-shm")
}

fn is_skipped_file_name(name: &str, pre_restore: bool) -> bool {
    if is_database_name(name)
        || name == BACKUP_MARKUP_NAME
        || name.eq_ignore_ascii_case("backup.zip")
    {
        return true;
    }

    if pre_restore {
        // Go's pre-restore archive only excludes the database sidecars and
        // `backup.zip`; keep transient files so the safety copy is complete.
        return false;
    }

    // Deliberately broader than Go (the task asks for temporary files to be
    // excluded). These rules only apply to files: directories with such names
    // are still walked so their contents are never silently dropped.
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".tmp")
        || lower.ends_with(".temp")
        || lower.ends_with(".part")
        || lower.ends_with(".swp")
        || lower.ends_with(".swo")
        || lower.ends_with('~')
        || lower.starts_with(".~")
        || lower.starts_with(".komari-backup-")
}

fn sanitize_zip_name(name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        return Err(DbError::InvalidBackup("empty entry name".to_owned()));
    }
    if name.contains('\0') {
        return Err(DbError::UnsafeBackupPath(name.to_owned()));
    }
    if name.contains('\\') {
        return Err(DbError::UnsafeBackupPath(name.to_owned()));
    }

    let raw = Path::new(name);
    if raw.is_absolute() {
        return Err(DbError::UnsafeBackupPath(name.to_owned()));
    }

    let mut clean = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(DbError::UnsafeBackupPath(name.to_owned()));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(DbError::UnsafeBackupPath(name.to_owned()));
    }
    Ok(clean)
}

fn path_to_zip_name(path: &Path) -> Result<Option<String>> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part.to_owned()),
                // The ZIP writer API is UTF-8 only. Skip the entry instead of
                // failing the whole backup; the caller logs the path.
                None => return Ok(None),
            },
            _ => {
                return Err(DbError::InvalidData(format!(
                    "unexpected path component in {}",
                    path.display()
                )));
            }
        }
    }
    Ok(Some(parts.join("/")))
}

fn zip_options(mode: u32, compression: CompressionMethod) -> SimpleFileOptions {
    let now = chrono::Local::now();
    let modified = zip::DateTime::from_date_and_time(
        now.year() as u16,
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
    )
    .unwrap_or_default();
    SimpleFileOptions::default()
        .compression_method(compression)
        .unix_permissions(mode)
        .last_modified_time(modified)
}

fn add_file_from_disk<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    name: &str,
    path: &Path,
    mode: u32,
) -> Result<()> {
    zip.start_file(name, zip_options(mode, CompressionMethod::Deflated))?;
    let mut file = File::open(path)?;
    io::copy(&mut file, zip)?;
    Ok(())
}

fn add_bytes<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    zip.start_file(name, zip_options(mode, CompressionMethod::Deflated))?;
    zip.write_all(bytes)?;
    Ok(())
}

fn add_directory<W: Write + Seek>(zip: &mut ZipWriter<W>, name: &str, mode: u32) -> Result<()> {
    let name = format!("{}/", name.trim_end_matches('/'));
    zip.add_directory(name, zip_options(mode, CompressionMethod::Stored))?;
    Ok(())
}

fn markup_content() -> String {
    // Go uses time.RFC3339: second precision, numeric offset, no trailing
    // newline.
    format!(
        "此文件为 Komari 备份标记文件，请勿删除。\n\
         This is a Komari backup markup file, please do not delete.\n\n\
         备份时间 / Backup Time: {}",
        chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
    )
}

fn normalize_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn non_empty_parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn unique_path(parent: &Path, prefix: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{prefix}-{}-{counter}", std::process::id()))
}

fn unique_temp_dir(parent: &Path, prefix: &str) -> Result<PathBuf> {
    for _ in 0..100 {
        let path = unique_path(parent, prefix);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(DbError::InvalidData(
        "could not create a unique staging directory".to_owned(),
    ))
}

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct TempDirGuard {
    path: PathBuf,
    armed: bool,
}

impl TempDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
