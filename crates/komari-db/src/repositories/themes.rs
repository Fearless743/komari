//! Theme configuration repository.

use rusqlite::params;
use serde_json::Value;

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::Result;
use crate::models::ThemeConfiguration;

/// Access to `theme_configurations`.
pub struct Themes<'a> {
    pub(crate) db: &'a Db,
}

impl Themes<'_> {
    pub fn get(&self, short: &str) -> Result<Option<ThemeConfiguration>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT * FROM theme_configurations WHERE short = ?1",
            [short],
        )
    }

    /// Fetch a theme's configuration object, defaulting to `{}` when the row
    /// is absent or contains invalid JSON (Go `GetPublicInfo`).
    pub fn get_data(&self, short: &str) -> Result<Value> {
        match self.get(short)? {
            Some(config) => Ok(serde_json::from_str(&config.data)
                .unwrap_or_else(|_| Value::Object(Default::default()))),
            None => Ok(Value::Object(Default::default())),
        }
    }

    /// Store raw JSON text.
    pub fn set_raw(&self, short: &str, data: &str) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO theme_configurations (short, data) VALUES (?1, ?2) \
             ON CONFLICT(short) DO UPDATE SET data = excluded.data",
            params![short, data],
        )?;
        Ok(())
    }

    /// Serialize and store a configuration object.
    pub fn set(&self, short: &str, data: &Value) -> Result<()> {
        self.set_raw(short, &serde_json::to_string(data)?)
    }

    pub fn list(&self) -> Result<Vec<ThemeConfiguration>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            "SELECT * FROM theme_configurations ORDER BY short",
            [],
        )
    }

    pub fn delete(&self, short: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM theme_configurations WHERE short = ?1", [short])? > 0)
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM theme_configurations", [])
    }
}
