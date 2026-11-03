//! Backup export/import coverage: round trips, WAL snapshots, ZIP safety,
//! limits, schema checks, rollback and concurrency.

mod support;

use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use komari_db::{
    BACKUP_DB_NAME, BACKUP_MARKUP_NAME, BackupOptions, Db, DbError, import_backup,
    import_backup_bytes, import_backup_file, restore_pending_backup,
};
use support::TempDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MARKER: &[u8] = b"komari backup marker\n";

fn make_data_dir(root: &TempDir) -> PathBuf {
    let data = root.join("data");
    fs::create_dir_all(&data).expect("create data dir");
    data
}

fn make_db(data: &Path) -> Db {
    let db = Db::open(data.join(BACKUP_DB_NAME)).expect("open database");
    db.migrate().expect("migrate database");
    db
}

fn options(data: &Path) -> BackupOptions {
    BackupOptions::new(data)
}

fn zip_names(bytes: &[u8]) -> Vec<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open zip");
    (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_owned())
        .collect()
}

fn read_entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open zip");
    let mut file = archive.by_name(name).ok()?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).expect("read entry");
    Some(out)
}

fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        for (name, data) in entries {
            let file_options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            if name.ends_with('/') {
                zip.add_directory(*name, file_options).unwrap();
            } else {
                zip.start_file(*name, file_options).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

fn build_zip_with_symlink(database: &[u8]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let file_options =
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file(BACKUP_MARKUP_NAME, file_options).unwrap();
        zip.write_all(MARKER).unwrap();
        zip.start_file(BACKUP_DB_NAME, file_options).unwrap();
        zip.write_all(database).unwrap();
        zip.add_symlink("link", "target", SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
    }
    cursor.into_inner()
}

fn duplicate_central_directory_name(mut bytes: Vec<u8>, from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let signature = b"PK\x01\x02";
    let mut position = 0;
    while let Some(relative) = bytes[position..]
        .windows(signature.len())
        .position(|window| window == signature)
    {
        let header = position + relative;
        if header + 46 <= bytes.len() {
            let name_len = u16::from_le_bytes([bytes[header + 28], bytes[header + 29]]) as usize;
            let extra_len = u16::from_le_bytes([bytes[header + 30], bytes[header + 31]]) as usize;
            let comment_len = u16::from_le_bytes([bytes[header + 32], bytes[header + 33]]) as usize;
            let name_start = header + 46;
            if name_start + name_len <= bytes.len()
                && &bytes[name_start..name_start + name_len] == from.as_bytes()
            {
                bytes[name_start..name_start + name_len].copy_from_slice(to.as_bytes());
            }
            position = name_start + name_len + extra_len + comment_len;
        } else {
            break;
        }
    }
    bytes
}

fn mark_entries_encrypted(mut bytes: Vec<u8>) -> Vec<u8> {
    for signature in [b"PK\x03\x04".as_slice(), b"PK\x01\x02".as_slice()] {
        let flag_offset = if signature == b"PK\x03\x04" { 6 } else { 8 };
        let positions: Vec<usize> = bytes
            .windows(signature.len())
            .enumerate()
            .filter(|(_, window)| *window == signature)
            .map(|(index, _)| index)
            .collect();
        for position in positions {
            let offset = position + flag_offset;
            if offset + 2 <= bytes.len() {
                bytes[offset] |= 1;
            }
        }
    }
    bytes
}

struct BlockingReader {
    inner: Cursor<Vec<u8>>,
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    blocked: bool,
}

impl Read for BlockingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Seek for BlockingReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if !self.blocked && position == SeekFrom::End(0) {
            self.blocked = true;
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            let _ = self.release.recv();
        }
        self.inner.seek(position)
    }
}

fn snapshot_bytes(db: &Db) -> Vec<u8> {
    let mut export_options = BackupOptions::new("unused");
    export_options.include_data = false;
    let archive = db.export_backup(&export_options).expect("export snapshot");
    read_entry(&archive, BACKUP_DB_NAME).expect("database entry")
}

fn sentinel_target(root: &TempDir) -> PathBuf {
    let target = root.join("data");
    fs::create_dir_all(&target).expect("create target");
    fs::write(target.join("sentinel.txt"), b"keep me").expect("write sentinel");
    target
}

fn assert_sentinel(target: &Path) {
    assert_eq!(
        fs::read(target.join("sentinel.txt")).expect("sentinel must survive"),
        b"keep me"
    );
}

