//! Typed repositories over the SQLite schema.
//!
//! Repositories are cheap borrows created from a [`Db`]:
//!
//! ```no_run
//! # use komari_db::Db;
//! # fn main() -> komari_db::Result<()> {
//! let db = Db::open_in_memory()?;
//! db.migrate()?;
//! let client = komari_db::models::Client::new("web-1");
//! db.clients().create(&client)?;
//! assert_eq!(db.clients().get_by_token(&client.token)?.unwrap().uuid, client.uuid);
//! # Ok(())
//! # }
//! ```

pub mod accounts;
pub mod clients;
pub mod clipboard;
pub mod config;
pub mod ddns;
pub mod logs;
pub mod notifications;
pub mod providers;
pub mod records;
pub mod sessions;
pub mod themes;

use rusqlite::types::{ToSql, ToSqlOutput, Value, ValueRef};
use rusqlite::{Connection, Params, params_from_iter};

use crate::db::Db;
use crate::error::{DbError, Result};
use crate::models::FromRow;

pub use accounts::Accounts;
pub use clients::{ClientPatch, Clients};
pub use clipboard::ClipboardRepo;
pub use config::ConfigRepo;
pub use ddns::Ddns;
pub use logs::Logs;
pub use notifications::Notifications;
pub use providers::Providers;
pub use records::{AggregateFn, RecordMetric, RecordTable, Records};
pub use sessions::Sessions;
pub use themes::Themes;

impl Db {
    /// Client/device repository.
    pub fn clients(&self) -> Clients<'_> {
        Clients { db: self }
    }

    /// User account repository (Go's `database/accounts`).
    pub fn accounts(&self) -> Accounts<'_> {
        Accounts { db: self }
    }

    /// Login session repository.
    pub fn sessions(&self) -> Sessions<'_> {
        Sessions { db: self }
    }

    /// Metric record repository.
    pub fn records(&self) -> Records<'_> {
        Records { db: self }
    }

    /// Key/value configuration repository.
    pub fn config(&self) -> ConfigRepo<'_> {
        ConfigRepo { db: self }
    }

    /// Offline/load notification repository.
    pub fn notifications(&self) -> Notifications<'_> {
        Notifications { db: self }
    }

    /// Notification sender provider repository (`message_sender_providers`).
    pub fn message_senders(&self) -> Providers<'_> {
        Providers::new(self, providers::MESSAGE_SENDERS)
    }

    /// OAuth/OIDC provider repository (`oidc_providers`).
    pub fn oauth(&self) -> Providers<'_> {
        Providers::new(self, providers::OAUTH_PROVIDERS)
    }

    /// Alias for [`Db::oauth`].
    pub fn oauth_providers(&self) -> Providers<'_> {
        self.oauth()
    }

    /// DDNS provider repository (`ddns_providers`).
    pub fn ddns_providers(&self) -> Providers<'_> {
        Providers::new(self, providers::DDNS_PROVIDERS)
    }

    /// DDNS synchronization history repository.
    pub fn ddns(&self) -> Ddns<'_> {
        Ddns { db: self }
    }

    /// Theme configuration repository.
    pub fn themes(&self) -> Themes<'_> {
        Themes { db: self }
    }

    /// Clipboard repository.
    pub fn clipboard(&self) -> ClipboardRepo<'_> {
        ClipboardRepo { db: self }
    }

    /// Audit log repository.
    pub fn logs(&self) -> Logs<'_> {
        Logs { db: self }
    }
}

pub(crate) fn query_optional<T: FromRow>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
) -> Result<Option<T>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    match rows.next()? {
        Some(row) => Ok(Some(T::from_row(row)?)),
        None => Ok(None),
    }
}

pub(crate) fn query_all<T: FromRow>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
) -> Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, T::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn query_i64(conn: &Connection, sql: &str, params: impl Params) -> Result<i64> {
    Ok(conn.query_row(sql, params, |row| row.get(0))?)
}

fn to_owned_value<T: ToSql>(value: &T) -> Result<Value> {
    match value.to_sql()? {
        ToSqlOutput::Owned(value) => Ok(value),
        ToSqlOutput::Borrowed(ValueRef::Null) => Ok(Value::Null),
        ToSqlOutput::Borrowed(ValueRef::Integer(value)) => Ok(Value::Integer(value)),
        ToSqlOutput::Borrowed(ValueRef::Real(value)) => Ok(Value::Real(value)),
        ToSqlOutput::Borrowed(ValueRef::Text(value)) => {
            Ok(Value::Text(String::from_utf8_lossy(value).into_owned()))
        }
        ToSqlOutput::Borrowed(ValueRef::Blob(value)) => Ok(Value::Blob(value.to_vec())),
        _ => Err(DbError::InvalidData(
            "unsupported SQLite parameter type".to_owned(),
        )),
    }
}

/// Incrementally builds an `UPDATE` statement with bound parameters.
pub(crate) struct UpdateBuilder {
    assignments: Vec<String>,
    values: Vec<Value>,
}

impl UpdateBuilder {
    pub(crate) fn new() -> Self {
        Self {
            assignments: Vec::new(),
            values: Vec::new(),
        }
    }

    pub(crate) fn set<T: ToSql>(&mut self, column: &str, value: &T) -> Result<&mut Self> {
        self.values.push(to_owned_value(value)?);
        self.assignments
            .push(format!("{column} = ?{}", self.values.len()));
        Ok(self)
    }

    pub(crate) fn set_opt<T: ToSql>(
        &mut self,
        column: &str,
        value: Option<&T>,
    ) -> Result<&mut Self> {
        if let Some(value) = value {
            self.set(column, value)?;
        }
        Ok(self)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }

    pub(crate) fn finish(
        self,
        conn: &Connection,
        table: &str,
        where_clause: &str,
        mut where_values: Vec<Value>,
    ) -> Result<usize> {
        if self.assignments.is_empty() {
            return Ok(0);
        }

        let mut values = self.values;
        let offset = values.len();
        // Renumber the WHERE placeholders after the SET placeholders.
        let where_clause = renumber_placeholders(where_clause, offset);
        values.append(&mut where_values);

        let sql = format!(
            "UPDATE {table} SET {} WHERE {where_clause}",
            self.assignments.join(", ")
        );
        Ok(conn.execute(&sql, params_from_iter(values.iter()))?)
    }
}

/// Replace `?1`, `?2`, ... in a fragment with placeholders shifted by `offset`.
fn renumber_placeholders(fragment: &str, offset: usize) -> String {
    let mut result = String::with_capacity(fragment.len());
    let mut chars = fragment.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '?' {
            let mut number = String::new();
            while let Some(next) = chars.peek() {
                if next.is_ascii_digit() {
                    number.push(*next);
                    chars.next();
                } else {
                    break;
                }
            }
            if number.is_empty() {
                result.push('?');
            } else if let Ok(value) = number.parse::<usize>() {
                result.push('?');
                result.push_str(&(value + offset).to_string());
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::renumber_placeholders;

    #[test]
    fn renumbers_where_placeholders() {
        assert_eq!(renumber_placeholders("uuid = ?1", 3), "uuid = ?4");
        assert_eq!(
            renumber_placeholders("uuid = ?1 AND token = ?2", 1),
            "uuid = ?2 AND token = ?3"
        );
    }
}
