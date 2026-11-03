//! Backup export/import coverage: round trips, WAL snapshots, ZIP safety,
//! limits, schema checks, rollback and concurrency.

mod support;

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use komari_db::{
    BACKUP_DB_NAME, BACKUP_MARKUP_NAME, BackupOptions, Db, DbError, import_backup_bytes,
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
    let live = Db::open(target.join(BACKUP_DB_NAME)).unwrap();
    live.config().set("sitename", "after").unwrap();
    drop(live);

    let backup_dir = destination.join("backup");
    let mut restore_options = options(&target);
    restore_options.pre_restore_backup_dir = Some(backup_dir.clone());
    let report = import_backup_bytes(&restore_options, &archive).expect("restore");

    let pre_restore = report.pre_restore_archive.expect("pre-restore archive");
    assert!(pre_restore.starts_with(&backup_dir));
    let pre_bytes = fs::read(&pre_restore).unwrap();
    let names = zip_names(&pre_bytes);
    assert!(names.iter().any(|name| name == "new.txt"));
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
fn concurrent_imports_do_not_corrupt_the_target() {
    let source = TempDir::new("backup-concurrent-source");
    let data = make_data_dir(&source);
    fs::create_dir_all(data.join("files")).unwrap();
    for index in 0..500 {
        fs::write(
            data.join(format!("files/{index}.bin")),
            vec![index as u8; 512],
        )
        .unwrap();
    }
    let db = make_db(&data);
    let archive = db.export_backup(&options(&data)).unwrap();
    drop(db);

    let destination = TempDir::new("backup-concurrent-destination");
    let target = destination.join("data");
    let archive = Arc::new(archive);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();

    for _ in 0..2 {
        let archive = Arc::clone(&archive);
        let barrier = Arc::clone(&barrier);
        let target = target.clone();
        handles.push(thread::spawn(move || {
            let options = options(&target);
            barrier.wait();
            import_backup_bytes(&options, &archive).map(|_| ())
        }));
    }

    let mut successes = 0;
    for handle in handles {
        match handle.join().expect("thread") {
            Ok(()) => successes += 1,
            Err(DbError::BackupInProgress) => {}
            Err(error) => panic!("unexpected import error: {error}"),
        }
    }
    assert!(successes >= 1);
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
    import_backup_bytes(&options(&restored), &bytes).unwrap();
    assert_eq!(fs::read(restored.join("theme/main.css")).unwrap(), b"css");
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
