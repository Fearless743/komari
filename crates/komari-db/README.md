# komari-db

SQLite persistence layer for the Rust rewrite of Komari.

The crate is intentionally **schema-compatible with the existing Go backend**:
it reads and writes the exact tables, columns, indexes and value encodings
created by GORM, so a running Komari deployment can point the Rust binary at its
current `data/komari.db` without a migration or export/import step.

## Opening a database

```rust
use komari_db::Db;

# fn main() -> komari_db::Result<()> {
// Same defaults as the Go backend: KOMARI_DB_TYPE (sqlite) and
// KOMARI_DB_FILE (./data/komari.db), creating parent directories and the
// database file when missing.
let db = Db::open_default()?;
db.migrate()?; // idempotent: CREATE TABLE/INDEX IF NOT EXISTS only
# Ok(())
# }
```

Explicit constructors:

| Constructor | Behaviour |
| --- | --- |
| `Db::open(path)` | read/write, creates the file and parent directories |
| `Db::open_with(DbOptions)` | full control over path, WAL, timeout, read-only |
| `Db::open_in_memory()` | tests and dry runs |
| `Db::open_read_only(path)` | inspection of an existing database |
| `Db::open_default()` | `KOMARI_DB_*` environment variables |

`DbOptions` also exposes `foreign_keys`: SQLite's default is disabled and the
Go backend never enables it, so it defaults to `false` for behavioural parity.
WAL journaling is enabled for read/write handles, like the Go backend.

`DbOptions::from_env()` implements the Go flag precedence for the database
variables. MySQL is not implemented yet and returns
`DbError::UnsupportedDatabase`.

## Schema

`migrate()` creates the tables from `database/models/**` in the Go backend and
adds missing columns to older tables with `ALTER TABLE ... ADD COLUMN` (never
dropping or rewriting existing data). Missing unique constraints are restored
with unique indexes when the existing data has no duplicates (empty
client tokens/user names/session ids are first backfilled with random values),
and the legacy single-row `configs` schema (including hybrid tables that already
received empty `key`/`value` columns from an older additive migration) is
converted into key/value form with the original table kept as
`configs_legacy_backup`.

### Historical migrations (Go `MergeDatabase` parity)

Very old databases are upgraded in the same transaction as the schema repair,
before the single-row `configs` table is converted:

| Era | Migration |
| --- | --- |
| pre-0.0.5 | `client_infos` is merged into `clients` by UUID. The static fields from Go `common.ClientInfo` are copied, unknown UUIDs are skipped, and the old table is renamed to `client_infos_backup` (created only once). |
| pre-1.0.2 OIDC | When `oidc_providers` does not exist, `o_auth_client_id` / `o_auth_client_secret` become the `github` provider with `addition = {"client_id":...,"client_secret":...}`. Legacy single-row `configs` are forced to `o_auth_provider = "github"`, matching Go. |
| pre-1.0.2 notifications | When `message_sender_providers` does not exist, `notification_method = "telegram"` / `"email"` creates the corresponding provider. Telegram keeps Go's `bot_token`, `chat_id`, `endpoint` shape (default endpoint `https://api.telegram.org/bot`); email keeps `host`, `port`, `username`, `password`, `sender`, `receiver`, `use_ssl`. |

The moved OAuth/notification columns are not copied into the key/value
`configs` table, matching Go's current `config.Legacy` model; they remain
available in `configs_legacy_backup` for manual recovery. Every step checks for
the target table/backup first, so re-running `migrate()` is a no-op. Any error
rolls the whole transaction back, leaving the original database untouched.

`clients`, `users`, `sessions`, `records`, `gpu_records`, `logs`, `clipboards`,
`offline_notifications`, `load_notifications`, `oidc_providers`,
`message_sender_providers`, `ddns_providers`, `ddns_sync_history`,
`theme_configurations`, `configs`, `records_long_term`,
`gpu_records_long_term`.

The DDL was copied from the schema GORM generated in a real Komari database
(including quirks such as `clients."group"`, `ddns_sync_history.ip_v4` /
`ip_v6`, `sessions.expires datetime NOT NULL`, the extra
`idx_sessions_session` unique index and the `uni_clipboards_id` constraint).
`tests/compat.rs` compares `PRAGMA table_info` and `PRAGMA index_list` against
`../komari/data/komari.db` when that file exists.

