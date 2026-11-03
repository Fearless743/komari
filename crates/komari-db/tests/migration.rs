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
fn legacy_sessions_without_expiry_are_expired() {
    let dir = TempDir::new("old-sessions");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE sessions (\
                 `uuid` varchar(36), \
                 `session` varchar(255) PRIMARY KEY, \
                 `user_agent` text, \
                 `latest_online` timestamp\
             );\
             INSERT INTO sessions (uuid, session, user_agent) VALUES ('user', 'old-session', 'agent');\
             INSERT INTO sessions (uuid, session, user_agent) VALUES ('user', 'null-expiry', 'agent');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    // The migration backfills a past timestamp for the newly added column.
    let migrated = db.sessions().get("old-session").unwrap().unwrap();
    assert_eq!(
        migrated.expires.to_storage_string(),
        "1970-01-01 00:00:00.0000000"
    );

    // `expires` is nullable on this legacy table, so exercise the NULL deletion
    // path as well.
    db.with_connection(|conn| {
        conn.execute(
            "UPDATE sessions SET expires = NULL WHERE session = 'null-expiry'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        db.sessions()
            .delete_expired_before(LocalTime::now())
            .unwrap(),
        2
    );

    assert!(db.sessions().get("old-session").unwrap().is_none());
    assert!(db.sessions().get("null-expiry").unwrap().is_none());
    assert!(komari_db::models::Session::default().is_expired());
}

#[test]
fn legacy_configs_are_converted_to_key_value() {
    let dir = TempDir::new("legacy-configs");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `allow_cors` boolean, \
                 `record_preserve_time` integer, \
                 `traffic_limit_percentage` float\
             );\
             INSERT INTO configs (sitename, allow_cors, record_preserve_time, traffic_limit_percentage) \
             VALUES ('Old Site', 1, 123, 80.5);",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let config = db.config();
    assert_eq!(
        config.get_as::<String>("sitename").unwrap().as_deref(),
        Some("Old Site")
    );
    assert_eq!(config.get_as::<bool>("allow_cors").unwrap(), Some(true));
    assert_eq!(
        config.get_as::<i64>("record_preserve_time").unwrap(),
        Some(123)
    );
    assert_eq!(
        config.get_as::<f64>("traffic_limit_percentage").unwrap(),
        Some(80.5)
    );

    // The key/value table is fully usable (ON CONFLICT on the primary key).
    config.set("theme", "dark").unwrap();
    assert_eq!(
        config.get_as::<String>("theme").unwrap().as_deref(),
        Some("dark")
    );

    // The original single row is preserved in the backup table.
    assert!(db.table_exists("configs_legacy_backup").unwrap());
    let backup_rows: i64 = db
        .with_connection(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM configs_legacy_backup", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(backup_rows, 1);
}

#[test]
fn migration_backfills_identifiers_with_random_values() {
    let dir = TempDir::new("random-backfill");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    // Columns missing entirely (or present but empty) in old schemas.
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE clients (`uuid` varchar(36) PRIMARY KEY, `name` varchar(100));\
             INSERT INTO clients VALUES ('c1', 'one');\
             INSERT INTO clients VALUES ('c2', 'two');\
             CREATE TABLE users (`uuid` varchar(36) PRIMARY KEY, `passwd` varchar(255));\
             INSERT INTO users VALUES ('u1', 'hash');\
             INSERT INTO users VALUES ('u2', 'hash');\
             CREATE TABLE sessions (`uuid` varchar(36), `user_agent` text);\
             INSERT INTO sessions VALUES ('u1', 'agent');\
             INSERT INTO sessions VALUES ('u1', 'agent');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let clients = db.clients().list().unwrap();
    assert_eq!(clients.len(), 2);
    let tokens: std::collections::HashSet<&str> =
        clients.iter().map(|client| client.token.as_str()).collect();
    assert_eq!(tokens.len(), 2);
    for client in &clients {
        assert_eq!(client.token.len(), 22);
        assert!(!client.token.starts_with("legacy-"));
    }

    let users = db.accounts().list().unwrap();
    assert_eq!(users.len(), 2);
    let usernames: std::collections::HashSet<&str> =
        users.iter().map(|user| user.username.as_str()).collect();
    assert_eq!(usernames.len(), 2);
    for user in &users {
        assert!(user.username.starts_with("legacy-"));
    }

    let sessions = db.sessions().list().unwrap();
    assert_eq!(sessions.len(), 2);
    let session_ids: std::collections::HashSet<&str> = sessions
        .iter()
        .map(|session| session.session.as_str())
        .collect();
    assert_eq!(session_ids.len(), 2);
    for session in &sessions {
        assert_eq!(session.session.len(), 32);
    }
}

#[test]
fn hybrid_configs_from_a_previous_migration_are_converted() {
    let dir = TempDir::new("hybrid-configs");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    // Shape produced by the previous version: legacy wide columns plus empty
    // key/value columns added by ALTER TABLE.
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `theme` varchar(100), \
                 `allow_cors` boolean, \
                 `key` text, \
                 `value` text\
             );\
             INSERT INTO configs (id, sitename, theme, allow_cors, key, value) \
             VALUES (1, 'Hybrid Site', 'legacy-theme', 1, '', '');\
             INSERT INTO configs (id, sitename, key, value) \
             VALUES (2, NULL, 'theme', '\"dark\"');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let config = db.config();
    assert_eq!(
        config.get_as::<String>("sitename").unwrap().as_deref(),
        Some("Hybrid Site")
    );
    assert_eq!(config.get_as::<bool>("allow_cors").unwrap(), Some(true));
    // Valid key/value rows that predated the conversion are preserved.
    assert_eq!(
        config.get_as::<String>("theme").unwrap().as_deref(),
        Some("dark")
    );
    assert!(db.table_exists("configs_legacy_backup").unwrap());

    // A second migration must not create another backup or change values.
    db.migrate().unwrap();
    assert!(!db.table_exists("configs_legacy_backup_2").unwrap());
    assert_eq!(
        config.get_as::<String>("sitename").unwrap().as_deref(),
        Some("Hybrid Site")
    );
}

#[test]
fn legacy_row_with_null_sitename_is_converted() {
    let dir = TempDir::new("null-sitename");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `description` text, \
                 `key` text, \
                 `value` text\
             );\
             INSERT INTO configs (id, sitename, description, key, value) \
             VALUES (1, NULL, 'desc-from-legacy', '', '');\
             INSERT INTO configs (id, sitename, key, value) \
             VALUES (2, NULL, 'theme', '\"dark\"');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let config = db.config();
    assert_eq!(
        config.get_as::<String>("description").unwrap().as_deref(),
        Some("desc-from-legacy")
    );
    assert_eq!(
        config.get_as::<String>("theme").unwrap().as_deref(),
        Some("dark")
    );
    assert!(
        config
            .get_as::<String>("sitename")
            .unwrap()
            .unwrap_or_default()
            .is_empty()
    );
}

