//! Database handle, connection options and schema migration.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, OpenFlags};

use crate::error::{DbError, Result};

/// Default SQLite path used by the Go backend (`./data/komari.db`).
pub const DEFAULT_DB_FILE: &str = "./data/komari.db";

/// Options controlling how a SQLite database is opened.
#[derive(Clone, Debug)]
pub struct DbOptions {
    /// Path to the SQLite file. `:memory:` opens an in-memory database.
    pub path: PathBuf,
    /// Open with `SQLITE_OPEN_READ_ONLY`. Migrations cannot run in this mode.
    pub read_only: bool,
    /// Create missing parent directories before opening the file.
    pub create_parent_dirs: bool,
    /// Enable WAL journaling, as the Go backend does for SQLite.
    pub wal: bool,
    /// Enable SQLite foreign-key enforcement. The Go backend leaves SQLite's
    /// default (disabled) in place, so this defaults to `false`.
    pub foreign_keys: bool,
    /// SQLite busy timeout.
    pub busy_timeout: Duration,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            path: PathBuf::from(DEFAULT_DB_FILE),
            read_only: false,
            create_parent_dirs: true,
            wal: true,
            foreign_keys: false,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

impl DbOptions {
    /// Build options for a writable database at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// A read-only handle, useful for inspecting an existing database.
    pub fn read_only(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            read_only: true,
            create_parent_dirs: false,
            wal: false,
            foreign_keys: false,
            ..Self::default()
        }
    }

    /// Resolve `KOMARI_DB_TYPE` and `KOMARI_DB_FILE`, matching the Go flags.
    ///
    /// Only SQLite is implemented; asking for MySQL returns
    /// [`DbError::UnsupportedDatabase`].
    pub fn from_env() -> Result<Self> {
        let db_type = std::env::var("KOMARI_DB_TYPE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "sqlite".to_owned());
        if !matches!(
            db_type.trim().to_ascii_lowercase().as_str(),
            "sqlite" | "sqlite3"
        ) {
            return Err(DbError::UnsupportedDatabase(db_type));
        }

        let path = std::env::var("KOMARI_DB_FILE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DB_FILE.to_owned());

        Ok(Self::new(path))
    }
}

/// A thread-safe SQLite handle shared by all repositories.
pub struct Db {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
    read_only: bool,
    in_memory: bool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.path)
            .field("read_only", &self.read_only)
            .field("in_memory", &self.in_memory)
            .finish()
    }
}

impl Db {
    /// Open (or create) a writable SQLite database with default options.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(DbOptions::new(path.as_ref()))
    }

    /// Open a fresh in-memory database. Tests use this; it is also handy for
    /// dry runs before persisting to disk.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Mutex::new(conn),
            path: None,
            read_only: false,
            in_memory: true,
        };
        db.configure_connection(false, false)?;
        Ok(db)
    }

    /// Open an existing database without write access.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(DbOptions::read_only(path.as_ref()))
    }

    /// Open a database using the `KOMARI_DB_*` environment variables.
    pub fn open_default() -> Result<Self> {
        Self::open_with(DbOptions::from_env()?)
    }

    /// Open a database with explicit options.
    pub fn open_with(options: DbOptions) -> Result<Self> {
        if options.path.as_os_str() == ":memory:" {
            return Self::open_in_memory();
        }

        if !options.read_only && options.create_parent_dirs {
            if let Some(parent) = options.path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }
        }

        let flags = if options.read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        };

        let conn = Connection::open_with_flags(&options.path, flags)?;
        let db = Self {
            conn: Mutex::new(conn),
            path: Some(options.path),
            read_only: options.read_only,
            in_memory: false,
        };
        db.configure_connection(options.wal && !options.read_only, options.foreign_keys)?;
        db.conn.lock().busy_timeout(options.busy_timeout)?;
        Ok(db)
    }

    fn configure_connection(&self, wal: bool, foreign_keys: bool) -> Result<()> {
        let conn = self.conn.lock();
        if foreign_keys {
            conn.pragma_update(None, "foreign_keys", "ON")?;
        }
        if wal {
            // Failure to switch journal modes is not fatal (read-only media,
            // network filesystems); the database still works in rollback mode.
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
        }
        Ok(())
    }

    /// The on-disk path, or `None` for an in-memory database.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether this handle was opened read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Whether this handle points at an in-memory database.
    pub fn is_in_memory(&self) -> bool {
        self.in_memory
    }

    /// Borrow the connection for a closure. Repositories use this to keep all
    /// access serialized behind the internal mutex.
    pub fn with_connection<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.conn.lock();
        f(&conn)
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    /// Execute a SQL batch. Intended for migrations and maintenance tasks.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        if self.read_only {
            return Err(DbError::ReadOnly);
        }
        self.conn.lock().execute_batch(sql)?;
        Ok(())
    }

    /// Check whether a table or view exists.
    pub fn table_exists(&self, name: &str) -> Result<bool> {
        self.with_connection(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
                [name],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
    }

    /// List every user table in the database (excluding SQLite internals).
    pub fn table_names(&self) -> Result<Vec<String>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(names)
        })
    }

    /// Run `PRAGMA integrity_check`; returns the SQLite report lines.
    pub fn integrity_check(&self) -> Result<Vec<String>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare("PRAGMA integrity_check")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Run `VACUUM`, mirroring the Go startup maintenance step.
    pub fn vacuum(&self) -> Result<()> {
        if self.read_only {
            return Err(DbError::ReadOnly);
        }
        self.conn.lock().execute_batch("VACUUM")?;
        Ok(())
    }
}

impl Default for Db {
    fn default() -> Self {
        Self::open_in_memory().expect("in-memory database must open")
    }
}
