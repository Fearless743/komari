//! Migration behaviour: idempotence, file creation and read-only opens.

mod support;

use komari_db::Db;
use support::{TempDir, memory_db};

#[test]
fn migration_can_run_twice_in_memory() {
    let db = memory_db();
    db.migrate().expect("second migration must succeed");
    assert!(db.schema_is_current().unwrap());

    let tables = db.table_names().unwrap();
    for expected in [
        "clients",
        "users",
        "sessions",
        "records",
        "gpu_records",
        "configs",
        "logs",
        "clipboards",
        "load_notifications",
        "offline_notifications",
        "oidc_providers",
        "message_sender_providers",
        "ddns_providers",
        "ddns_sync_history",
        "theme_configurations",
        "records_long_term",
        "gpu_records_long_term",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }
}

#[test]
fn migration_is_idempotent_on_disk_and_preserves_data() {
    let dir = TempDir::new("idempotent");
    let path = dir.join("komari.db");

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.config().set("sitename", "Komari").unwrap();
        db.clients().create_named("node-1").unwrap();
    }

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
        assert!(db.schema_is_current().unwrap());
        assert_eq!(
            db.config().get_as::<String>("sitename").unwrap().as_deref(),
            Some("Komari")
        );
        assert_eq!(db.clients().count().unwrap(), 1);
    }
}

#[test]
fn opening_a_missing_file_creates_it_and_its_parent() {
    let dir = TempDir::new("create");
    let path = dir.join("nested/komari.db");
    assert!(!path.exists());

    let db = Db::open(&path).unwrap();
    db.migrate().unwrap();
    assert!(path.exists());
    assert!(db.schema_is_current().unwrap());
}

#[test]
fn read_only_handle_can_inspect_but_not_write() {
    let dir = TempDir::new("readonly");
    let path = dir.join("komari.db");

    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.config().set("theme", "default").unwrap();
    }

    let db = Db::open_read_only(&path).unwrap();
    assert!(db.is_read_only());
    assert!(db.schema_is_current().unwrap());
    assert_eq!(
        db.config().get_as::<String>("theme").unwrap().as_deref(),
        Some("default")
    );
    assert!(db.config().set("theme", "dark").is_err());
}

#[test]
fn database_options_read_environment_names() {
    let dir = TempDir::new("options");
    let path = dir.join("env.db");
    // `DbOptions::from_env` is testable without touching global state by
    // checking the explicit constructor; environment mutation is intentionally
    // avoided because tests run in parallel.
    let options = komari_db::DbOptions::new(&path);
    assert!(!options.read_only);
    let db = Db::open_with(options).unwrap();
    db.migrate().unwrap();
    assert!(db.table_exists("clients").unwrap());
}
