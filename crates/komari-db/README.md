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

`DbOptions::from_env()` implements the Go flag precedence for the database
variables. MySQL is not implemented yet and returns
`DbError::UnsupportedDatabase`.

## Schema

`migrate()` creates the tables from `database/models/**` in the Go backend:

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
| `records()` | `records*`, `gpu_records*` | batch insert, ranges, latest, aggregate, cleanup |
| `config()` | `configs` | raw/typed `Get`/`Set`, `GetAs<T>`, persisted defaults |
| `notifications()` | `offline_notifications`, `load_notifications` | CRUD for both rule types |
| `message_senders()` | `message_sender_providers` | provider JSON CRUD |
| `oauth()` | `oidc_providers` | provider JSON CRUD |
| `ddns_providers()` | `ddns_providers` | provider JSON CRUD |
| `ddns()` | `ddns_sync_history` | save/list/cleanup history |
| `themes()` | `theme_configurations` | JSON configuration per theme |
| `clipboard()` | `clipboards` | CRUD and batch delete |
| `logs()` | `logs` | audit log insert/list/retention |

Errors are defined by `DbError` (`thiserror`); lookups return
`Option<T>` and `require*` helpers produce `DbError::NotFound`.

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

`StringArray` stores a JSON array of strings, matching `models.StringArray`.

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
- The percentile-based record compaction (`records` -> `records_long_term`) is
  not ported yet; the tables and queries are available, so callers can read
  long-term data written by the Go backend.
- Legacy pre-1.1.4 single-row `configs` tables and the legacy `client_infos`
  merge are not converted automatically. The current key/value `configs`
  schema is supported; the legacy `Config` model is provided for reference.
- `ConfigRepo::get_as<T>` uses serde's conversion rules. Values that need
  Go-style lossy numeric coercion (for example an integer stored as a JSON
  float) should be decoded as `f64` or converted by the caller.
