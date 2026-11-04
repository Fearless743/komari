//! Historical migrations ported from Go `database/dbcore.MergeDatabase`.
//!
//! These tests build pre-1.0.2 databases by hand and verify that
//! `Db::migrate()` merges `client_infos`, moves the old OAuth/notification
//! columns into the provider tables, and leaves already-current databases
//! untouched.

mod support;

use komari_db::{Db, LocalTime};
use support::TempDir;

fn open_legacy(tag: &str) -> (TempDir, Db) {
    let dir = TempDir::new(tag);
    let db = Db::open(dir.join("komari.db")).expect("open database");
    (dir, db)
}

fn scalar<T: rusqlite::types::FromSql>(db: &Db, sql: &str) -> T {
    db.with_connection(|conn| Ok(conn.query_row(sql, [], |row| row.get(0))?))
        .expect("query scalar")
}

fn table_columns(db: &Db, table: &str) -> Vec<String> {
    db.with_connection(|conn| {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(`{table}`)"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .expect("table columns")
}

const CLIENT_INFO_COLUMNS: &str = "\
    `uuid` varchar(36) PRIMARY KEY, \
    `name` varchar(100) NOT NULL, \
    `cpu_name` varchar(100), \
    `virtualization` varchar(50), \
    `arch` varchar(50), \
    `cpu_cores` integer, \
    `os` varchar(100), \
    `gpu_name` varchar(100), \
    `ipv4` varchar(100), \
    `ipv6` varchar(100), \
    `region` varchar(100), \
    `remark` longtext, \
    `public_remark` longtext, \
    `mem_total` bigint, \
    `swap_total` bigint, \
    `disk_total` bigint, \
    `version` varchar(100), \
    `weight` integer, \
    `price` real, \
    `billing_cycle` integer, \
    `expired_at` timestamp, \
    `created_at` datetime, \
    `updated_at` datetime";

#[test]
fn merges_legacy_client_infos_and_renames_backup() {
    let (_dir, db) = open_legacy("client-infos-merge");
    db.with_connection(|conn| {
        conn.execute_batch(&format!(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100), \
                 `kernel_version` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name, kernel_version) \
             VALUES ('c1', 'token-1', 'old-name', '6.1.0');\
             CREATE TABLE client_infos ({CLIENT_INFO_COLUMNS});\
             INSERT INTO client_infos (\
                 uuid, name, cpu_name, virtualization, arch, cpu_cores, os, \
                 gpu_name, ipv4, ipv6, region, remark, public_remark, \
                 mem_total, swap_total, disk_total, version, weight, price, \
                 billing_cycle, expired_at\
             ) VALUES (\
                 'c1', 'new-name', 'AMD EPYC', 'kvm', 'x86_64', 8, 'Debian', \
                 'RTX 4090', '1.2.3.4', '::1', 'US', 'secret', 'public', \
                 1024, 512, 2048, '1.0.0', 5, 12.5, 30, \
                 '2027-01-02 03:04:05'\
             );"
        ))?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let client = db.clients().require("c1").unwrap();
    assert_eq!(client.name, "new-name");
    assert_eq!(client.cpu_name, "AMD EPYC");
    assert_eq!(client.virtualization, "kvm");
    assert_eq!(client.arch, "x86_64");
    assert_eq!(client.cpu_cores, 8);
    assert_eq!(client.os, "Debian");
    assert_eq!(client.gpu_name, "RTX 4090");
    assert_eq!(client.ipv4, "1.2.3.4");
    assert_eq!(client.ipv6, "::1");
    assert_eq!(client.region, "US");
    assert_eq!(client.remark, "secret");
    assert_eq!(client.public_remark, "public");
    assert_eq!(client.mem_total, 1024);
    assert_eq!(client.swap_total, 512);
    assert_eq!(client.disk_total, 2048);
    assert_eq!(client.version, "1.0.0");
    assert_eq!(client.weight, 5);
    assert_eq!(client.price, 12.5);
    assert_eq!(client.billing_cycle, 30);
    assert_eq!(
        client.expired_at,
        LocalTime::parse("2027-01-02 03:04:05").unwrap()
    );
    // Fields absent from ClientInfo must not be overwritten.
    assert_eq!(client.kernel_version, "6.1.0");
    assert_eq!(client.token, "token-1");
    assert!(!client.updated_at.is_zero());

    assert!(!db.table_exists("client_infos").unwrap());
    assert!(db.table_exists("client_infos_backup").unwrap());
    assert_eq!(
        scalar::<i64>(&db, "SELECT COUNT(*) FROM client_infos_backup"),
        1
    );
}

#[test]
fn client_infos_rows_without_a_client_are_skipped() {
    let (_dir, db) = open_legacy("client-infos-ghost");
    db.with_connection(|conn| {
        conn.execute_batch(&format!(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name) VALUES ('c1', 'token-1', 'kept');\
             CREATE TABLE client_infos ({CLIENT_INFO_COLUMNS});\
             INSERT INTO client_infos (uuid, name) VALUES ('c1', 'merged');\
             INSERT INTO client_infos (uuid, name) VALUES ('ghost', 'ignored');"
        ))?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    assert_eq!(db.clients().count().unwrap(), 1);
    assert_eq!(db.clients().require("c1").unwrap().name, "merged");
    assert!(db.clients().get("ghost").unwrap().is_none());
    // Go still backs up the table even when no client matched.
    assert_eq!(
        scalar::<i64>(&db, "SELECT COUNT(*) FROM client_infos_backup"),
        2
    );
}

#[test]
fn client_infos_with_missing_columns_still_merges() {
    let (_dir, db) = open_legacy("client-infos-missing-columns");
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name) VALUES ('c1', 'token-1', 'old');\
             CREATE TABLE client_infos (`uuid` varchar(36) PRIMARY KEY, `name` varchar(100));\
             INSERT INTO client_infos (uuid, name) VALUES ('c1', 'new');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate()
        .expect("missing columns must not abort the merge");

    let client = db.clients().require("c1").unwrap();
    assert_eq!(client.name, "new");
    assert_eq!(client.cpu_name, "");
    assert_eq!(client.mem_total, 0);
    assert!(db.table_exists("client_infos_backup").unwrap());
}

#[test]
fn migrates_legacy_oidc_configuration() {
    let (_dir, db) = open_legacy("legacy-oidc");
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `o_auth_client_id` varchar(255), \
                 `o_auth_client_secret` varchar(255), \
                 `o_auth_provider` varchar(50)\
             );\
             INSERT INTO configs (id, sitename, o_auth_client_id, o_auth_client_secret, o_auth_provider) \
             VALUES (1, 'Legacy Site', 'client-id', 'client-secret', 'google');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let provider = db.oauth().get("github").unwrap().unwrap();
    let addition: serde_json::Value = serde_json::from_str(&provider.addition).unwrap();
    assert_eq!(addition["client_id"], "client-id");
    assert_eq!(addition["client_secret"], "client-secret");
    assert_eq!(
        provider.addition,
        r#"{"client_id":"client-id","client_secret":"client-secret"}"#
    );

    // Go forces the active provider to github for legacy single-row configs.
    assert_eq!(
        db.config()
            .get_as::<String>("o_auth_provider")
            .unwrap()
            .as_deref(),
        Some("github")
    );
    assert_eq!(
        db.config().get_as::<String>("sitename").unwrap().as_deref(),
        Some("Legacy Site")
    );
    // Credentials moved out of the active configuration.
    assert!(db.config().get("o_auth_client_id").unwrap().is_none());
    assert!(db.config().get("o_auth_client_secret").unwrap().is_none());
    assert!(db.table_exists("configs_legacy_backup").unwrap());
}

#[test]
fn migrates_legacy_telegram_configuration() {
    let (_dir, db) = open_legacy("legacy-telegram");
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `notification_method` varchar(50), \
                 `telegram_bot_token` varchar(255), \
                 `telegram_chat_id` varchar(255), \
                 `telegram_endpoint` varchar(255)\
             );\
             INSERT INTO configs (id, sitename, notification_method, telegram_bot_token, telegram_chat_id, telegram_endpoint) \
             VALUES (1, 'Site', 'telegram', 'bot-token', '-100123', '');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let provider = db.message_senders().get("telegram").unwrap().unwrap();
    assert_eq!(
        provider.addition,
        r#"{"bot_token":"bot-token","chat_id":"-100123","endpoint":"https://api.telegram.org/bot"}"#
    );
    assert!(db.config().get("telegram_bot_token").unwrap().is_none());
    assert!(db.config().get("telegram_chat_id").unwrap().is_none());
    assert!(db.config().get("telegram_endpoint").unwrap().is_none());
    assert_eq!(db.message_senders().count().unwrap(), 1);
}

