//! Idempotent schema migration.
//!
//! The statements below are the exact schema GORM generates for the Go models
//! (verified against `komari/data/komari.db`). Every object uses
//! `IF NOT EXISTS`, so running the migration again never alters, drops or
//! rewrites an existing table — an old Komari database keeps all of its data.

use crate::db::Db;
use crate::error::Result;

/// Tables created by the Go backend's `AutoMigrate` call.
pub const TABLES: &[&str] = &[
    "users",
    "clients",
    "sessions",
    "records",
    "gpu_records",
    "logs",
    "clipboards",
    "offline_notifications",
    "load_notifications",
    "oidc_providers",
    "message_sender_providers",
    "ddns_providers",
    "ddns_sync_history",
    "theme_configurations",
    "configs",
    "records_long_term",
    "gpu_records_long_term",
];

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS `users` (
    `uuid` varchar(36),
    `username` varchar(50) NOT NULL,
    `passwd` varchar(255) NOT NULL,
    `sso_type` varchar(20),
    `sso_id` varchar(100),
    `two_factor` varchar(255),
    `created_at` datetime,
    `updated_at` datetime,
    PRIMARY KEY (`uuid`),
    CONSTRAINT `uni_users_username` UNIQUE (`username`)
);

CREATE TABLE IF NOT EXISTS `clients` (
    `uuid` varchar(36),
    `token` varchar(255) NOT NULL,
    `name` varchar(100),
    `cpu_name` varchar(100),
    `virtualization` varchar(50),
    `arch` varchar(50),
    `cpu_cores` integer,
    `os` varchar(100),
    `kernel_version` varchar(100),
    `gpu_name` varchar(100),
    `ipv4` varchar(100),
    `ipv6` varchar(100),
    `region` varchar(100),
    `remark` longtext,
    `public_remark` longtext,
    `mem_total` bigint,
    `swap_total` bigint,
    `disk_total` bigint,
    `version` varchar(100),
    `weight` integer,
    `price` real,
    `billing_cycle` integer,
    `auto_renewal` numeric DEFAULT false,
    `currency` varchar(20) DEFAULT "$",
    `expired_at` timestamp,
    `group` varchar(100),
    `tags` text,
    `ddns_enabled` numeric DEFAULT false,
    `ddns_hostname` varchar(255),
    `ddns_record_id` varchar(255),
    `ddns_record_type` varchar(10) DEFAULT "inherit",
    `hidden` numeric DEFAULT false,
    `traffic_limit` bigint,
    `traffic_limit_type` varchar(10) DEFAULT "max",
    `created_at` datetime,
    `updated_at` datetime,
    PRIMARY KEY (`uuid`),
    CONSTRAINT `uni_clients_token` UNIQUE (`token`)
);

CREATE TABLE IF NOT EXISTS `sessions` (
    `uuid` varchar(36),
    `session` varchar(255) NOT NULL,
    `user_agent` text,
    `ip` varchar(100),
    `login_method` varchar(50),
    `latest_online` timestamp,
    `latest_user_agent` text,
    `latest_ip` varchar(100),
    `expires` datetime NOT NULL,
    `created_at` datetime,
    PRIMARY KEY (`session`),
    CONSTRAINT `fk_users_sessions` FOREIGN KEY (`uuid`) REFERENCES `users`(`uuid`) ON DELETE CASCADE ON UPDATE CASCADE
);

CREATE TABLE IF NOT EXISTS `records` (
    `client` varchar(36),
    `time` datetime,
    `cpu` decimal(5,2),
    `gpu` decimal(5,2),
    `ram` bigint,
    `ram_total` bigint,
    `swap` bigint,
    `swap_total` bigint,
    `load` decimal(5,2),
    `temp` decimal(5,2),
    `disk` bigint,
    `disk_total` bigint,
    `net_in` bigint,
    `net_out` bigint,
    `net_total_up` bigint,
    `net_total_down` bigint,
    `process` integer,
    `connections` integer,
    `connections_udp` integer
);

CREATE TABLE IF NOT EXISTS `gpu_records` (
    `client` varchar(36),
    `time` datetime,
    `device_index` integer,
    `device_name` varchar(100),
    `mem_total` bigint,
    `mem_used` bigint,
    `utilization` decimal(5,2),
    `temperature` integer
);

