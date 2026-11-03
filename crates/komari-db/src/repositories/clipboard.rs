//! Clipboard repository (port of `database/clipboard`).

use rusqlite::params;

use super::{UpdateBuilder, query_all, query_i64, query_optional};
use crate::db::Db;
use crate::error::Result;
use crate::models::Clipboard;
use crate::time::LocalTime;

/// Partial update for a clipboard row.
#[derive(Clone, Debug, Default)]
pub struct ClipboardPatch {
    pub text: Option<String>,
    pub name: Option<String>,
    pub weight: Option<i64>,
    pub remark: Option<String>,
}

/// Access to the `clipboards` table.
pub struct ClipboardRepo<'a> {
    pub(crate) db: &'a Db,
}

impl ClipboardRepo<'_> {
    /// Insert a row and return its generated id.
    pub fn insert(&self, clipboard: &Clipboard) -> Result<i64> {
        let now = LocalTime::now();
        let created_at = if clipboard.created_at.is_zero() {
            now
        } else {
            clipboard.created_at
        };
        let updated_at = if clipboard.updated_at.is_zero() {
            now
        } else {
            clipboard.updated_at
        };
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO clipboards (text, name, weight, remark, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                clipboard.text,
                clipboard.name,
                clipboard.weight,
                clipboard.remark,
                created_at,
                updated_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Convenience constructor.
    pub fn create(&self, text: impl Into<String>, name: impl Into<String>) -> Result<Clipboard> {
        let now = LocalTime::now();
        let mut clipboard = Clipboard {
            id: 0,
            text: text.into(),
            name: name.into(),
            weight: 0,
            remark: String::new(),
            created_at: now,
            updated_at: now,
        };
        clipboard.id = self.insert(&clipboard)?;
        Ok(clipboard)
    }

    pub fn get(&self, id: i64) -> Result<Option<Clipboard>> {
        let conn = self.db.conn();
        query_optional(&conn, "SELECT * FROM clipboards WHERE id = ?1", [id])
    }

    pub fn list(&self) -> Result<Vec<Clipboard>> {
        let conn = self.db.conn();
        query_all(&conn, "SELECT * FROM clipboards ORDER BY id", [])
    }

    /// Replace all mutable fields.
    pub fn update(&self, clipboard: &Clipboard) -> Result<usize> {
        let conn = self.db.conn();
        Ok(conn.execute(
            "UPDATE clipboards SET text = ?1, name = ?2, weight = ?3, remark = ?4, \
             updated_at = ?5 WHERE id = ?6",
            params![
                clipboard.text,
                clipboard.name,
                clipboard.weight,
                clipboard.remark,
                LocalTime::now(),
                clipboard.id,
            ],
        )?)
    }

    /// Apply a partial update (Go `UpdateClipboardFields`).
    pub fn patch(&self, id: i64, patch: &ClipboardPatch) -> Result<usize> {
        let mut builder = UpdateBuilder::new();
        builder
            .set_opt("text", patch.text.as_ref())?
            .set_opt("name", patch.name.as_ref())?
            .set_opt("weight", patch.weight.as_ref())?
            .set_opt("remark", patch.remark.as_ref())?;
        if builder.is_empty() {
            return Ok(0);
        }
        builder.set("updated_at", &LocalTime::now())?;
        let conn = self.db.conn();
        builder.finish(
            &conn,
            "clipboards",
            "id = ?1",
            vec![rusqlite::types::Value::Integer(id)],
        )
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.db.conn();
        Ok(conn.execute("DELETE FROM clipboards WHERE id = ?1", [id])? > 0)
    }

    pub fn delete_batch(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = (1..=ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let conn = self.db.conn();
        Ok(conn.execute(
            &format!("DELETE FROM clipboards WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(ids.iter()),
        )?)
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.db.conn();
        query_i64(&conn, "SELECT COUNT(*) FROM clipboards", [])
    }
}
