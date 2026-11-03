//! Login session repository (port of `database/accounts/sessions.go`).

use rusqlite::params;

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::Result;
use crate::models::{Session, User};
use crate::time::LocalTime;

/// Access to the `sessions` table.
pub struct Sessions<'a> {
    pub(crate) db: &'a Db,
}

impl Sessions<'_> {
    /// Insert a session row.
    pub fn insert(&self, session: &Session) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO sessions \
             (uuid, session, user_agent, ip, login_method, latest_online, latest_user_agent, \
              latest_ip, expires, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                session.uuid,
                session.session,
                session.user_agent,
                session.ip,
                session.login_method,
                session.latest_online,
                session.latest_user_agent,
                session.latest_ip,
                session.expires,
                session.created_at,
            ],
        )?;
        Ok(())
    }

    /// Create and persist a session, returning the generated record.
    pub fn create(
        &self,
        user_uuid: impl Into<String>,
        expires_in_seconds: i64,
        user_agent: impl Into<String>,
        ip: impl Into<String>,
        login_method: impl Into<String>,
    ) -> Result<Session> {
        let session = Session::new(user_uuid, expires_in_seconds, user_agent, ip, login_method);
        self.insert(&session)?;
        Ok(session)
    }

    /// Create a session and return only its opaque identifier
    /// (Go `accounts.CreateSession`).
    pub fn create_id(
        &self,
        user_uuid: impl Into<String>,
        expires_in_seconds: i64,
        user_agent: impl Into<String>,
        ip: impl Into<String>,
        login_method: impl Into<String>,
    ) -> Result<String> {
        Ok(self
            .create(user_uuid, expires_in_seconds, user_agent, ip, login_method)?
            .session)
    }

    /// Raw lookup without expiry handling.
    pub fn get(&self, session: &str) -> Result<Option<Session>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT * FROM sessions WHERE session = ?1",
            [session],
        )
    }

    /// Validate a session. Expired sessions are deleted and yield `None`,
    /// matching Go `GetSession`.
    pub fn validate(&self, session: &str) -> Result<Option<String>> {
        let Some(record) = self.get(session)? else {
            return Ok(None);
        };
        if record.is_expired() {
            self.delete(session)?;
            return Ok(None);
        }
        Ok(Some(record.uuid))
    }

    /// Load the user that owns a session.
    pub fn user_by_session(&self, session: &str) -> Result<Option<User>> {
        let conn = self.db.conn();
        query_optional(
            &conn,
            "SELECT users.* FROM users \
             INNER JOIN sessions ON sessions.uuid = users.uuid \
             WHERE sessions.session = ?1",
            [session],
        )
    }

    pub fn delete(&self, session: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM sessions WHERE session = ?1", [session])? > 0)
    }

    pub fn delete_all(&self) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM sessions", [])?)
    }

    pub fn delete_for_user(&self, user_uuid: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM sessions WHERE uuid = ?1", [user_uuid])?)
    }

    /// Remove every session whose `expires` is before `now`.
    pub fn delete_expired(&self) -> Result<usize> {
        self.delete_expired_before(LocalTime::now())
    }

    /// Remove every session that expired before the supplied timestamp.
    pub fn delete_expired_before(&self, now: LocalTime) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM sessions WHERE expires < ?1", [now])?)
    }

    /// Refresh `latest_online`, `latest_user_agent` and `latest_ip`.
    pub fn update_latest(&self, session: &str, user_agent: &str, ip: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE sessions SET latest_online = ?1, latest_user_agent = ?2, latest_ip = ?3 \
             WHERE session = ?4",
            params![LocalTime::now(), user_agent, ip, session],
        )?)
    }

    pub fn update_latest_online(&self, session: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE sessions SET latest_online = ?1 WHERE session = ?2",
            params![LocalTime::now(), session],
        )?)
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let conn = self.db.conn();
        query_all(&conn, "SELECT * FROM sessions ORDER BY rowid", [])
    }

    pub fn list_for_user(&self, user_uuid: &str) -> Result<Vec<Session>> {
        let conn = self.db.conn();
        query_all(
            &conn,
            "SELECT * FROM sessions WHERE uuid = ?1 ORDER BY rowid",
            [user_uuid],
        )
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM sessions", [])
    }
}
