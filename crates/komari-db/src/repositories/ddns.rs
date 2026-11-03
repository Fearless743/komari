//! DDNS synchronization history repository.

use rusqlite::params;

use super::{query_all, query_i64};
use crate::db::Db;
use crate::error::Result;
use crate::models::DdnsSyncHistory;
use crate::time::LocalTime;

/// Access to `ddns_sync_history`.
pub struct Ddns<'a> {
    pub(crate) db: &'a Db,
}

impl Ddns<'_> {
    /// Insert a history entry and return its generated id (Go
    /// `SaveDdnsSyncHistory`).
    pub fn save_history(&self, history: &DdnsSyncHistory) -> Result<i64> {
        let synced_at = if history.synced_at.is_zero() {
            LocalTime::now()
        } else {
            history.synced_at
        };
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO ddns_sync_history \
             (client_uuid, client_name, hostname, record_type, ip_v4, ip_v6, record_id, \
              status, error, triggered_by, synced_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                history.client_uuid,
                history.client_name,
                history.hostname,
                history.record_type,
                history.ipv4,
                history.ipv6,
                history.record_id,
                history.status,
                history.error,
                history.triggered_by,
                synced_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Most recent entries, newest first. `limit <= 0` means 50, matching Go
    /// `GetDdnsSyncHistory`.
    pub fn history(&self, client_uuid: &str, limit: i64) -> Result<Vec<DdnsSyncHistory>> {
        let limit = if limit <= 0 { 50 } else { limit };
        let conn = self.db.conn();
        if client_uuid.is_empty() {
            query_all(
                &conn,
                "SELECT * FROM ddns_sync_history ORDER BY id DESC LIMIT ?1",
                [limit],
            )
        } else {
            query_all(
                &conn,
                "SELECT * FROM ddns_sync_history WHERE client_uuid = ?1 \
                 ORDER BY id DESC LIMIT ?2",
                params![client_uuid, limit],
            )
        }
    }

    /// Most recent entries for one client.
    pub fn history_for_client(
        &self,
        client_uuid: &str,
        limit: i64,
    ) -> Result<Vec<DdnsSyncHistory>> {
        self.history(client_uuid, limit)
    }

    /// Delete entries older than `before` (Go `DeleteDdnsSyncHistoryBefore`).
    pub fn delete_history_before(&self, before: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "DELETE FROM ddns_sync_history WHERE synced_at < ?1",
            [before],
        )?)
    }

    pub fn delete_history_for_client(&self, client_uuid: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "DELETE FROM ddns_sync_history WHERE client_uuid = ?1",
            [client_uuid],
        )?)
    }

    pub fn history_count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM ddns_sync_history", [])
    }
}
