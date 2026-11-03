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
    use komari_db::DbOptions;
    use std::collections::HashMap;

    let values: HashMap<&str, &str> = HashMap::from([
        ("KOMARI_DB_TYPE", "sqlite"),
        ("KOMARI_DB_FILE", "/tmp/x.db"),
    ]);
    let options =
        DbOptions::from_env_with(|key| values.get(key).map(|value| (*value).to_owned())).unwrap();
    assert_eq!(options.path, std::path::PathBuf::from("/tmp/x.db"));
    assert!(!options.read_only);

    let options = DbOptions::from_env_with(|_| None).unwrap();
    assert_eq!(
        options.path,
        std::path::PathBuf::from(komari_db::DEFAULT_DB_FILE)
    );

    let error =
        DbOptions::from_env_with(|key| (key == "KOMARI_DB_TYPE").then(|| "mysql".to_owned()))
            .unwrap_err();
    assert!(matches!(error, komari_db::DbError::UnsupportedDatabase(_)));

    // Creating an explicit database still works in a temp directory.
    let dir = TempDir::new("options");
    let db = Db::open_with(DbOptions::new(dir.join("env.db"))).unwrap();
    db.migrate().unwrap();
    assert!(db.table_exists("clients").unwrap());
}
