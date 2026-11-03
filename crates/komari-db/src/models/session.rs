use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;
use crate::util::generate_random_string;

/// A user login session (`sessions` table).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub uuid: String,
    pub session: String,
    pub user_agent: String,
    pub ip: String,
    pub login_method: String,
    pub latest_online: LocalTime,
    pub latest_user_agent: String,
    pub latest_ip: String,
    pub expires: LocalTime,
    pub created_at: LocalTime,
}

impl Session {
    /// Build a session with a fresh random identifier, matching
    /// `accounts.CreateSession`.
    pub fn new(
        user_uuid: impl Into<String>,
        expires_in_seconds: i64,
        user_agent: impl Into<String>,
        ip: impl Into<String>,
        login_method: impl Into<String>,
    ) -> Self {
        let now = LocalTime::now();
        let expires = now.add_seconds(expires_in_seconds);
        Self {
            uuid: user_uuid.into(),
            session: generate_random_string(32),
            user_agent: user_agent.into(),
            ip: ip.into(),
            login_method: login_method.into(),
            latest_online: now,
            latest_user_agent: String::new(),
            latest_ip: String::new(),
            expires,
            created_at: now,
        }
    }

    pub fn is_expired(&self) -> bool {
        !self.expires.is_zero() && self.expires < LocalTime::now()
    }
}

impl FromRow for Session {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            uuid: row.get("uuid")?,
            session: row.get("session")?,
            user_agent: row.get("user_agent")?,
            ip: row.get("ip")?,
            login_method: row.get("login_method")?,
            latest_online: row.get("latest_online")?,
            latest_user_agent: row.get("latest_user_agent")?,
            latest_ip: row.get("latest_ip")?,
            expires: row.get("expires")?,
            created_at: row.get("created_at")?,
        })
    }
}
