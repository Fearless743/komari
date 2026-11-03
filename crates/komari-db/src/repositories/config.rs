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
    ///
    /// Values are decoded with the same fallbacks as Go: JSON `null` becomes
    /// the type's zero value and numbers are converted between integer and
    /// floating-point targets (a stored `80.0` can be read as `i64`).
    pub fn get_as<T: DeserializeOwned + Default>(&self, key: &str) -> Result<Option<T>> {
        match self.get_raw(key)? {
            Some(raw) => {
                let value: JsonValue = serde_json::from_str(&raw)?;
                Ok(Some(decode_loose(&value)?))
            }
            None => Ok(None),
        }
    }

    /// Typed decode with a default that is persisted when the key is absent
    /// (Go `config.GetAs` with a default argument).
    pub fn get_as_or<T: DeserializeOwned + Serialize + Default>(
        &self,
        key: &str,
        default: T,
    ) -> Result<T> {
        if let Some(raw) = self.get_raw(key)? {
            let value: JsonValue = serde_json::from_str(&raw)?;
            return decode_loose(&value);
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

/// Decode a JSON value with Go `config.GetAs`-style fallbacks.
fn decode_loose<T: DeserializeOwned + Default>(value: &JsonValue) -> Result<T> {
    // `json.Unmarshal("null", &t)` leaves t at its zero value and returns nil.
    if value.is_null() {
        return Ok(T::default());
    }

    if let Ok(decoded) = serde_json::from_value(value.clone()) {
        return Ok(decoded);
    }

    if let Some(number) = value.as_f64() {
        if !number.is_finite() {
            return Ok(serde_json::from_value(value.clone())?);
        }
        // Go's convertAndSet truncates floats when the target is an integer.
        let truncated = number.trunc();
        if truncated >= i64::MIN as f64 && truncated <= i64::MAX as f64 {
            if let Ok(decoded) = serde_json::from_value(JsonValue::from(truncated as i64)) {
                return Ok(decoded);
            }
        }
        if truncated >= 0.0 && truncated <= u64::MAX as f64 {
            if let Ok(decoded) = serde_json::from_value(JsonValue::from(truncated as u64)) {
                return Ok(decoded);
            }
        }
    }

    // Go's `encoding/json` decodes a JSON string into `[]byte` using base64.
    if let Some(text) = value.as_str() {
        use base64::Engine;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(text) {
            let array = JsonValue::Array(bytes.iter().copied().map(JsonValue::from).collect());
            if let Ok(decoded) = serde_json::from_value(array) {
                return Ok(decoded);
            }
        }
    }

    // Surface serde's original error for incompatible values.
    Ok(serde_json::from_value(value.clone())?)
}