CREATE TABLE IF NOT EXISTS `logs` (
    `id` integer PRIMARY KEY AUTOINCREMENT,
    `ip` varchar(45),
    `uuid` varchar(36),
    `message` text NOT NULL,
    `msg_type` varchar(20) NOT NULL,
    `time` datetime NOT NULL
);

CREATE TABLE IF NOT EXISTS `clipboards` (
    `id` integer PRIMARY KEY AUTOINCREMENT,
    `text` longtext,
    `name` varchar(255),
    `weight` integer,
    `remark` text,
    `created_at` datetime,
    `updated_at` datetime,
    CONSTRAINT `uni_clipboards_id` UNIQUE (`id`)
);

CREATE TABLE IF NOT EXISTS `offline_notifications` (
    `client` varchar(36) NOT NULL,
    `enable` boolean DEFAULT false,
    `grace_period` integer NOT NULL DEFAULT 180,
    `last_notified` datetime,
    CONSTRAINT `fk_offline_notifications_client_info` FOREIGN KEY (`client`) REFERENCES `clients`(`uuid`),
    CONSTRAINT `uni_offline_notifications_client` UNIQUE (`client`)
);

CREATE TABLE IF NOT EXISTS `load_notifications` (
    `id` integer PRIMARY KEY AUTOINCREMENT,
    `name` varchar(255),
    `clients` longtext,
    `metric` varchar(50) NOT NULL DEFAULT "cpu",
    `threshold` decimal(5,2) NOT NULL DEFAULT 80,
    `ratio` decimal(5,2) NOT NULL DEFAULT 0.8,
    `interval` integer NOT NULL DEFAULT 15,
    `last_notified` datetime
);

CREATE TABLE IF NOT EXISTS `oidc_providers` (
    `name` text NOT NULL,
    `addition` longtext,
    PRIMARY KEY (`name`),
    CONSTRAINT `uni_oidc_providers_name` UNIQUE (`name`)
);

CREATE TABLE IF NOT EXISTS `message_sender_providers` (
    `name` text NOT NULL,
    `addition` longtext,
    PRIMARY KEY (`name`),
    CONSTRAINT `uni_message_sender_providers_name` UNIQUE (`name`)
);

CREATE TABLE IF NOT EXISTS `ddns_providers` (
    `name` text NOT NULL,
    `addition` longtext,
    PRIMARY KEY (`name`),
    CONSTRAINT `uni_ddns_providers_name` UNIQUE (`name`)
);

CREATE TABLE IF NOT EXISTS `ddns_sync_history` (
    `id` integer PRIMARY KEY AUTOINCREMENT,
    `client_uuid` varchar(36),
    `client_name` varchar(100),
    `hostname` varchar(255),
    `record_type` varchar(10),
    `ip_v4` varchar(100),
    `ip_v6` varchar(100),
    `record_id` varchar(255),
    `status` varchar(20),
    `error` text,
    `triggered_by` varchar(50),
    `synced_at` datetime
);

CREATE TABLE IF NOT EXISTS `theme_configurations` (
    `short` text NOT NULL,
    `data` longtext,
    PRIMARY KEY (`short`),
    CONSTRAINT `uni_theme_configurations_short` UNIQUE (`short`)
);

CREATE TABLE IF NOT EXISTS `configs` (
    `key` text,
    `value` text,
    PRIMARY KEY (`key`)
);

CREATE TABLE IF NOT EXISTS `records_long_term` (
    `client` varchar(36),
    `time` datetime,
    `cpu` decimal(5,2),
    `gpu` decimal(5,2),
    `ram` bigint,
    `ram_total` bigint,
    `swap` bigint,
    `swap_total` bigint,
    `load` decimal(5,2),
    `temp` decimal(5,2),
    `disk` bigint,
    `disk_total` bigint,
    `net_in` bigint,
    `net_out` bigint,
    `net_total_up` bigint,
    `net_total_down` bigint,
    `process` integer,
    `connections` integer,
    `connections_udp` integer
);