#[test]
fn empty_database_round_trip() {
    let source = TempDir::new("backup-empty-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);

    let archive = db.export_backup(&options(&data)).expect("export");
    drop(db);

    assert!(
        zip_names(&archive)
            .iter()
            .any(|name| name == BACKUP_DB_NAME)
    );
    assert!(
        zip_names(&archive)
            .iter()
            .any(|name| name == BACKUP_MARKUP_NAME)
    );

    let destination = TempDir::new("backup-empty-destination");
    let target = destination.join("data");
    let report = import_backup_bytes(&options(&target), &archive).expect("import");

    assert!(!report.replaced_existing);
    assert!(!report.legacy_schema);
    assert_eq!(report.data_entries, 0);
    assert!(!target.join(BACKUP_MARKUP_NAME).exists());

    let restored = Db::open(target.join(BACKUP_DB_NAME)).expect("open restored database");
    assert!(restored.schema_is_current().expect("schema check"));
    assert_eq!(restored.clients().count().expect("client count"), 0);
}

#[test]
fn in_memory_database_exports_to_a_restorable_archive() {
    let db = Db::open_in_memory().expect("in-memory database");
    db.migrate().expect("migrate");
    db.config().set("sitename", "in-memory").unwrap();

    let mut export_options = BackupOptions::new("unused");
    export_options.include_data = false;
    let archive = db.export_backup(&export_options).expect("export");

    let destination = TempDir::new("backup-memory-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).expect("import");

    let restored = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    let sitename: Option<String> = restored.config().get_as("sitename").unwrap();
    assert_eq!(sitename.as_deref(), Some("in-memory"));
}

#[test]
fn data_directory_and_rows_round_trip() {
    let source = TempDir::new("backup-data-source");
    let data = make_data_dir(&source);
    fs::create_dir_all(data.join("theme")).unwrap();
    fs::create_dir_all(data.join("uploads")).unwrap();
    fs::write(data.join("theme/main.css"), b"body { color: red; }").unwrap();
    fs::write(data.join("uploads/blob.bin"), [0u8, 1, 2, 3, 255]).unwrap();

    let db = make_db(&data);
    db.config().set("sitename", "Komari Rust").unwrap();
    let client = db.clients().create_named("web-1").unwrap();
    let archive = db.export_backup(&options(&data)).expect("export");
    drop(db);

    let destination = TempDir::new("backup-data-destination");
    let target = destination.join("data");
    let report = import_backup_bytes(&options(&target), &archive).expect("import");
    assert_eq!(report.data_entries, 2);
    assert_eq!(
        fs::read(target.join("theme/main.css")).unwrap(),
        b"body { color: red; }"
    );
    assert_eq!(
        fs::read(target.join("uploads/blob.bin")).unwrap(),
        [0u8, 1, 2, 3, 255]
    );

    let restored = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    let sitename: Option<String> = restored.config().get_as("sitename").unwrap();
    assert_eq!(sitename.as_deref(), Some("Komari Rust"));
    assert_eq!(
        restored.clients().require(&client.uuid).unwrap().name,
        "web-1"
    );
}

#[test]
fn wal_content_is_included_in_the_snapshot() {
    let source = TempDir::new("backup-wal-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    db.with_connection(|conn| {
        conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        Ok(())
    })
    .unwrap();

    let mut expected = Vec::new();
    for index in 0..64 {
        let client = db.clients().create_named(format!("wal-{index}")).unwrap();
        expected.push(client.uuid);
    }

    let archive = db.export_backup(&options(&data)).expect("export with WAL");
    let names = zip_names(&archive);
    assert!(names.iter().any(|name| name == BACKUP_DB_NAME));
    assert!(!names.iter().any(|name| name.ends_with(".db-wal")));

    let destination = TempDir::new("backup-wal-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).expect("import");

    let restored = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    assert_eq!(restored.clients().count().unwrap(), expected.len() as i64);
    for uuid in expected {
        assert!(restored.clients().get(&uuid).unwrap().is_some());
    }
}

#[test]
fn path_traversal_is_rejected_without_touching_existing_data() {
    let source = TempDir::new("backup-slip-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("../evil.txt", b"pwned"),
    ]);

    let destination = TempDir::new("backup-slip-destination");
    let target = sentinel_target(&destination);
    let error = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(matches!(error, DbError::UnsafeBackupPath(_)), "{error}");
    assert_sentinel(&target);
    assert!(!destination.join("evil.txt").exists());
}

#[test]
fn corrupt_zip_is_rejected() {
    let destination = TempDir::new("backup-corrupt-zip");
    let target = sentinel_target(&destination);
    let error = import_backup_bytes(&options(&target), b"this is not a zip file").unwrap_err();
    assert!(matches!(error, DbError::Zip(_)), "{error}");
    assert_sentinel(&target);
}

#[test]
fn missing_marker_is_rejected_unless_disabled() {
    let source = TempDir::new("backup-marker-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[(BACKUP_DB_NAME, &database)]);
    let destination = TempDir::new("backup-marker-destination");
    let target = sentinel_target(&destination);
    let error = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(matches!(error, DbError::InvalidBackup(_)), "{error}");
    assert_sentinel(&target);

    let mut relaxed = options(&target);
    relaxed.require_markup = false;
    import_backup_bytes(&relaxed, &archive).expect("marker is optional");
    assert!(target.join(BACKUP_DB_NAME).is_file());
}

#[test]
fn old_schema_is_rejected_by_default_and_accepted_explicitly() {
    let source = TempDir::new("backup-legacy-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    db.with_connection(|conn| {
        conn.execute_batch("DROP TABLE theme_configurations")?;
        Ok(())
    })
    .unwrap();
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[(BACKUP_MARKUP_NAME, MARKER), (BACKUP_DB_NAME, &database)]);

    let strict_destination = TempDir::new("backup-legacy-strict");
    let strict_target = sentinel_target(&strict_destination);
    let error = import_backup_bytes(&options(&strict_target), &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupSchema(_)), "{error}");
    assert_sentinel(&strict_target);

    let relaxed_destination = TempDir::new("backup-legacy-relaxed");
    let relaxed_target = relaxed_destination.join("data");
    let mut relaxed = options(&relaxed_target);
    relaxed.allow_legacy_schema = true;
    let report = import_backup_bytes(&relaxed, &archive).expect("legacy schema allowed");
    assert!(report.legacy_schema);

    let restored = Db::open(relaxed_target.join(BACKUP_DB_NAME)).unwrap();
    assert!(!restored.table_exists("theme_configurations").unwrap());
    restored.migrate().expect("migrate adds missing tables");
    assert!(restored.schema_is_current().unwrap());
}