#[test]
fn migrates_legacy_email_configuration() {
    let (_dir, db) = open_legacy("legacy-email");
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `notification_method` varchar(50), \
                 `email_host` varchar(255), \
                 `email_port` integer, \
                 `email_username` varchar(255), \
                 `email_password` varchar(255), \
                 `email_sender` varchar(255), \
                 `email_receiver` varchar(255), \
                 `email_use_ssl` boolean\
             );\
             INSERT INTO configs (id, sitename, notification_method, email_host, email_port, email_username, email_password, email_sender, email_receiver, email_use_ssl) \
             VALUES (1, 'Site', 'email', 'smtp.example.com', 587, 'user', 'pass', 'from@example.com', 'to@example.com', 1);",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let provider = db.message_senders().get("email").unwrap().unwrap();
    let addition: serde_json::Value = serde_json::from_str(&provider.addition).unwrap();
    assert_eq!(
        addition,
        serde_json::json!({
            "host": "smtp.example.com",
            "port": 587,
            "username": "user",
            "password": "pass",
            "sender": "from@example.com",
            "receiver": "to@example.com",
            "use_ssl": true,
        })
    );
    assert_eq!(db.message_senders().count().unwrap(), 1);
}

#[test]
fn legacy_migrations_are_idempotent() {
    let (_dir, db) = open_legacy("legacy-idempotent");
    db.with_connection(|conn| {
        conn.execute_batch(&format!(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name) VALUES ('c1', 'token-1', 'old');\
             CREATE TABLE client_infos ({CLIENT_INFO_COLUMNS});\
             INSERT INTO client_infos (uuid, name, cpu_name) VALUES ('c1', 'new', 'CPU');\
             CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `o_auth_client_id` varchar(255), \
                 `o_auth_client_secret` varchar(255), \
                 `o_auth_provider` varchar(50), \
                 `notification_method` varchar(50), \
                 `telegram_bot_token` varchar(255), \
                 `telegram_chat_id` varchar(255)\
             );\
             INSERT INTO configs (id, sitename, o_auth_client_id, o_auth_client_secret, o_auth_provider, notification_method, telegram_bot_token, telegram_chat_id) \
             VALUES (1, 'Site', 'cid', 'secret', 'google', 'telegram', 'token', 'chat');"
        ))?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();
    let first_client = db.clients().require("c1").unwrap();
    let first_oidc = db.oauth().get("github").unwrap().unwrap();
    let first_telegram = db.message_senders().get("telegram").unwrap().unwrap();

    db.migrate().unwrap();

    assert_eq!(db.clients().require("c1").unwrap().name, "new");
    assert_eq!(db.clients().require("c1").unwrap().cpu_name, "CPU");
    assert_eq!(db.oauth().count().unwrap(), 1);
    assert_eq!(db.message_senders().count().unwrap(), 1);
    assert_eq!(db.oauth().get("github").unwrap().unwrap(), first_oidc);
    assert_eq!(
        db.message_senders().get("telegram").unwrap().unwrap(),
        first_telegram
    );
    assert_eq!(
        db.clients().require("c1").unwrap().token,
        first_client.token
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT COUNT(*) FROM client_infos_backup"),
        1
    );
    assert!(!db.table_exists("client_infos_backup_2").unwrap());
    assert!(!db.table_exists("client_infos").unwrap());
}