CREATE TABLE IF NOT EXISTS `gpu_records_long_term` (
    `client` varchar(36),
    `time` datetime,
    `device_index` integer,
    `device_name` varchar(100),
    `mem_total` bigint,
    `mem_used` bigint,
    `utilization` decimal(5,2),
    `temperature` integer
);

"#;

const INDEX_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS `idx_offline_notifications_client` ON `offline_notifications`(`client`);
CREATE INDEX IF NOT EXISTS `idx_ddns_sync_history_client_uuid` ON `ddns_sync_history`(`client_uuid`);
CREATE INDEX IF NOT EXISTS `idx_records_client` ON `records`(`client`);
CREATE INDEX IF NOT EXISTS `idx_records_time` ON `records`(`time`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_client` ON `gpu_records`(`client`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_time` ON `gpu_records`(`time`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_device_index` ON `gpu_records`(`device_index`);
CREATE INDEX IF NOT EXISTS `idx_records_long_term_client` ON `records_long_term`(`client`);
CREATE INDEX IF NOT EXISTS `idx_records_long_term_time` ON `records_long_term`(`time`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_long_term_client` ON `gpu_records_long_term`(`client`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_long_term_time` ON `gpu_records_long_term`(`time`);
CREATE INDEX IF NOT EXISTS `idx_gpu_records_long_term_device_index` ON `gpu_records_long_term`(`device_index`);
"#;

/// Extract the column definitions for `table` from the canonical DDL above.
fn expected_columns(table: &str) -> Vec<(String, String)> {
    let marker = format!("CREATE TABLE IF NOT EXISTS `{table}` (");
    let Some(start) = SCHEMA_SQL.find(&marker) else {
        return Vec::new();
    };
    let body = &SCHEMA_SQL[start + marker.len()..];
    let end = body.find("\n);").unwrap_or(body.len());

    body[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',').trim();
            if line.is_empty() {
                return None;
            }
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("PRIMARY KEY")
                || upper.starts_with("CONSTRAINT")
                || upper.starts_with("UNIQUE")
                || upper.starts_with("FOREIGN KEY")
            {
                return None;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let name = parts.next()?.trim_matches('`').trim_matches('"').to_owned();
            let definition = parts.next()?.trim().to_owned();
            Some((name, definition))
        })
        .collect()
}

/// Columns added to old tables must be nullable or have a default, otherwise
/// SQLite rejects the ALTER on a non-empty table. Keep declared defaults, and
/// synthesize one for scalar types so existing rows are readable by `FromRow`.
fn alter_definition(definition: &str) -> String {
    let mut definition = definition
        .replace("PRIMARY KEY", "")
        .replace("AUTOINCREMENT", "")
        .replace("NOT NULL", "")
        .replace("UNIQUE", "")
        .trim()
        .to_owned();
    while definition.contains("  ") {
        definition = definition.replace("  ", " ");
    }

    if definition.to_ascii_uppercase().contains("DEFAULT") {
        return definition;
    }

    let lower = definition.to_ascii_lowercase();
    let numeric = lower.starts_with("integer")
        || lower.starts_with("bigint")
        || lower.starts_with("int")
        || lower.starts_with("decimal")
        || lower.starts_with("numeric")
        || lower.starts_with("real")
        || lower.starts_with("double")
        || lower.starts_with("float")
        || lower.starts_with("boolean");
    let text = lower.starts_with("varchar")
        || lower.starts_with("text")
        || lower.starts_with("longtext")
        || lower.starts_with("char")
        || lower.starts_with("blob");

    if numeric {
        definition.push_str(" DEFAULT 0");
    } else if text {
        definition.push_str(" DEFAULT ''");
    }
    definition
}