#[test]
fn oversized_entry_is_rejected() {
    let source = TempDir::new("backup-large-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let big = vec![0u8; database.len() + 4096];
    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("big.bin", &big),
    ]);

    let destination = TempDir::new("backup-large-destination");
    let target = sentinel_target(&destination);
    let mut strict = options(&target);
    strict.max_entry_uncompressed_bytes = (database.len() + 16) as u64;
    let error = import_backup_bytes(&strict, &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert_sentinel(&target);
}

#[test]
fn too_many_entries_are_rejected() {
    let source = TempDir::new("backup-entries-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("a.txt", b"a"),
        ("b.txt", b"b"),
    ]);

    let destination = TempDir::new("backup-entries-destination");
    let target = sentinel_target(&destination);
    let mut limited = options(&target);
    limited.max_entries = 2;
    let error = import_backup_bytes(&limited, &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert_sentinel(&target);
}

#[test]
fn invalid_database_rolls_back_the_import() {
    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, b"this is not sqlite"),
        ("theme/main.css", b"new"),
    ]);

    let destination = TempDir::new("backup-rollback");
    let target = sentinel_target(&destination);
    fs::write(target.join("theme.css"), b"old").unwrap();

    let error = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupIntegrity(_)), "{error}");
    assert_sentinel(&target);
    assert_eq!(fs::read(target.join("theme.css")).unwrap(), b"old");
    assert!(!target.join("theme/main.css").exists());
}

#[test]
fn repeated_import_replaces_existing_data() {
    let source = TempDir::new("backup-repeat-source");
    let data = make_data_dir(&source);
    fs::write(data.join("version.txt"), b"v1").unwrap();
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-repeat-destination");
    let target = destination.join("data");
    let first = import_backup_bytes(&options(&target), &archive).expect("first import");
    assert!(!first.replaced_existing);

    fs::write(target.join("stale.txt"), b"stale").unwrap();
    let second = import_backup_bytes(&options(&target), &archive).expect("second import");
    assert!(second.replaced_existing);
    assert!(!target.join("stale.txt").exists());
    assert_eq!(fs::read(target.join("version.txt")).unwrap(), b"v1");
    assert!(
        Db::open(target.join(BACKUP_DB_NAME))
            .unwrap()
            .schema_is_current()
            .unwrap()
    );
}