#[test]
fn empty_legacy_tables_are_handled() {
    let (_dir, db) = open_legacy("legacy-empty");
    db.with_connection(|conn| {
        conn.execute_batch(&format!(
            "CREATE TABLE client_infos ({CLIENT_INFO_COLUMNS});\
             CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `o_auth_client_id` varchar(255), \
                 `o_auth_client_secret` varchar(255)\
             );"
        ))?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    assert!(db.table_exists("client_infos_backup").unwrap());
    assert_eq!(
        scalar::<i64>(&db, "SELECT COUNT(*) FROM client_infos_backup"),
        0
    );
    // Go still creates the default github provider from an empty legacy row.
    let provider = db.oauth().get("github").unwrap().unwrap();
    assert_eq!(provider.addition, r#"{"client_id":"","client_secret":""}"#);
    assert_eq!(db.message_senders().count().unwrap(), 0);
    assert_eq!(db.config().count().unwrap(), 0);
}

#[test]
fn legacy_oidc_values_are_json_escaped() {
    let (_dir, db) = open_legacy("legacy-oidc-json");
    db.with_connection(|conn| {
        conn.execute_batch(
            "CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `o_auth_client_id` varchar(255), \
                 `o_auth_client_secret` varchar(255)\
             );\
             INSERT INTO configs (id, sitename, o_auth_client_id, o_auth_client_secret) \
             VALUES (1, 'Site', 'id\"with\"quotes', 'line1\nline2\\end');",
        )?;
        Ok(())
    })
    .unwrap();

    db.migrate().unwrap();

    let provider = db.oauth().get("github").unwrap().unwrap();
    let addition: serde_json::Value =
        serde_json::from_str(&provider.addition).expect("addition must stay valid JSON");
    assert_eq!(addition["client_id"], "id\"with\"quotes");
    assert_eq!(addition["client_secret"], "line1\nline2\\end");
}

#[test]
fn existing_provider_rows_with_unusual_json_are_not_rewritten() {
    let (_dir, db) = open_legacy("existing-providers");
    db.migrate().unwrap();

    db.oauth()
        .save(&komari_db::models::Provider::new("github", "not-json{"))
        .unwrap();
    db.message_senders()
        .save(&komari_db::models::Provider::new(
            "telegram",
            r#"{"bot_token":"existing"}"#,
        ))
        .unwrap();

    db.migrate().unwrap();

    assert_eq!(
        db.oauth().get("github").unwrap().unwrap().addition,
        "not-json{"
    );
    assert_eq!(
        db.message_senders()
            .get("telegram")
            .unwrap()
            .unwrap()
            .addition,
        r#"{"bot_token":"existing"}"#
    );
}

#[test]
fn failed_legacy_migration_rolls_back_every_change() {
    let (_dir, db) = open_legacy("legacy-rollback");
    db.with_connection(|conn| {
        conn.execute_batch(&format!(
            "CREATE TABLE clients (\
                 `uuid` varchar(36) PRIMARY KEY, \
                 `token` varchar(255) NOT NULL, \
                 `name` varchar(100)\
             );\
             INSERT INTO clients (uuid, token, name) VALUES ('c1', 'token-1', 'old');\
             CREATE TABLE client_infos ({CLIENT_INFO_COLUMNS});\
             INSERT INTO client_infos (uuid, name) VALUES ('c1', 'new');\
             CREATE TABLE configs (\
                 `id` integer PRIMARY KEY AUTOINCREMENT, \
                 `sitename` varchar(100), \
                 `o_auth_client_id` varchar(255), \
                 `o_auth_client_secret` varchar(255)\
             );\
             INSERT INTO configs (id, sitename, o_auth_client_id, o_auth_client_secret) \
             VALUES (1, 'Site', 'cid', 'secret');\
             CREATE TABLE sessions (`uuid` varchar(36), `session` varchar(255));\
             INSERT INTO sessions (uuid, session) VALUES ('u1', 'duplicate');\
             INSERT INTO sessions (uuid, session) VALUES ('u2', 'duplicate');"
        ))?;
        Ok(())
    })
    .unwrap();

    let error = db.migrate().unwrap_err();
    assert!(
        matches!(error, komari_db::DbError::InvalidData(ref message)
            if message.contains("duplicate")),
        "unexpected error: {error}"
    );

    // The transaction rolled back: the legacy tables are exactly as they were.
    assert!(db.table_exists("client_infos").unwrap());
    assert!(!db.table_exists("client_infos_backup").unwrap());
    assert!(!db.table_exists("oidc_providers").unwrap());
    assert!(!db.table_exists("message_sender_providers").unwrap());
    assert_eq!(
        scalar::<String>(&db, "SELECT name FROM clients WHERE uuid = 'c1'"),
        "old"
    );
    assert_eq!(
        scalar::<String>(&db, "SELECT o_auth_client_id FROM configs WHERE id = 1"),
        "cid"
    );
    assert_eq!(scalar::<i64>(&db, "SELECT COUNT(*) FROM sessions"), 2);
    assert!(table_columns(&db, "configs").contains(&"sitename".to_owned()));
}

#[test]
fn current_database_is_unchanged_by_migrate() {
    let (_dir, db) = open_legacy("current-unchanged");
    db.migrate().unwrap();

    let client = db.clients().create_named("node-1").unwrap();
    db.config().set("sitename", "Current").unwrap();
    db.oauth()
        .save(&komari_db::models::Provider::new(
            "github",
            r#"{"client_id":"keep"}"#,
        ))
        .unwrap();
    db.message_senders()
        .save(&komari_db::models::Provider::new(
            "telegram",
            r#"{"bot_token":"keep"}"#,
        ))
        .unwrap();

    let tables_before = db.table_names().unwrap();
    let counts_before: Vec<i64> = komari_db::migrate::TABLES
        .iter()
        .map(|table| scalar::<i64>(&db, &format!("SELECT COUNT(*) FROM `{table}`")))
        .collect();

    db.migrate().unwrap();

    assert_eq!(db.table_names().unwrap(), tables_before);
    let counts_after: Vec<i64> = komari_db::migrate::TABLES
        .iter()
        .map(|table| scalar::<i64>(&db, &format!("SELECT COUNT(*) FROM `{table}`")))
        .collect();
    assert_eq!(counts_after, counts_before);

    assert_eq!(db.clients().require(&client.uuid).unwrap().name, "node-1");
    assert_eq!(
        db.config().get_as::<String>("sitename").unwrap().as_deref(),
        Some("Current")
    );
    assert_eq!(
        db.oauth().get("github").unwrap().unwrap().addition,
        r#"{"client_id":"keep"}"#
    );
    assert_eq!(
        db.message_senders()
            .get("telegram")
            .unwrap()
            .unwrap()
            .addition,
        r#"{"bot_token":"keep"}"#
    );
    assert!(!db.table_exists("client_infos").unwrap());
    assert!(!db.table_exists("client_infos_backup").unwrap());
    assert!(!db.table_exists("configs_legacy_backup").unwrap());
}
