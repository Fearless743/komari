use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;

/// An audit log entry (`logs`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Log {
    pub id: i64,
    #[serde(rename = "ip")]
    pub ip: String,
    pub uuid: String,
    pub message: String,
    pub msg_type: String,
    pub time: LocalTime,
}

impl Log {
    pub fn new(message: impl Into<String>, msg_type: impl Into<String>) -> Self {
        Self {
            id: 0,
            ip: String::new(),
            uuid: String::new(),
            message: message.into(),
            msg_type: msg_type.into(),
            time: LocalTime::now(),
        }
    }
}

impl FromRow for Log {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            ip: row.get("ip")?,
            uuid: row.get("uuid")?,
            message: row.get("message")?,
            msg_type: row.get("msg_type")?,
            time: row.get("time")?,
        })
    }
}
