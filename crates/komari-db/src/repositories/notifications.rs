//! Offline and load notification repositories.

use rusqlite::params;

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::{LoadNotification, OfflineNotification};
use crate::time::LocalTime;

/// Access to `offline_notifications` and `load_notifications`.
pub struct Notifications<'a> {
    pub(crate) db: &'a Db,
}

impl Notifications<'_> {
    pub fn offline_list(&self) -> Result<Vec<OfflineNotification>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            "SELECT * FROM offline_notifications ORDER BY rowid",
            [],
        )
    }

    pub fn offline_get(&self, client: &str) -> Result<Option<OfflineNotification>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT * FROM offline_notifications WHERE client = ?1",
            [client],
        )
    }

    /// Insert or replace the settings for one client.
    pub fn offline_save(&self, notification: &OfflineNotification) -> Result<()> {
        let mut notification = notification.clone();
        // The Go model relies on `default:180`.
        if notification.grace_period == 0 {
            notification.grace_period = 180;
        }
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO offline_notifications (client, enable, grace_period, last_notified) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(client) DO UPDATE SET \
                 enable = excluded.enable, \
                 grace_period = excluded.grace_period, \
                 last_notified = excluded.last_notified",
            params![
                notification.client,
                notification.enable,
                notification.grace_period,
                notification.last_notified,
            ],
        )?;
        Ok(())
    }

    pub fn offline_set_enabled(&self, client: &str, enabled: bool) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE offline_notifications SET enable = ?1 WHERE client = ?2",
            params![enabled, client],
        )?)
    }

    pub fn offline_set_last_notified(&self, client: &str, at: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE offline_notifications SET last_notified = ?1 WHERE client = ?2",
            params![at, client],
        )?)
    }

    pub fn offline_delete(&self, client: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "DELETE FROM offline_notifications WHERE client = ?1",
            [client],
        )? > 0)
    }

    pub fn offline_count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM offline_notifications", [])
    }

    pub fn load_list(&self) -> Result<Vec<LoadNotification>> {
        let conn = self.db.conn();
        query_all(&conn, "SELECT * FROM load_notifications ORDER BY id", [])
    }

    pub fn load_get(&self, id: i64) -> Result<Option<LoadNotification>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT * FROM load_notifications WHERE id = ?1",
            [id],
        )
    }

    /// Insert a new rule and return its generated id (Go
    /// `AddLoadNotification`).
    pub fn load_insert(&self, notification: &LoadNotification) -> Result<i64> {
        let mut notification = notification.clone();
        // Reproduce the database defaults declared by the Go model.
        if notification.metric.is_empty() {
            notification.metric = "cpu".to_owned();
        }
        if notification.threshold == 0.0 {
            notification.threshold = 80.0;
        }
        if notification.ratio == 0.0 {
            notification.ratio = 0.8;
        }
        if notification.interval == 0 {
            notification.interval = 15;
        }
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO load_notifications \
             (name, clients, metric, threshold, ratio, interval, last_notified) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                notification.name,
                notification.clients,
                notification.metric,
                notification.threshold,
                notification.ratio,
                notification.interval,
                notification.last_notified,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Update an existing rule by id. Returns the number of affected rows.
    pub fn load_update(&self, notification: &LoadNotification) -> Result<usize> {
        if notification.id == 0 {
            return Err(DbError::InvalidData(
                "load notification id must be set for update".to_owned(),
            ));
        }
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE load_notifications SET name = ?1, clients = ?2, metric = ?3, \
             threshold = ?4, ratio = ?5, interval = ?6, last_notified = ?7 WHERE id = ?8",
            params![
                notification.name,
                notification.clients,
                notification.metric,
                notification.threshold,
                notification.ratio,
                notification.interval,
                notification.last_notified,
                notification.id,
            ],
        )?)
    }

    /// Update only the cooldown timestamp, matching Go's
    /// `updateLastNotified` (`Update("last_notified", ...)`).
    pub fn load_set_last_notified(&self, id: i64, at: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE load_notifications SET last_notified = ?1 WHERE id = ?2",
            params![at, id],
        )?)
    }

    /// Insert when `id == 0`, otherwise update.
    pub fn load_save(&self, notification: &mut LoadNotification) -> Result<i64> {
        if notification.id == 0 {
            let id = self.load_insert(notification)?;
            notification.id = id;
            Ok(id)
        } else {
            self.load_update(notification)?;
            Ok(notification.id)
        }
    }

    pub fn load_delete(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = (1..=ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let conn = self.db.conn();
        Ok(conn.execute(
            &format!("DELETE FROM load_notifications WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(ids.iter()),
        )?)
    }

    pub fn load_count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM load_notifications", [])
    }
}
