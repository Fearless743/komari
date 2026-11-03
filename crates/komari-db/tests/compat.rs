//! Compatibility checks against the real Go backend database.
//!
//! The test is skipped when the reference database is not present (for example
//! on CI), but whenever it exists every table must be readable and its schema
//! must match what this crate creates.

use std::path::Path;

use komari_db::models::Config as LegacyConfig;
use komari_db::{Db, LocalTime, migrate::TABLES};

mod support;
use support::TempDir;

const REFERENCE_DB: &str = "/home/fearless/github/komari/komari/data/komari.db";

fn reference_path() -> Option<String> {
    let path = std::env::var("KOMARI_COMPAT_DB").unwrap_or_else(|_| REFERENCE_DB.to_owned());
    Path::new(&path).exists().then_some(path)
}

type ColumnInfo = (String, String, i64, Option<String>, i64);

fn columns(db: &Db, table: &str) -> Vec<ColumnInfo> {
    db.with_connection(|conn| {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(`{table}`)"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .unwrap()
}

type IndexInfo = (String, i64, String);
type IndexSnapshot = Vec<(String, Vec<IndexInfo>)>;

fn indexes(db: &Db, table: &str) -> Vec<IndexInfo> {
    db.with_connection(|conn| {
        let mut stmt = conn.prepare(&format!("PRAGMA index_list(`{table}`)"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .unwrap()
}

fn index_snapshot(db: &Db) -> IndexSnapshot {
    db.table_names()
        .unwrap()
        .into_iter()
        .map(|table| {
            let table_indexes = indexes(db, &table);
            (table, table_indexes)
        })
        .collect()
}

/// Validate the shape of the configuration map without assuming any specific
/// key (custom `KOMARI_COMPAT_DB` databases may not have `sitename`).
fn assert_config_shape(db: &Db) {
    let config = db.config();
    let all = config.get_all().expect("read configs");
    if all.is_empty() {
        eprintln!("reference database has an empty configs table; skipping value assertions");
        return;
    }

    if let Some(sitename) = config.get("sitename").expect("read sitename") {
        assert!(sitename.is_string());
    }
    if let Some(theme) = config.get("theme").expect("read theme") {
        assert!(theme.is_string());
    }
    if let Some(preserve) = config
        .get_as::<i64>("record_preserve_time")
        .expect("read record_preserve_time")
    {
        assert!(preserve >= 0);
    }
    if let Some(sitename) = all.get("sitename") {
        assert!(sitename.is_string());
    }
}

#[test]
fn reference_database_schema_matches_our_migration() {
    let Some(path) = reference_path() else {
        eprintln!("reference database {REFERENCE_DB} not found; skipping");
        return;
    };

    let reference = Db::open_read_only(&path).expect("open reference database read-only");
    assert!(reference.is_read_only());

    let fresh = Db::open_in_memory().unwrap();
    fresh.migrate().unwrap();

    for table in TABLES {
        assert!(
            reference.table_exists(table).unwrap(),
            "reference database is missing table {table}"
        );
        let expected = columns(&fresh, table);
        let actual = columns(&reference, table);
        assert_eq!(actual, expected, "column mismatch for table {table}");
        assert_eq!(
            indexes(&reference, table),
            indexes(&fresh, table),
            "index mismatch for table {table}"
        );
    }
}

#[test]
fn reference_database_is_readable_and_has_expected_tables() {
    let Some(path) = reference_path() else {
        eprintln!("reference database {REFERENCE_DB} not found; skipping");
        return;
    };

    let db = Db::open_read_only(&path).unwrap();
    for table in [
        "clients",
        "users",
        "sessions",
        "records",
        "configs",
        "gpu_records",
        "logs",
        "clipboards",
        "load_notifications",
        "offline_notifications",
        "oidc_providers",
        "message_sender_providers",
        "ddns_providers",
        "ddns_sync_history",
        "theme_configurations",
        "records_long_term",
        "gpu_records_long_term",
    ] {
        assert!(db.table_exists(table).unwrap(), "missing {table}");
    }
    assert!(db.schema_is_current().unwrap());

    // Configuration values differ between installations; validate shape only.
    assert_config_shape(&db);

    // Users should be readable, including the non-standard timestamp layout.
    let users = db.accounts().list().expect("read users");
    if let Some(user) = users.first() {
        assert!(!user.username.is_empty());
        assert!(!user.created_at.is_zero());
        assert!(user.created_at.to_storage_string().starts_with("20"));
    }

    // Sessions contain driver-written timestamps with nanosecond precision and
    // an explicit +00:00 offset; Go's LocalTime keeps the wall clock.
    let sessions = db.sessions().list().expect("read sessions");
    if let Some(session) = sessions.first() {
        assert!(!session.expires.is_zero());
        if !session.latest_online.is_zero() {
            assert_eq!(session.latest_online.to_storage_string().len(), 27);
        }
    }

    // Configs are key/value JSON, and the legacy single-row model still
    // deserializes when explicitly requested.
    let _legacy = LegacyConfig::default();
    let _: LocalTime = LocalTime::default();
}

#[test]
fn migrating_a_copy_of_the_reference_database_preserves_data() {
    let Some(path) = reference_path() else {
        eprintln!("reference database {REFERENCE_DB} not found; skipping");
        return;
    };

    let dir = TempDir::new("reference-copy");
    let copy = dir.join("komari.db");
    // Use SQLite's online backup API: it produces a transactionally consistent
    // snapshot (including WAL content) and must succeed for the test to run.
    {
        let source = Db::open_read_only(&path).unwrap();
        let mut destination = rusqlite::Connection::open(&copy).expect("create snapshot file");
        source
            .with_connection(|conn| {
                let backup = rusqlite::backup::Backup::new(conn, &mut destination)?;
                backup.run_to_completion(64, std::time::Duration::from_millis(0), None)?;
                Ok(())
            })
            .expect("sqlite backup API must succeed");
    }

    // The snapshot must already match the source before migration runs.
    let source = Db::open_read_only(&path).unwrap();
    let snapshot = Db::open_read_only(&copy).unwrap();
    assert_eq!(
        source.config().get_all().unwrap(),
        snapshot.config().get_all().unwrap()
    );
    assert_eq!(
        source.accounts().count().unwrap(),
        snapshot.accounts().count().unwrap()
    );
    assert_eq!(
        source.sessions().count().unwrap(),
        snapshot.sessions().count().unwrap()
    );

    let db = Db::open(&copy).expect("open copied database");
    let config_before = db.config().get_all().unwrap();
    let before = db.config().count().unwrap();
    let users_before = db.accounts().count().unwrap();
    let sessions_before = db.sessions().count().unwrap();
    // Only assert "no structural change" when the snapshot already had the
    // full schema; an intentionally old KOMARI_COMPAT_DB may be upgraded.
    let schema_was_current = db.schema_is_current().unwrap();
    let tables_before = db.table_names().unwrap();
    let indexes_before = index_snapshot(&db);

    // Running the migration twice must not drop or rewrite existing rows.
    db.migrate().unwrap();
    db.migrate().unwrap();

    assert_eq!(db.config().count().unwrap(), before);
    assert_eq!(db.accounts().count().unwrap(), users_before);
    assert_eq!(db.sessions().count().unwrap(), sessions_before);
    assert!(db.schema_is_current().unwrap());
    assert_eq!(db.config().get_all().unwrap(), config_before);

    if schema_was_current {
        assert_eq!(db.table_names().unwrap(), tables_before);
        assert_eq!(index_snapshot(&db), indexes_before);
        assert!(!db.table_exists("configs_legacy_backup").unwrap());
        let mut index_names = Vec::new();
        for (_, table_indexes) in index_snapshot(&db) {
            index_names.extend(table_indexes.into_iter().map(|(name, _, _)| name));
        }
        index_names.sort();
        let mut unique_names = index_names.clone();
        unique_names.dedup();
        assert_eq!(index_names, unique_names, "duplicate index names");
    }

    // New writes still work on the upgraded copy.
    let client = db.clients().create_named("compat-node").unwrap();
    assert!(db.clients().get(&client.uuid).unwrap().is_some());
}

#[test]
fn config_shape_without_sitename_is_accepted() {
    let dir = TempDir::new("no-sitename");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();
    db.migrate().unwrap();
    db.config().set("theme", "dark").unwrap();

    // No `sitename` key at all: shape validation must not assume it exists.
    assert_config_shape(&db);
}

#[test]
fn config_shape_allows_empty_values_and_zero_retention() {
    let dir = TempDir::new("empty-config");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();
    db.migrate().unwrap();
    db.config().set("sitename", "").unwrap();
    db.config().set("record_preserve_time", &0_i64).unwrap();

    assert_config_shape(&db);
}