Tables owned by other Go packages (`command_audits`, `ping_tasks`,
`ping_records`, `tasks`, `task_results`) are left untouched.

## Repositories

All repositories are cheap borrows created from a `Db`:

```rust
# use komari_db::{Db, models::Client};
# fn main() -> komari_db::Result<()> {
# let db = Db::open_in_memory()?;
# db.migrate()?;
let client = db.clients().create_named("web-1")?;
db.clients().get_by_token(&client.token)?;
db.config().set("sitename", "Komari")?;
let sitename: Option<String> = db.config().get_as("sitename")?;
# Ok(())
# }
```

| Accessor | Table(s) | Highlights |
| --- | --- | --- |
| `clients()` | `clients` | create/get by UUID or token, patch, rename, delete, list |
| `accounts()` | `users` | salted SHA-256 password check/change, SSO binding, 2FA |
| `sessions()` | `sessions` | create/validate, expiry cleanup, latest activity |
| `records()` | `records*`, `gpu_records*` | batch insert, combined live+long-term ranges, latest, aggregate, cleanup |
| `config()` | `configs` | raw/typed `Get`/`Set`, `GetAs<T>`, persisted defaults |
| `notifications()` | `offline_notifications`, `load_notifications` | CRUD for both rule types |
| `message_senders()` | `message_sender_providers` | provider JSON CRUD |
| `oauth()` | `oidc_providers` | provider JSON CRUD |
| `ddns_providers()` | `ddns_providers` | provider JSON CRUD |
| `ddns()` | `ddns_sync_history` | save/list/cleanup history |
| `themes()` | `theme_configurations` | JSON configuration per theme |
| `clipboard()` | `clipboards` | CRUD and batch delete |
| `logs()` | `logs` | audit log insert, `time DESC` listing with limit/offset pagination and retention |

Errors are defined by `DbError` (`thiserror`); lookups return
`Option<T>` and `require*` helpers produce `DbError::NotFound`.

`notifications().load_set_last_notified(id, at)` is a narrow cross-crate helper
added for `komari-notify` (B07) to mirror Go's `updateLastNotified`, which runs
`UPDATE load_notifications SET last_notified = ?`. It updates only that column,
does not change the schema, and leaves the existing B02 `load_update`/`load_save`
interfaces untouched.

## Backup export and import

`komari-db` can produce and consume the same `backup.zip` layout as the Go
backend's admin download/upload endpoints, without depending on HTTP:

```rust
use komari_db::{BackupOptions, Db, import_backup_file};

# fn main() -> komari_db::Result<()> {
let db = Db::open("data/komari.db")?;
db.migrate()?;

// Stream into memory, or write atomically to a path.
let options = BackupOptions::new("data");
let bytes = db.export_backup(&options)?;
db.export_backup_to(&options, "backup-2026-11-04.zip")?;

// Restore into a data directory (typically on the next process start).
let report = import_backup_file(&BackupOptions::new("data"), "backup-2026-11-04.zip")?;
println!("restored {} data files", report.data_entries);
# let _ = bytes;
# Ok(())
# }
```

| Function | Purpose |
| --- | --- |
| `Db::export_backup(&options)` | return the archive as `Vec<u8>` |
| `Db::export_backup_to(&options, path)` | write to a temporary file and rename into place |
| `import_backup(&options, reader)` | restore from any `Read + Seek` source |
| `import_backup_bytes(&options, bytes)` | restore from memory |
| `import_backup_file(&options, path)` | restore from a ZIP file |
| `restore_pending_backup(&options)` | restore `<data_dir>/backup.zip` when present (Go startup behaviour) |

`export_backup_to` refuses to write over the live database, its `-wal`/`-shm`
sidecars or the configured database path, including through symlinked
directories or symlinked destination files; use a separate archive path.
`restore_pending_backup` is the helper a server startup task can call before
opening the database: it checks `max_archive_bytes` from metadata, copies the
pending ZIP to a sibling temporary file outside `data_dir`, streams the import
from disk, and removes the restored `backup.zip` (even when the archive itself
contained a `backup.zip` entry). Wiring it into a concrete server lifecycle is
left to the startup/integration task.

