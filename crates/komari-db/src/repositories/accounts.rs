//! User account repository (port of `database/accounts`).

use rusqlite::params;

use super::{query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::User;
use crate::time::LocalTime;
use crate::util::{generate_password, hash_password};

/// Access to the `users` table.
pub struct Accounts<'a> {
    pub(crate) db: &'a Db,
}

impl Accounts<'_> {
    /// Create an account with a salted SHA-256 password hash.
    pub fn create(&self, username: impl Into<String>, password: &str) -> Result<User> {
        let user = User::new(username, password);
        self.insert(&user)?;
        Ok(user)
    }

    /// Insert a pre-built user row (for example one created from SSO).
    pub fn insert(&self, user: &User) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO users \
             (uuid, username, passwd, sso_type, sso_id, two_factor, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                user.uuid,
                user.username,
                user.passwd,
                user.sso_type,
                user.sso_id,
                user.two_factor,
                user.created_at,
                user.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Create the initial administrator, using `ADMIN_USERNAME` /
    /// `ADMIN_PASSWORD` when present (Go `CreateDefaultAdminAccount`).
    pub fn create_default_admin(&self) -> Result<(String, String, User)> {
        let username = std::env::var("ADMIN_USERNAME")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "admin".to_owned());
        let password = std::env::var("ADMIN_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(generate_password);
        let user = self.create(username.clone(), &password)?;
        Ok((username, password, user))
    }

    /// Look up by primary key.
    pub fn get_by_uuid(&self, uuid: &str) -> Result<Option<User>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM users WHERE uuid = ?1", [uuid])
    }

    /// Look up by unique username.
    pub fn get_by_username(&self, username: &str) -> Result<Option<User>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM users WHERE username = ?1", [username])
    }

    /// Look up by OAuth subject identifier.
    pub fn get_by_sso(&self, sso_id: &str) -> Result<Option<User>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM users WHERE sso_id = ?1", [sso_id])
    }

    pub fn require_by_uuid(&self, uuid: &str) -> Result<User> {
        self.get_by_uuid(uuid)?
            .ok_or_else(|| DbError::not_found("user", uuid))
    }

    pub fn require_by_username(&self, username: &str) -> Result<User> {
        self.get_by_username(username)?
            .ok_or_else(|| DbError::not_found("user", username))
    }

    /// Check username/password. Returns the user UUID on success, matching
    /// `accounts.CheckPassword`.
    pub fn check_password(&self, username: &str, password: &str) -> Result<Option<String>> {
        match self.get_by_username(username)? {
            Some(user) if user.check_password(password) => Ok(Some(user.uuid)),
            _ => Ok(None),
        }
    }

    /// Replace all mutable fields of a user row.
    pub fn update(&self, user: &User) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE users SET username = ?1, passwd = ?2, sso_type = ?3, sso_id = ?4, \
             two_factor = ?5, created_at = ?6, updated_at = ?7 WHERE uuid = ?8",
            params![
                user.username,
                user.passwd,
                user.sso_type,
                user.sso_id,
                user.two_factor,
                user.created_at,
                user.updated_at,
                user.uuid,
            ],
        )?)
    }

    /// Change a password by user UUID. Returns the number of affected rows.
    pub fn set_password(&self, uuid: &str, password: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE users SET passwd = ?1, updated_at = ?2 WHERE uuid = ?3",
            params![hash_password(password), LocalTime::now(), uuid],
        )?)
    }

    /// Change a password by username (Go `ForceResetPassword`).
    pub fn set_password_by_username(&self, username: &str, password: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE users SET passwd = ?1, updated_at = ?2 WHERE username = ?3",
            params![hash_password(password), LocalTime::now(), username],
        )?)
    }

    /// Rename a user and/or change its SSO type.
    pub fn update_profile(
        &self,
        uuid: &str,
        username: Option<&str>,
        sso_type: Option<&str>,
    ) -> Result<usize> {
        let conn = self.db.conn();
        let mut assignments = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(username) = username {
            values.push(username.to_owned().into());
            assignments.push(format!("username = ?{}", values.len()));
        }
        if let Some(sso_type) = sso_type {
            values.push(sso_type.to_owned().into());
            assignments.push(format!("sso_type = ?{}", values.len()));
        }
        if assignments.is_empty() {
            return Ok(0);
        }
        values.push(LocalTime::now().to_storage_string().into());
        let now_index = values.len();
        values.push(uuid.to_owned().into());
        let uuid_index = values.len();
        let sql = format!(
            "UPDATE users SET {}, updated_at = ?{now_index} WHERE uuid = ?{uuid_index}",
            assignments.join(", ")
        );
        Ok(conn.execute(&sql, rusqlite::params_from_iter(values.iter()))?)
    }

    /// Bind an external (OAuth) account identifier.
    pub fn bind_sso(&self, uuid: &str, sso_id: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE users SET sso_id = ?1, updated_at = ?2 WHERE uuid = ?3",
            params![sso_id, LocalTime::now(), uuid],
        )?)
    }

    /// Remove the external account binding.
    pub fn unbind_sso(&self, uuid: &str) -> Result<usize> {
        self.bind_sso(uuid, "")
    }

    /// Store a TOTP secret.
    pub fn enable_2fa(&self, uuid: &str, secret: &str) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE users SET two_factor = ?1, updated_at = ?2 WHERE uuid = ?3",
            params![secret, LocalTime::now(), uuid],
        )?)
    }

    /// Clear the TOTP secret.
    pub fn disable_2fa(&self, uuid: &str) -> Result<usize> {
        self.enable_2fa(uuid, "")
    }

    pub fn delete(&self, uuid: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM users WHERE uuid = ?1", [uuid])? > 0)
    }

    pub fn delete_by_username(&self, username: &str) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM users WHERE username = ?1", [username])? > 0)
    }

    pub fn list(&self) -> Result<Vec<User>> {
        let conn = self.db.conn();
        query_all(&conn, "SELECT * FROM users ORDER BY rowid", [])
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM users", [])
    }
}
