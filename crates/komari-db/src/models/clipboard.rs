use serde::{Deserialize, Serialize};

use super::FromRow;
use crate::time::LocalTime;

/// A clipboard entry (`clipboards`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Clipboard {
    pub id: i64,
    pub text: String,
    pub name: String,
    pub weight: i64,
    pub remark: String,
    pub created_at: LocalTime,
    pub updated_at: LocalTime,
}

impl FromRow for Clipboard {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            text: row.get("text")?,
            name: row.get("name")?,
            weight: row.get("weight")?,
            remark: row.get("remark")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}