#[test]
fn optional_pre_restore_archive_captures_the_previous_data() {
    let source = TempDir::new("backup-pre-source");
    let data = make_data_dir(&source);
    fs::write(data.join("version.txt"), b"v1").unwrap();
    let db = make_db(&data);
    db.config().set("sitename", "before").unwrap();
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-pre-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).expect("initial import");

    fs::write(target.join("new.txt"), b"added later").unwrap();
    fs::write(target.join("scratch.tmp"), b"keep me too").unwrap();
    let live = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    live.config().set("sitename", "after").unwrap();
    drop(live);

    let backup_dir = destination.join("backup");
    let mut restore_options = options(&target);
    // The restore itself only needs the database, but the pre-restore archive
    // must still capture the whole old data directory.
    restore_options.include_data = false;
    // The pre-restore snapshot must ignore these exclusions and keep transient
    // files, unlike a normal export.
    restore_options.exclude = vec![target.join("scratch.tmp")];
    restore_options.pre_restore_backup_dir = Some(backup_dir.clone());
    let report = import_backup_bytes(&restore_options, &archive).expect("restore");

    let pre_restore = report.pre_restore_archive.expect("pre-restore archive");
    assert!(pre_restore.starts_with(&backup_dir));
    let pre_bytes = fs::read(&pre_restore).unwrap();
    let names = zip_names(&pre_bytes);
    assert!(names.iter().any(|name| name == "new.txt"));
    assert!(names.iter().any(|name| name == "scratch.tmp"));
    assert!(names.iter().any(|name| name == BACKUP_DB_NAME));

    let extracted = TempDir::new("backup-pre-extracted");
    let database = read_entry(&pre_bytes, BACKUP_DB_NAME).unwrap();
    let database_path = extracted.join("komari.db");
    fs::write(&database_path, database).unwrap();
    let old = Db::open_read_only(&database_path).unwrap();
    let sitename: Option<String> = old.config().get_as("sitename").unwrap();
    assert_eq!(sitename.as_deref(), Some("after"));

    let restored = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    let sitename: Option<String> = restored.config().get_as("sitename").unwrap();
    assert_eq!(sitename.as_deref(), Some("before"));
    assert!(!target.join("new.txt").exists());
}

#[test]
fn concurrent_imports_are_serialized_per_data_directory() {
    let source = TempDir::new("backup-concurrent-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-concurrent-destination");
    let target = destination.join("data");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let thread_target = target.clone();
    let thread_archive = archive.clone();
    let handle = thread::spawn(move || {
        let reader = BlockingReader {
            inner: Cursor::new(thread_archive),
            started: Some(started_tx),
            release: release_rx,
            blocked: false,
        };
        import_backup(&options(&thread_target), reader).map(|_| ())
    });

    // The first import holds the per-directory lock while blocked in Seek.
    started_rx.recv().expect("first import acquired the lock");
    let busy = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(matches!(busy, DbError::BackupInProgress), "{busy}");

    release_tx.send(()).expect("release first import");
    handle
        .join()
        .expect("first import thread")
        .expect("first import succeeds");

    let restored = Db::open(target.join(BACKUP_DB_NAME)).expect("restored database");
    assert!(restored.schema_is_current().unwrap());
}