fn actual_columns(tx: &rusqlite::Transaction<'_>, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info(`{table}`)"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

fn add_missing_columns(tx: &rusqlite::Transaction<'_>) -> crate::error::Result<()> {
    for table in TABLES {
        let actual = actual_columns(tx, table)?;
        for (name, definition) in expected_columns(table) {
            if actual
                .iter()
                .any(|column| column.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            let definition = alter_definition(&definition);
            tx.execute(
                &format!("ALTER TABLE `{table}` ADD COLUMN `{name}` {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn table_exists_tx(tx: &rusqlite::Transaction<'_>, table: &str) -> rusqlite::Result<bool> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn index_columns(
    tx: &rusqlite::Transaction<'_>,
    index: &str,
) -> rusqlite::Result<Vec<Option<String>>> {
    let mut stmt = tx.prepare(&format!("PRAGMA index_info(`{index}`)"))?;
    let rows = stmt.query_map([], |row| row.get::<_, Option<String>>(2))?;
    rows.collect()
}

fn has_unique_column(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut stmt = tx.prepare(&format!("PRAGMA index_list(`{table}`)"))?;
    let indexes = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    for (name, unique) in indexes {
        if unique == 0 {
            continue;
        }
        let columns = index_columns(tx, &name)?;
        if columns.len() == 1
            && columns[0]
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(column))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_no_duplicates(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
) -> crate::error::Result<()> {
    let duplicates: i64 = tx.query_row(
        &format!(
            "SELECT COUNT(*) FROM (\
                 SELECT `{column}` FROM `{table}` \
                 WHERE `{column}` IS NOT NULL \
                 GROUP BY `{column}` HAVING COUNT(*) > 1\
             )"
        ),
        [],
        |row| row.get(0),
    )?;
    if duplicates > 0 {
        return Err(crate::error::DbError::InvalidData(format!(
            "cannot restore unique constraint on {table}.{column}: {duplicates} duplicate value(s)"
        )));
    }
    Ok(())
}

/// Old tables often predate unique constraints. Restore them with a unique
/// index (functionally identical to a `UNIQUE` constraint) when the data has
/// no duplicates. Empty identifiers get a synthetic rowid-based value first so
/// additive migrations remain usable.
fn ensure_unique_columns(tx: &rusqlite::Transaction<'_>) -> crate::error::Result<()> {
    // Backfill identifiers that were added as empty columns by the migration.
    for (table, column) in [
        ("clients", "token"),
        ("users", "username"),
        ("sessions", "session"),
    ] {
        if table_exists_tx(tx, table)? {
            tx.execute(
                &format!(
                    "UPDATE `{table}` SET `{column}` = 'legacy-' || rowid \
                     WHERE `{column}` IS NULL OR `{column}` = ''"
                ),
                [],
            )?;
        }
    }

    // The canonical schema keeps both the primary key and this explicit index.
    if table_exists_tx(tx, "sessions")? && !has_unique_column(tx, "sessions", "session")? {
        ensure_no_duplicates(tx, "sessions", "session")?;
    }
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS `idx_sessions_session` ON `sessions`(`session`)",
        [],
    )?;

    for (table, column, index) in [
        ("users", "username", "uni_users_username"),
        ("clients", "token", "uni_clients_token"),
        ("oidc_providers", "name", "uni_oidc_providers_name"),
        (
            "message_sender_providers",
            "name",
            "uni_message_sender_providers_name",
        ),
        ("ddns_providers", "name", "uni_ddns_providers_name"),
        (
            "offline_notifications",
            "client",
            "uni_offline_notifications_client",
        ),
        (
            "theme_configurations",
            "short",
            "uni_theme_configurations_short",
        ),
        ("configs", "key", "uni_configs_key"),
    ] {
        if has_unique_column(tx, table, column)? {
            continue;
        }
        ensure_no_duplicates(tx, table, column)?;
        tx.execute(
            &format!("CREATE UNIQUE INDEX IF NOT EXISTS `{index}` ON `{table}`(`{column}`)"),
            [],
        )?;
    }
    Ok(())
}

#[derive(Debug)]
enum LegacyCell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob,
}

fn read_legacy_config(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
) -> crate::error::Result<Vec<(String, LegacyCell)>> {
    let columns = actual_columns(tx, table)?;
    let order = if columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("id"))
    {
        "ORDER BY `id` DESC"
    } else {
        "ORDER BY rowid DESC"
    };
    // A hybrid table produced by an older version of this migration has both
    // legacy columns and an (empty) key/value pair. Pick the newest row that
    // actually carries legacy configuration.
    let filter = if columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("sitename"))
    {
        "WHERE `sitename` IS NOT NULL"
    } else {
        ""
    };
    let mut stmt = tx.prepare(&format!("SELECT * FROM `{table}` {filter} {order} LIMIT 1"))?;
    let mut rows = stmt.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(Vec::new());
    };

    let mut values = Vec::with_capacity(columns.len());
    for (index, name) in columns.into_iter().enumerate() {
        let cell = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => LegacyCell::Null,
            rusqlite::types::ValueRef::Integer(value) => LegacyCell::Integer(value),
            rusqlite::types::ValueRef::Real(value) => LegacyCell::Real(value),
            rusqlite::types::ValueRef::Text(value) => {
                LegacyCell::Text(String::from_utf8_lossy(value).into_owned())
            }
            rusqlite::types::ValueRef::Blob(_) => LegacyCell::Blob,
        };
        values.push((name, cell));
    }
    Ok(values)
}

fn legacy_bool(cell: &LegacyCell) -> bool {
    match cell {
        LegacyCell::Integer(value) => *value != 0,
        LegacyCell::Real(value) => *value != 0.0,
        LegacyCell::Text(value) => {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        }
        LegacyCell::Null | LegacyCell::Blob => false,
    }
}

fn legacy_i64(cell: &LegacyCell) -> i64 {
    match cell {
        LegacyCell::Integer(value) => *value,
        LegacyCell::Real(value) => *value as i64,
        LegacyCell::Text(value) => value
            .trim()
            .parse::<i64>()
            .or_else(|_| value.trim().parse::<f64>().map(|number| number as i64))
            .unwrap_or_default(),
        LegacyCell::Null | LegacyCell::Blob => 0,
    }
}

fn legacy_f64(cell: &LegacyCell) -> f64 {
    match cell {
        LegacyCell::Integer(value) => *value as f64,
        LegacyCell::Real(value) => *value,
        LegacyCell::Text(value) => value.trim().parse::<f64>().unwrap_or_default(),
        LegacyCell::Null | LegacyCell::Blob => 0.0,
    }
}

fn legacy_string(cell: &LegacyCell) -> String {
    match cell {
        LegacyCell::Text(value) => value.clone(),
        LegacyCell::Integer(value) => value.to_string(),
        LegacyCell::Real(value) => value.to_string(),
        LegacyCell::Null | LegacyCell::Blob => String::new(),
    }
}

fn legacy_config_value(column: &str, cell: &LegacyCell) -> Option<serde_json::Value> {
    let name = column.to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "id" | "created_at" | "updated_at" | "key" | "value"
    ) {
        return None;
    }
    let boolean = matches!(
        name.as_str(),
        "allow_cors"
            | "private_site"
            | "send_ip_addr_to_guest"
            | "eula_accepted"
            | "geo_ip_enabled"
            | "ddns_enabled"
            | "o_auth_enabled"
            | "disable_password_login"
            | "notification_enabled"
            | "expire_notification_enabled"
            | "login_notification"
            | "record_enabled"
    );
    let integer = matches!(
        name.as_str(),
        "ddns_sync_interval" | "expire_notification_lead_days" | "record_preserve_time"
    );
    let float = name == "traffic_limit_percentage";

    Some(if boolean {
        serde_json::Value::Bool(legacy_bool(cell))
    } else if integer {
        serde_json::Value::Number(serde_json::Number::from(legacy_i64(cell)))
    } else if float {
        serde_json::Number::from_f64(legacy_f64(cell))
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::String(legacy_string(cell))
    })
}

