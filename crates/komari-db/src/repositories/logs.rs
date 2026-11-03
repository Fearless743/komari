//! Audit log repository (port of `database/auditlog`).

use chrono::TimeDelta;
use rusqlite::params;

use super::{query_all, query_i64};
use crate::db::Db;
use crate::error::Result;
use crate::models::Log;
use crate::time::LocalTime;

/// Access to the `logs` table.
pub struct Logs<'a> {
    pub(crate) db: &'a Db,
}

impl Logs<'_> {
    /// Insert a log row and return its generated id.
    pub fn insert(&self, entry: &Log) -> Result<i64> {
        let time = if entry.time.is_zero() {
            LocalTime::now()
        } else {
            entry.time
        };
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO logs (ip, uuid, message, msg_type, time) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry.ip, entry.uuid, entry.message, entry.msg_type, time],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Port of `auditlog.Log`.
    pub fn log(
        &self,
        ip: &str,
        uuid: &str,
        message: impl Into<String>,
        msg_type: &str,
    ) -> Result<i64> {
        self.insert(&Log {
            id: 0,
            ip: ip.to_owned(),
            uuid: uuid.to_owned(),
            message: message.into(),
            msg_type: msg_type.to_owned(),
            time: LocalTime::now(),
        })
    }

    /// Newest entries first (`time DESC, id DESC`).
    pub fn list(&self, limit: i64) -> Result<Vec<Log>> {
        self.list_page(limit, 0)
    }

    /// Newest entries first with `LIMIT`/`OFFSET` pagination.
    pub fn list_page(&self, limit: i64, offset: i64) -> Result<Vec<Log>> {
        let limit = if limit <= 0 { 100 } else { limit };
        let offset = offset.max(0);
        let conn = self.db.conn();
        query_all(
            &conn,
            "SELECT * FROM logs ORDER BY time DESC, id DESC LIMIT ?1 OFFSET ?2",
            params![limit, offset],
        )
    }

    /// Newest entries of one message type first.
    pub fn list_by_type(&self, msg_type: &str, limit: i64) -> Result<Vec<Log>> {
        self.list_by_type_page(msg_type, limit, 0)
    }

    /// Paginated variant of [`Logs::list_by_type`].
    pub fn list_by_type_page(&self, msg_type: &str, limit: i64, offset: i64) -> Result<Vec<Log>> {
        let limit = if limit <= 0 { 100 } else { limit };
        let offset = offset.max(0);
        let conn = self.db.conn();
        query_all(
            &conn,
            "SELECT * FROM logs WHERE msg_type = ?1 \
             ORDER BY time DESC, id DESC LIMIT ?2 OFFSET ?3",
            params![msg_type, limit, offset],
        )
    }

    pub fn delete_before(&self, before: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM logs WHERE time < ?1", [before])?)
    }

    /// Delete entries older than `days` days (Go `RemoveOldLogs`, 30 days).
    pub fn remove_old(&self, days: i64) -> Result<usize> {
        let now = LocalTime::now();
        let cutoff = LocalTime::from_naive(
            now.to_naive() - TimeDelta::try_days(days.max(0)).unwrap_or_default(),
        );
        self.delete_before(cutoff)
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM logs", [])
    }
}