#[test]
fn export_excludes_database_files_temporary_files_and_itself() {
    let source = TempDir::new("backup-exclude-source");
    let data = make_data_dir(&source);
    fs::create_dir_all(data.join("theme")).unwrap();
    fs::write(data.join("theme/main.css"), b"css").unwrap();
    fs::write(data.join("uploads.bin"), b"upload").unwrap();
    fs::write(data.join("scratch.tmp"), b"temp").unwrap();
    fs::write(data.join("backup.zip"), b"old backup").unwrap();
    fs::write(data.join("komari.db-wal"), b"wal").unwrap();
    fs::write(data.join("komari.db-shm"), b"shm").unwrap();
    fs::write(data.join(BACKUP_MARKUP_NAME), b"old marker").unwrap();
    fs::create_dir_all(data.join("archive.db")).unwrap();
    fs::write(data.join("archive.db/child.txt"), b"child").unwrap();

    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let names = zip_names(&archive);
    assert!(names.iter().any(|name| name == "komari.db"));
    assert!(names.iter().any(|name| name == "theme/main.css"));
    assert!(names.iter().any(|name| name == "uploads.bin"));
    assert!(names.iter().any(|name| name == "theme/"));
    assert!(!names.iter().any(|name| name == "scratch.tmp"));
    assert!(!names.iter().any(|name| name == "backup.zip"));
    assert!(!names.iter().any(|name| name.ends_with(".db-wal")));
    assert!(!names.iter().any(|name| name.ends_with(".db-shm")));
    assert!(names.iter().any(|name| name == "archive.db/child.txt"));
    assert!(!names.iter().any(|name| name == "archive.db/"));
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == BACKUP_MARKUP_NAME)
            .count(),
        1
    );

    let destination = TempDir::new("backup-exclude-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).unwrap();
    assert_eq!(fs::read(target.join("theme/main.css")).unwrap(), b"css");
    assert_eq!(
        fs::read(target.join("archive.db/child.txt")).unwrap(),
        b"child"
    );
    assert!(!target.join("scratch.tmp").exists());
    assert!(!target.join(BACKUP_MARKUP_NAME).exists());
}

#[test]
fn export_to_file_excludes_the_destination_archive() {
    let source = TempDir::new("backup-destination-source");
    let data = make_data_dir(&source);
    fs::create_dir_all(data.join("theme")).unwrap();
    fs::write(data.join("theme/main.css"), b"css").unwrap();

    let db = make_db(&data);
    let destination = data.join("export.zip");
    let report = db
        .export_backup_to(&options(&data), &destination)
        .expect("export to file");
    assert_eq!(report.data_entries, 1);
    drop(db);

    let bytes = fs::read(&destination).unwrap();
    let names = zip_names(&bytes);
    assert!(!names.iter().any(|name| name == "export.zip"));
    assert!(
        !names
            .iter()
            .any(|name| name.contains(".komari-backup-export-"))
    );
    assert!(names.iter().any(|name| name == "theme/main.css"));

    let restored_dir = TempDir::new("backup-destination-restored");
    let restored = restored_dir.join("data");
    import_backup_file(&options(&restored), &destination).unwrap();
    assert_eq!(fs::read(restored.join("theme/main.css")).unwrap(), b"css");
}

#[test]
fn export_refuses_to_overwrite_the_live_database() {
    let source = TempDir::new("backup-overwrite-db");
    let data = make_data_dir(&source);
    let db = make_db(&data);

    for destination in [
        data.join(BACKUP_DB_NAME),
        data.join("komari.db-wal"),
        data.join("komari.db-shm"),
    ] {
        let error = db
            .export_backup_to(&options(&data), &destination)
            .unwrap_err();
        assert!(matches!(error, DbError::InvalidData(_)), "{error}");
    }

    assert!(
        Db::open(data.join(BACKUP_DB_NAME))
            .unwrap()
            .schema_is_current()
            .unwrap()
    );
}

#[test]
fn marker_uses_go_rfc3339_second_precision_without_newline() {
    let source = TempDir::new("backup-marker-format");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();

    let marker = read_entry(&archive, BACKUP_MARKUP_NAME).expect("marker");
    let text = String::from_utf8(marker).unwrap();
    assert!(!text.ends_with('\n'));
    let timestamp = text.rsplit(": ").next().unwrap();
    assert!(timestamp.contains('T'));
    assert!(!timestamp.contains('.'), "{timestamp}");
    if let Some(prefix) = timestamp.strip_suffix('Z') {
        assert_eq!(prefix.len(), 19, "{timestamp}");
    } else {
        assert_eq!(timestamp.len(), 25, "{timestamp}");
        let bytes = timestamp.as_bytes();
        assert!(bytes[19] == b'+' || bytes[19] == b'-', "{timestamp}");
        assert_eq!(bytes[22], b':', "{timestamp}");
    }
}

#[test]
fn symlink_zip_entry_is_rejected() {
    let source = TempDir::new("backup-symlink-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip_with_symlink(&database);

    let destination = TempDir::new("backup-symlink-destination");
    let target = sentinel_target(&destination);
    let error = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(matches!(error, DbError::UnsafeBackupPath(_)), "{error}");
    assert_sentinel(&target);
}

#[test]
fn encrypted_zip_entry_is_rejected() {
    let source = TempDir::new("backup-encrypted-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = mark_entries_encrypted(build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
    ]));

    let destination = TempDir::new("backup-encrypted-destination");
    let target = sentinel_target(&destination);
    let error = import_backup_bytes(&options(&target), &archive).unwrap_err();
    assert!(
        matches!(error, DbError::Zip(_) | DbError::InvalidBackup(_)),
        "{error}"
    );
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("encrypt") || message.contains("password"),
        "{error}"
    );
    assert_sentinel(&target);
}