#[test]
fn unsupported_configs_schema_returns_a_clear_error() {
    let dir = TempDir::new("bad-configs");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (`foo` text); INSERT INTO configs VALUES ('bar');",
        )?;
        Ok(())
    })
    .unwrap();

    let error = db.migrate().unwrap_err();
    assert!(
        matches!(error, komari_db::DbError::InvalidData(ref message) if message.contains("configs")),
        "unexpected error: {error}"
    );
}

#[test]
fn migration_restores_missing_unique_constraints() {
    let dir = TempDir::new("old-unique");
    let path = dir.join("komari.db");
    let db = Db::open(&path).unwrap();

    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients VALUES ('c1', 'token-1', 'one');\
             INSERT INTO clients VALUES ('c2', 'token-2', 'two');\
             CREATE TABLE users (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `username` varchar(50) NOT NULL, \
                 `passwd` varchar(255) NOT NULL\
             );\
             INSERT INTO users VALUES ('u1', 'admin', 'hash');\
             INSERT INTO users VALUES ('u2', 'other', 'hash');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let existing = db.clients().require("c1").unwrap();
    let duplicate_client = komari_db::models::Client {
        uuid: "c3".to_owned(),
        token: existing.token.clone(),
        name: "duplicate".to_owned(),
        ..komari_db::models::Client::default()
    };
    assert!(db.clients().create(&duplicate_client).is_err());

    let duplicate_user = komari_db::models::User {
        uuid: "u3".to_owned(),
        username: "admin".to_owned(),
        passwd: "hash".to_owned(),
        ..komari_db::models::User::default()
    };
    assert!(db.accounts().insert(&duplicate_user).is_err());
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