`BackupOptions` defaults match the Go backend: `data_dir = "./data"`,
database at `<data_dir>/komari.db`, marker required. Limits default to 10 000
entries, 512 MiB per entry, 2 GiB total uncompressed and 2 GiB for the archive
itself, with a 200:1 compression-ratio guard. `include_data = false` produces a
database-only archive; `allow_legacy_schema = true` accepts databases that
predate the current table set so `Db::migrate()` can add the missing tables
afterwards. Set `pre_restore_backup_dir` to opt into the Go backend's
`./backup/{timestamp}.zip` pre-restore archive; it is best effort and disabled
by default so a library call never writes outside the configured data
directory unless asked. The pre-restore directory must be **outside**
`data_dir`; an inside path is ignored to avoid recursive/self-including
archives. The pre-restore snapshot intentionally keeps transient files and
ignores `BackupOptions::exclude`, matching Go's pre-restore safety copy; it only
excludes the database sidecars, the marker and `backup.zip`.

### Archive layout (Go parity)

| Entry | Content |
| --- | --- |
| `komari.db` | consistent SQLite snapshot created with `VACUUM INTO` (committed WAL content included) |
| `<relative path>` | every file below `data_dir`, excluding SQLite files and transient files |
| `komari-backup-markup` | marker file at the archive root, same text as Go, with an RFC 3339 second-precision timestamp (`Z` for UTC, `±HH:MM` otherwise) and no trailing newline |

The export walk skips `*.db`, `*.db-wal`, `*.db-shm`, `backup.zip`, the marker,
the destination archive itself, common temporary file suffixes (`.tmp`,
`.temp`, `.part`, `.swp`, `.swo`, `~`, `.~`, `.komari-backup-*`) and symbolic
links. The temporary-file rules are intentionally broader than Go and apply
only to files: a directory with such a name is still walked. Likewise, a
directory named like a database file is not stored as an entry but its children
are still archived, matching Go's `Walk` behaviour. Non-UTF-8 file names are
skipped with a warning instead of failing the whole export (the ZIP writer API
is UTF-8 only); a non-UTF-8 directory name skips its entire subtree and the
warning says so explicitly.

### Import safety

The archive is inspected before anything is written:

1. ZIP Slip protection rejects absolute paths, `..`, backslashes, NUL bytes and
   symlink entries.
2. Entry count, per-entry size, total uncompressed size, archive size and
   compression ratio are enforced.
3. The marker and `komari.db` must exist at the root (the marker can be
   disabled with `require_markup(false)`).
4. The database is opened read-only and must pass `PRAGMA integrity_check`;
   missing tables are rejected unless legacy schemas are explicitly allowed.
5. Files are extracted into a sibling staging directory, then the whole data
   directory is replaced. On Linux the swap uses the
   `SYS_renameat2` syscall with `RENAME_EXCHANGE`, so there is no moment where
   `data_dir` is missing and no glibc 2.28 wrapper dependency. Other platforms
   (or older kernels/filesystems without exchange support; `ENOSYS` falls
   back) use two renames; if the process crashes between them, the previous data
   is at a `.komari-backup-previous-*` sibling and can be moved back manually. A failure at any point removes the staging directory and leaves
   the existing data untouched. If `pre_restore_backup_dir` is set, a
   best-effort snapshot of the whole old data directory is written there
   first, matching the Go startup restore (even when the restore itself was
   configured with `include_data = false`).

Imports into the same data directory are serialized: a concurrent attempt
returns `DbError::BackupInProgress`. Different data directories can be
restored in parallel. Errors are explicit variants of `DbError`
(`InvalidBackup`, `UnsafeBackupPath`, `BackupLimit`, `BackupIntegrity`,
`BackupSchema`, `BackupInProgress`, `Zip`, `Io`).

## Time and JSON compatibility

`LocalTime` mirrors `models/UTCTime.go` exactly:

- persisted as `YYYY-MM-DD HH:MM:SS.fffffff` (seven digits, application
  timezone, no offset), zero values as SQL `NULL`;