#[test]
fn archive_and_total_size_limits_are_enforced() {
    let source = TempDir::new("backup-limits-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("payload.bin", b"payload"),
    ]);

    let destination = TempDir::new("backup-limits-destination");
    let target = sentinel_target(&destination);

    let mut archive_limit = options(&target);
    archive_limit.max_archive_bytes = (archive.len() - 1) as u64;
    let error = import_backup_bytes(&archive_limit, &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert_sentinel(&target);

    let mut total_limit = options(&target);
    total_limit.max_total_uncompressed_bytes = database.len() as u64;
    let error = import_backup_bytes(&total_limit, &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert_sentinel(&target);
}

#[cfg(unix)]
#[test]
fn symlinked_data_dir_is_rejected() {
    let root = TempDir::new("backup-symlink-dir");
    let real = root.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = root.join("data");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let error = import_backup_bytes(&options(&link), b"unused").unwrap_err();
    assert!(matches!(error, DbError::InvalidData(_)), "{error}");
    assert!(real.is_dir());
}

#[test]
fn filesystem_root_is_rejected_as_data_dir() {
    let error = import_backup_bytes(&BackupOptions::new("/"), b"unused").unwrap_err();
    assert!(matches!(error, DbError::InvalidData(_)), "{error}");
}

#[cfg(unix)]
#[test]
fn non_utf8_file_names_are_skipped_without_failing_export() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let source = TempDir::new("backup-non-utf8-source");
    let data = make_data_dir(&source);
    let bad = data.join(OsString::from_vec(b"bad\xff.txt".to_vec()));
    fs::write(&bad, b"bad").unwrap();
    fs::write(data.join("good.txt"), b"good").unwrap();

    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let names = zip_names(&archive);
    assert!(names.iter().any(|name| name == "good.txt"));
    assert!(!names.iter().any(|name| name.starts_with("bad")));

    let destination = TempDir::new("backup-non-utf8-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).unwrap();
    assert_eq!(fs::read(target.join("good.txt")).unwrap(), b"good");
}

#[test]
fn duplicate_entry_names_are_deduplicated_by_the_zip_reader() {
    let source = TempDir::new("backup-duplicate-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("theme/a.txt", b"first"),
        ("theme/b.txt", b"second"),
    ]);
    // Same-length rename in the central directory creates a duplicate name
    // that the zip writer itself refuses to produce.
    let archive = duplicate_central_directory_name(archive, "theme/b.txt", "theme/a.txt");

    let destination = TempDir::new("backup-duplicate-destination");
    let target = destination.join("data");
    let report = import_backup_bytes(&options(&target), &archive)
        .expect("the zip reader de-duplicates central-directory names");
    assert_eq!(report.data_entries, 1);
    assert!(target.join("theme/a.txt").is_file());
}

#[test]
fn restore_pending_backup_mirrors_go_startup_restore() {
    let source = TempDir::new("backup-pending-source");
    let data = make_data_dir(&source);
    fs::write(data.join("version.txt"), b"v1").unwrap();
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-pending-destination");
    let target = destination.join("data");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("backup.zip"), &archive).unwrap();

    let report = restore_pending_backup(&options(&target))
        .expect("restore")
        .expect("pending archive");
    assert!(report.replaced_existing);
    assert!(!target.join("backup.zip").exists());
    assert_eq!(fs::read(target.join("version.txt")).unwrap(), b"v1");
    assert!(
        Db::open(target.join(BACKUP_DB_NAME))
            .unwrap()
            .schema_is_current()
            .unwrap()
    );

    assert!(
        restore_pending_backup(&options(&destination.join("missing")))
            .unwrap()
            .is_none()
    );
}

