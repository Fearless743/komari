//! Key/value configuration repository (port of the Go `config` package).

use std::collections::HashMap;

use rusqlite::params;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use super::{query_all, query_optional};
use crate::db::Db;
use crate::error::Result;
use crate::models::ConfigItem;

/// Access to the `configs` key/value table.
pub struct ConfigRepo<'a> {
    pub(crate) db: &'a Db,
}

impl ConfigRepo<'_> {
    /// Raw JSON text for a key.
    pub fn get_raw(&self, key: &str) -> Result<Option<String>> {
        let conn = self.db.conn();
        let item: Option<ConfigItem> =
            query_optional(&conn, "SELECT * FROM configs WHERE key = ?1", [key])?;
        Ok(item.map(|item| item.value))
    }

    /// Store raw JSON text for a key.
    pub fn set_raw(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO configs (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Decode a value as generic JSON (Go `config.Get`).
    pub fn get(&self, key: &str) -> Result<Option<JsonValue>> {
        match self.get_raw(key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// Decode a value, writing `default` when the key is absent
    /// (Go `config.Get` with a default argument).
    pub fn get_or(&self, key: &str, default: JsonValue) -> Result<JsonValue> {
        if let Some(value) = self.get(key)? {
            return Ok(value);
        }
        self.set_value(key, &default)?;
        Ok(default)
    }

    /// Typed decode (Go `config.GetAs`). Returns `None` when the key is absent.
    pub fn get_as<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get_raw(key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// Typed decode with a default that is persisted when the key is absent
    /// (Go `config.GetAs` with a default argument).
    pub fn get_as_or<T: DeserializeOwned + Serialize>(&self, key: &str, default: T) -> Result<T> {
        if let Some(raw) = self.get_raw(key)? {
            return Ok(serde_json::from_str(&raw)?);
        }
        self.set(key, &default)?;
        Ok(default)
    }

    /// Serialize and store a value as compact JSON.
    pub fn set<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        self.set_raw(key, &raw)
    }

    /// Store an already-decoded JSON value.
    pub fn set_value(&self, key: &str, value: &JsonValue) -> Result<()> {
        self.set(key, value)
    }

    /// Remove a key. Returns `true` when a row was deleted.
    pub fn remove(&self, key: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM configs WHERE key = ?1", [key])? > 0)
    }

    pub fn contains(&self, key: &str) -> Result<bool> {
        let conn = self.db.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM configs WHERE key = ?1",
            [key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// All configuration entries whose values are valid JSON (Go
    /// `config.GetAll`); invalid rows are skipped.
    pub fn get_all(&self) -> Result<HashMap<String, JsonValue>> {
        let conn = self.db.conn();
        let items: Vec<ConfigItem> = query_all(&conn, "SELECT * FROM configs", [])?;
        let mut result = HashMap::with_capacity(items.len());
        for item in items {
            if let Ok(value) = serde_json::from_str(&item.value) {
                result.insert(item.key, value);
            }
        }
        Ok(result)
    }

    /// Fetch several values at once. Missing keys are omitted.
    pub fn get_many(&self, keys: &[&str]) -> Result<HashMap<String, JsonValue>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (1..=keys.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let conn = self.db.conn();
        let items: Vec<ConfigItem> = query_all(
            &conn,
            &format!("SELECT * FROM configs WHERE key IN ({placeholders})"),
            rusqlite::params_from_iter(keys.iter()),
        )?;
        let mut result = HashMap::with_capacity(items.len());
        for item in items {
            if let Ok(value) = serde_json::from_str(&item.value) {
                result.insert(item.key, value);
            }
        }
        Ok(result)
    }

    /// Upsert many values in a single transaction (Go `config.SetMany`).
    pub fn set_many(&self, values: &HashMap<String, JsonValue>) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO configs (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?;
            for (key, value) in values {
                stmt.execute(params![key, serde_json::to_string(value)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Number of configuration rows.
    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM configs", [], |row| row.get(0))?;
        Ok(count)
    }
}