- parsing accepts every layout the Go code accepts, including driver-written
  values such as `2026-06-23 01:01:32.488977903+00:00`; the wall clock is kept
  (Go's `Scan(time.Time)` does the same);
- JSON is `null` for the zero value and RFC 3339 in the application timezone
  otherwise;
- `TZ` selects the application timezone (unset/empty means UTC, matching Go).
- `LocalTime::from_instant` converts absolute date-times into the application
  timezone (the equivalent of Go `FromTime` + `.In(GetAppLocation())`);
  `LocalTime::now_and_after`/`absolute_after` return absolute `DateTime<Utc>`
  pairs (a DST-fold pair still differs by exactly the requested duration), and
  `Session::expires_instant` keeps the absolute expiry for in-process expiry
  checks. The database stores wall clocks only, so the two sides of an autumn
  fold format identically — the same limitation as Go's `LocalTime`.

`StringArray` stores a JSON array of strings, matching `models.StringArray`.
Its zero value serializes as `null` (Go's nil slice) and both `null` and SQL
`NULL` are read back as an empty array, so rows written by Go or other tools
remain readable.

## Tests

```sh
cargo test -p komari-db
cargo clippy -p komari-db --all-targets -- -D warnings
```

`tests/compat.rs` looks for
`/home/fearless/github/komari/komari/data/komari.db`; override it with
`KOMARI_COMPAT_DB=/path/to/komari.db`. When the file is absent the test is
skipped, but when present it asserts every table/column/index matches and reads
real configuration values.

## Known limitations

- Only SQLite is implemented; the Go MySQL path is not ported.
- Backup import replaces the whole data directory and therefore requires the
  database to live at `<data_dir>/komari.db` (the Go layout). A
  `database_path` pointing elsewhere is rejected instead of performing a
  non-atomic two-location restore.
- An open `Db` handle keeps the old database inode after an import; reopen the
  handle (or restart the process) before serving requests, as the Go backend
  does on startup.
- Backup symlinks are skipped on export and rejected on import. Encrypted ZIP
  entries are rejected (the `zip` parser may surface them as `DbError::Zip`
  before the per-entry check runs). Files added after a backup was taken are
  removed by a restore, matching the Go restore semantics. The Go
  `./backup/{timestamp}.zip` pre-restore archive is available through
  `pre_restore_backup_dir` but is not enabled by default.
- Non-UTF-8 file names cannot be represented by the ZIP writer API and are
  skipped with a warning during export.
- `restore_pending_backup` implements the Go pending-`backup.zip` restore
  helper, but no server crate calls it yet; startup wiring belongs to the
  server integration task.
- The Linux atomic directory exchange falls back to two renames on other
  platforms. A crash between those renames leaves the old data in a
  `.komari-backup-previous-*` sibling for manual recovery.
- Schema compatibility is checked by table names; the Go backend has no
  explicit schema version field, so no numeric version is stored in the
  archive.
- The percentile-based record compaction (`records` -> `records_long_term`) is
  not ported yet; the tables and queries are available, so callers can read
  long-term data written by the Go backend.
- The pre-0.0.5 `client_infos` merge and the pre-1.0.2 OAuth/notification
  migrations are performed automatically in the same transaction as the rest
  of `migrate()`. `client_infos_backup` and `configs_legacy_backup` are kept for
  recovery; remove them manually after verifying the migrated data.
- The old OAuth/notification credential columns are deliberately not copied
  into the new key/value `configs` table. They remain in
  `configs_legacy_backup` (if that table was created) so an operator can
  recover them; this is the only place the legacy secrets survive.
- `ConfigRepo::get_as<T>` applies the same fallbacks as Go `config.GetAs`:
  `null` becomes the type's zero value, integers/floats are converted between
  numeric target types, and base64 strings decode to `Vec<u8>` using
  `base64.StdEncoding` semantics (CR/LF ignored, non-zero trailing bits
  accepted). Scalar integer targets follow Go's reflect semantics: floats
  truncate toward zero, narrow integers wrap modulo 2^bits (so `300.0` read as
  `i8` is `44`), and negative values converted to unsigned targets wrap (so
  `-1.0` read as `u64` is `u64::MAX`). Out-of-range float-to-integer
  conversions use Go's amd64 results (`i64::MIN` for signed, `2^63` for
  unsigned) instead of returning an error. Compound targets reproduce Go's
  generic remarshal fallback, so integral floats such as `80.0` populate
  struct/slice/map integer fields, while `80.5` and out-of-range narrow
  integers still fail.