#[test]
fn max_compression_ratio_is_enforced() {
    let source = TempDir::new("backup-ratio-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let zeros = vec![0u8; 1024 * 1024];
    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("zeros.bin", &zeros),
    ]);

    let destination = TempDir::new("backup-ratio-destination");
    let target = sentinel_target(&destination);
    let mut limited = options(&target);
    // Above the SQLite snapshot's ratio, below the zero-filled file's ratio.
    limited.max_compression_ratio = 50;
    let error = import_backup_bytes(&limited, &archive).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert!(error.to_string().contains("zeros.bin"), "{error}");
    assert_sentinel(&target);
}

#[test]
fn pre_restore_directory_inside_data_dir_is_skipped() {
    let source = TempDir::new("backup-pre-inside-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-pre-inside-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).expect("initial import");

    let mut restore_options = options(&target);
    restore_options.pre_restore_backup_dir = Some(target.join("backup"));
    let report = import_backup_bytes(&restore_options, &archive).expect("restore");
    assert!(report.pre_restore_archive.is_none());
    assert!(!target.join("backup").exists());
}

#[cfg(unix)]
#[test]
fn pre_restore_symlink_into_data_dir_is_skipped() {
    let source = TempDir::new("backup-pre-symlink-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-pre-symlink-destination");
    let target = destination.join("data");
    import_backup_bytes(&options(&target), &archive).expect("initial import");
    fs::create_dir_all(target.join("inner")).unwrap();

    let link = destination.join("backup-link");
    std::os::unix::fs::symlink(target.join("inner"), &link).unwrap();

    let mut restore_options = options(&target);
    restore_options.pre_restore_backup_dir = Some(link);
    let report = import_backup_bytes(&restore_options, &archive).expect("restore");
    assert!(report.pre_restore_archive.is_none());
}

#[test]
fn restore_pending_removes_backup_zip_extracted_from_the_archive() {
    let source = TempDir::new("backup-pending-nested-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let database = snapshot_bytes(&db);
    drop(db);

    let archive = build_zip(&[
        (BACKUP_MARKUP_NAME, MARKER),
        (BACKUP_DB_NAME, &database),
        ("backup.zip", b"nested pending archive"),
    ]);

    let destination = TempDir::new("backup-pending-nested-destination");
    let target = destination.join("data");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("backup.zip"), &archive).unwrap();

    restore_pending_backup(&options(&target))
        .expect("restore")
        .expect("pending archive");
    assert!(!target.join("backup.zip").exists());
    assert!(
        Db::open(target.join(BACKUP_DB_NAME))
            .unwrap()
            .schema_is_current()
            .unwrap()
    );
}

#[test]
fn restore_pending_rejects_an_oversized_pending_archive() {
    let source = TempDir::new("backup-pending-large-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-pending-large-destination");
    let target = sentinel_target(&destination);
    fs::write(target.join("backup.zip"), &archive).unwrap();

    let mut limited = options(&target);
    limited.max_archive_bytes = (archive.len() - 1) as u64;
    let error = restore_pending_backup(&limited).unwrap_err();
    assert!(matches!(error, DbError::BackupLimit(_)), "{error}");
    assert!(target.join("backup.zip").is_file());
    assert_sentinel(&target);
}

#[cfg(unix)]
#[test]
fn export_refuses_symlinked_database_destinations() {
    let source = TempDir::new("backup-symlink-destination-source");
    let data = make_data_dir(&source);
    let db = make_db(&data);

    let link_dir = source.join("link");
    std::os::unix::fs::symlink(&data, &link_dir).unwrap();
    let error = db
        .export_backup_to(&options(&data), link_dir.join(BACKUP_DB_NAME))
        .unwrap_err();
    assert!(matches!(&error, DbError::InvalidData(_)), "{error}");

    let link_file = source.join("db-link");
    std::os::unix::fs::symlink(data.join(BACKUP_DB_NAME), &link_file).unwrap();
    let error = db
        .export_backup_to(&options(&data), &link_file)
        .unwrap_err();
    assert!(matches!(&error, DbError::InvalidData(_)), "{error}");

    assert!(
        Db::open(data.join(BACKUP_DB_NAME))
            .unwrap()
            .schema_is_current()
            .unwrap()
    );
}

#[test]
fn import_rejects_a_database_path_outside_the_data_dir() {
    let destination = TempDir::new("backup-outside-db");
    let target = destination.join("data");
    let mut options = options(&target);
    options.database_path = Some(destination.join("elsewhere.db"));

    let error = import_backup_bytes(&options, b"unused").unwrap_err();
    assert!(matches!(error, DbError::InvalidData(_)), "{error}");
    assert!(!target.exists());
}
