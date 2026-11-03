use serde::{Deserialize, Serialize};

use super::{FromRow, Session};
use crate::time::LocalTime;

/// An authenticated user (`users` table).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct User {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uuid: String,
    pub username: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub passwd: String,
    #[serde(rename = "sso_type")]
    pub sso_type: String,
    #[serde(rename = "sso_id")]
    pub sso_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub two_factor: String,
    /// Relationship loaded by GORM; it is not a column on `users`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    pub created_at: LocalTime,
    pub updated_at: LocalTime,
}

impl User {
    pub fn new(username: impl Into<String>, password: &str) -> Self {
        let now = LocalTime::now();
        Self {
            uuid: uuid::Uuid::new_v4().to_string(),
            username: username.into(),
            passwd: crate::util::hash_password(password),
            created_at: now,
            updated_at: now,
            ..Self::default()
        }
    }

    /// Check a plaintext password against the stored salted hash.
    pub fn check_password(&self, password: &str) -> bool {
        crate::util::verify_password(password, &self.passwd)
    }
}

impl FromRow for User {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            uuid: row.get("uuid")?,
            username: row.get("username")?,
            passwd: row.get("passwd")?,
            sso_type: row.get("sso_type")?,
            sso_id: row.get("sso_id")?,
            two_factor: row.get("two_factor")?,
            sessions: Vec::new(),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}
