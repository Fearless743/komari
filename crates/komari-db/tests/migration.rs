//! Migration behaviour: idempotence, file creation and read-only opens.

mod support;

use komari_db::{Db, LocalTime};
use support::{TempDir, memory_db};

#[test]
fn migration_can_run_twice_in_memory() {
    let db = memory_db();
    db.migrate().expect("second migration must succeed");
    assert!(db.schema_is_current().unwrap());

    let tables = db.table_names().unwrap();
    for expected in [
        "clients",
        "users",
        "sessions",
        "records",
        "gpu_records",
        "configs",
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
        assert!(
            tables.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn migration_adds_columns_before_building_indexes() {
    let dir = TempDir::new("old-indexed");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    // Old tables missing indexed columns: records.client, gpu_records.client
    // and gpu_records.device_index.
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE records (\
                 `time` datetime, `cpu` decimal(5,2), `ram` bigint\
             );\
             INSERT INTO records (time, cpu, ram) VALUES ('2026-01-01 00:00:00.0000000', 1.5, 42);\
             CREATE TABLE gpu_records (\
                 `time` datetime, `device_name` varchar(100)\
             );\
             INSERT INTO gpu_records (time, device_name) VALUES ('2026-01-01 00:00:00.0000000', 'GPU');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate()
        .expect("indexes must be created after missing columns");

    let records = db.records().all_in(komari_db::RecordTable::Live).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].client, "");
    assert_eq!(records[0].cpu, 1.5);

    let gpu = db
        .records()
        .gpu_by_client_and_time_in(
            komari_db::RecordTable::Live,
            "",
            LocalTime::parse("2025-01-01").unwrap(),
            LocalTime::parse("2027-01-01").unwrap(),
        )
        .unwrap();
    assert_eq!(gpu.len(), 1);
    assert_eq!(gpu[0].device_index, 0);

    assert!(db.schema_is_current().unwrap());
}

#[test]
fn migration_adds_missing_columns_without_losing_data() {
    let dir = TempDir::new("old-schema");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    // Simulate a database created before newer fields were added.
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name) VALUES ('old-client', 'old-token', 'legacy');\
             CREATE TABLE users (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `username` varchar(50) NOT NULL, \
                 `passwd` varchar(255) NOT NULL\
             );\
             INSERT INTO users (uuid, username, passwd) VALUES ('old-user', 'admin', 'hash');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let client = db.clients().require("old-client").unwrap();
    assert_eq!(client.name, "legacy");
    assert_eq!(client.token, "old-token");
    assert_eq!(client.currency, "$");
    assert_eq!(client.traffic_limit_type, "max");
    assert_eq!(client.tags, "");
    assert_eq!(client.mem_total, 0);
    assert!(client.created_at.is_zero());

    let user = db.accounts().require_by_uuid("old-user").unwrap();
    assert_eq!(user.username, "admin");
    assert_eq!(user.sso_type, "");
    assert_eq!(user.two_factor, "");

    // New columns are writable after the additive migration.
    db.clients()
        .patch(
            "old-client",
            &komari_db::ClientPatch {
                tags: Some("prod;db".to_owned()),
                ..komari_db::ClientPatch::default()
            },
        )
        .unwrap();
    assert_eq!(db.clients().require("old-client").unwrap().tags, "prod;db");

    // The second migration must be a no-op.
    db.migrate().unwrap();
    assert!(db.schema_is_current().unwrap());
}

#[test]
fn migration_is_idempotent_on_disk_and_preserves_data() {
    let dir = TempDir::new("idempotent");
    let path = dir.join("komari.db");

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.config().set("sitename", "Komari").unwrap();
        db.clients().create_named("node-1").unwrap();
    }

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
        assert!(db.schema_is_current().unwrap());
        assert_eq!(
            db.config().get_as::<String>("sitename").unwrap().as_deref(),
            Some("Komari")
        );
        assert_eq!(db.clients().count().unwrap(), 1);
    }
}

#[test]
fn opening_a_missing_file_creates_it_and_its_parent() {
    let dir = TempDir::new("create");
    let path = dir.join("nested/komari.db");
    assert!(!path.exists());

    let db = Db::open(&path).unwrap();
    db.migrate().unwrap();
    assert!(path.exists());
    assert!(db.schema_is_current().unwrap());
}

#[test]
fn read_only_handle_can_inspect_but_not_write() {
    let dir = TempDir::new("readonly");
    let path = dir.join("komari.db");

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.config().set("theme", "default").unwrap();
    }

    let db = Db::open_read_only(&path).unwrap();
    assert!(db.is_read_only());
    assert!(db.schema_is_current().unwrap());
    assert_eq!(
        db.config().get_as::<String>("theme").unwrap().as_deref(),
        Some("default")
    );
    assert!(db.config().set("theme", "dark").is_err());
}

#[test]
fn database_options_read_environment_names() {
    use komari_db::DbOptions;
    use std::collections::HashMap;

    let values: HashMap<&str, &str> = HashMap::from([
        ("KOMARI_DB_TYPE", "sqlite"),
        ("KOMARI_DB_FILE", "/tmp/x.db"),
    ]);
    let options =
        DbOptions::from_env_with(|key| values.get(key).map(|value| (*value).to_owned())).unwrap();
    assert_eq!(options.path, std::path::PathBuf::from("/tmp/x.db"));
    assert!(!options.read_only);

    let options = DbOptions::from_env_with(|_| None).unwrap();
    assert_eq!(
        options.path,
        std::path::PathBuf::from(komari_db::DEFAULT_DB_FILE)
    );

    let error =
        DbOptions::from_env_with(|key| (key == "KOMARI_DB_TYPE").then(|| "mysql".to_owned()))
            .unwrap_err();
    assert!(matches!(error, komari_db::DbError::UnsupportedDatabase(_)));

    // Creating an explicit database still works in a temp directory.
    let dir = TempDir::new("options");
    let db = Db::open_with(DbOptions::new(dir.join("env.db"))).unwrap();
    db.migrate().unwrap();
    assert!(db.table_exists("clients").unwrap());
}