/// Convert a pre-1.1.4 single-row `configs` table into the current key/value
/// form. The old table is renamed to `configs_legacy_backup` instead of being
/// dropped, so no data is lost.
fn existing_config_rows(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
) -> crate::error::Result<Vec<(String, String)>> {
    let columns = actual_columns(tx, table)?;
    let has_key = columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("key"));
    let has_value = columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("value"));
    if !has_key || !has_value {
        return Ok(Vec::new());
    }

    let mut stmt = tx.prepare(&format!(
        "SELECT `key`, `value` FROM `{table}` \
         WHERE `key` IS NOT NULL AND `key` <> ''"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Convert a pre-1.1.4 single-row `configs` table into the current key/value
/// form. The old table is renamed to `configs_legacy_backup` instead of being
/// dropped, so no data is lost.
fn convert_legacy_configs(tx: &rusqlite::Transaction<'_>) -> crate::error::Result<()> {
    let mut backup = "configs_legacy_backup".to_owned();
    let mut suffix = 2;
    while table_exists_tx(tx, &backup)? {
        backup = format!("configs_legacy_backup_{suffix}");
        suffix += 1;
    }

    let existing = existing_config_rows(tx, "configs")?;
    let values = read_legacy_config(tx, "configs")?;
    tx.execute_batch(&format!("ALTER TABLE `configs` RENAME TO `{backup}`"))?;
    tx.execute_batch("CREATE TABLE `configs` (`key` text, `value` text, PRIMARY KEY (`key`))")?;

    for (key, value) in existing {
        tx.execute(
            "INSERT OR REPLACE INTO `configs` (`key`, `value`) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
    }
    for (column, cell) in values {
        let Some(value) = legacy_config_value(&column, &cell) else {
            continue;
        };
        tx.execute(
            "INSERT OR REPLACE INTO `configs` (`key`, `value`) VALUES (?1, ?2)",
            rusqlite::params![column, serde_json::to_string(&value)?],
        )?;
    }
    Ok(())
}

fn configs_need_conversion(tx: &rusqlite::Transaction<'_>) -> crate::error::Result<bool> {
    if !table_exists_tx(tx, "configs")? {
        return Ok(false);
    }
    let columns = actual_columns(tx, "configs")?;
    // `sitename` wins over `key`: a hybrid table created by an older version
    // of this migration has a `key` column but still holds legacy data.
    if columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("sitename"))
    {
        return Ok(true);
    }
    if columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case("key"))
    {
        return Ok(false);
    }
    Err(crate::error::DbError::InvalidData(
        "unsupported legacy configs table: expected either (key, value) or a \
         single-row configuration with a sitename column"
            .to_owned(),
    ))
}

impl Db {
    /// Create every missing table and index, and add missing columns to old
    /// tables. Safe to call on every startup and safe to call repeatedly on an
    /// existing database.
    ///
    /// The order is table DDL -> `ALTER TABLE ADD COLUMN` -> index DDL, so an
    /// old table that lacks an indexed column (for example `records.client`)
    /// is repaired before SQLite tries to build the index.
    ///
    /// `CREATE TABLE IF NOT EXISTS` leaves older tables untouched, which means
    /// a database created by an older Komari release may lack newer columns.
    /// This method compares `PRAGMA table_info` with the target schema and
    /// only runs `ALTER TABLE ... ADD COLUMN` for the missing ones; it never
    /// drops, renames or rewrites existing columns.
    pub fn migrate(&self) -> Result<()> {
        if self.is_read_only() {
            // A read-only handle can still be used for inspection. Only fail
            // if the schema is incomplete, which the caller can detect with
            // `table_exists`.
            return Ok(());
        }

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // Convert the pre-1.1.4 single-row configs table before generic
        // column repair, because its schema is structurally different.
        if configs_need_conversion(&tx)? {
            convert_legacy_configs(&tx)?;
        }
        // Tables first: older tables keep their existing definition.
        tx.execute_batch(SCHEMA_SQL)?;
        // Then add any columns the old table is missing, so the index DDL and
        // repository queries below never reference a non-existent column.
        add_missing_columns(&tx)?;
        // Restore unique constraints that old tables may never have had.
        ensure_unique_columns(&tx)?;
        // Indexes last: they can reference columns added in the step above.
        tx.execute_batch(INDEX_SQL)?;
        // Older `sessions` tables had no `expires` column, so the additive
        // migration leaves NULL. Treat those sessions as long expired instead
        // of leaving them valid forever.
        tx.execute(
            "UPDATE sessions SET expires = '1970-01-01 00:00:00.0000000' \
             WHERE expires IS NULL",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether every table from [`TABLES`] exists.
    pub fn schema_is_current(&self) -> Result<bool> {
        for table in TABLES {
            if !self.table_exists(table)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
        assert!(db.schema_is_current().unwrap());
    }

    #[test]
    fn migration_creates_every_expected_table() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let names = db.table_names().unwrap();
        for table in TABLES {
            assert!(names.iter().any(|name| name == table), "missing {table}");
        }
    }
}
