//! Provider tables: OAuth/OIDC, notification senders and DDNS providers.
//!
//! All three tables share the same `(name, addition)` shape. A repository is
//! bound to one table when it is created through [`crate::Db`].

use rusqlite::params;

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::Provider;

pub const OAUTH_PROVIDERS: &str = "oidc_providers";
pub const MESSAGE_SENDERS: &str = "message_sender_providers";
pub const DDNS_PROVIDERS: &str = "ddns_providers";

/// Access to one of the provider tables.
pub struct Providers<'a> {
    pub(crate) db: &'a Db,
    table: &'static str,
}

impl<'a> Providers<'a> {
    pub(crate) fn new(db: &'a Db, table: &'static str) -> Self {
        debug_assert!(matches!(
            table,
            OAUTH_PROVIDERS | MESSAGE_SENDERS | DDNS_PROVIDERS
        ));
        Self { db, table }
    }

    /// The underlying table name (useful for diagnostics).
    pub fn table(&self) -> &'static str {
        self.table
    }

    pub fn list(&self) -> Result<Vec<Provider>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            &format!("SELECT * FROM {} ORDER BY name", self.table),
            [],
        )
    }

    pub fn get(&self, name: &str) -> Result<Option<Provider>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            &format!("SELECT * FROM {} WHERE name = ?1", self.table),
            [name],
        )
    }

    /// Insert or replace a provider configuration (Go `Save*Config`).
    pub fn save(&self, provider: &Provider) -> Result<()> {
        if provider.name.is_empty() {
            return Err(DbError::InvalidData(
                "provider name must not be empty".to_owned(),
            ));
        }
        let conn = self.db.conn();
        conn.execute(
            &format!(
                "INSERT INTO {} (name, addition) VALUES (?1, ?2) \
                 ON CONFLICT(name) DO UPDATE SET addition = excluded.addition",
                self.table
            ),
            params![provider.name, provider.addition],
        )?;
        Ok(())
    }

    /// Alias for [`Providers::save`] matching Go's `SaveXxxConfig` naming.
    pub fn upsert(&self, provider: &Provider) -> Result<()> {
        self.save(provider)
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute(
            &format!("DELETE FROM {} WHERE name = ?1", self.table),
            [name],
        )? > 0)
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, &format!("SELECT COUNT(*) FROM {}", self.table), [])
    }
}
