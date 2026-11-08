use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use komari_db::Db;
use komari_db::models::User;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDb {
    dir: PathBuf,
    path: PathBuf,
}

impl TempDb {
    fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "komari-cli-auth-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("komari.db");
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        Self { dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn missing_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn open(&self) -> Db {
        Db::open(&self.path).unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_komari"));
    for key in [
        "KOMARI_LISTEN",
        "KOMARI_DB_TYPE",
        "KOMARI_DB_FILE",
        "KOMARI_DB_HOST",
        "KOMARI_DB_PORT",
        "KOMARI_DB_USER",
        "KOMARI_DB_PASS",
        "KOMARI_DB_NAME",
        "KOMARI_LOG_LEVEL",
        "KOMARI_THEME",
        "KOMARI_ALLOW_CORS",
        "KOMARI_ENABLE_CLOUDFLARED",
        "KOMARI_CLOUDFLARED_TOKEN",
        "KOMARI_WS_DISABLE_ORIGIN",
    ] {
        command.env_remove(key);
    }
    command
}

fn run_path(args: &[&str], path: &Path) -> Output {
    bin()
        .args(args)
        .args(["--database", path.to_str().unwrap()])
        .output()
        .expect("run komari")
}

fn run(args: &[&str], db: &TempDb) -> Output {
    run_path(args, db.path())
}

#[test]
fn chpasswd_writes_bcrypt_hash_and_revokes_sessions() {
    let temp = TempDb::new("chpasswd");
    let uuid = {
        let db = temp.open();
        let user = db.accounts().create("admin", "old-password").unwrap();
        db.sessions()
            .create(&user.uuid, 3600, "agent", "127.0.0.1", "password")
            .unwrap();
        user.uuid
    };

    let output = run(&["chpasswd", "--password", "new-password"], &temp);
    assert!(
        output.status.success(),
        "chpasswd failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Changing password for user: admin"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Password changed successfully for user: admin"),
        "{stdout}"
    );
    assert!(!stdout.contains("new-password"), "{stdout}");
    assert!(stdout.contains("Please restart the server"), "{stdout}");

    let db = temp.open();
    let user = db.accounts().require_by_uuid(&uuid).unwrap();
    assert!(komari_auth::is_bcrypt_hash(&user.passwd));
    assert!(komari_auth::verify_password("new-password", &user.passwd));
    assert_eq!(db.sessions().count().unwrap(), 0);
}

#[test]
fn chpasswd_accepts_an_explicit_username() {
    let temp = TempDb::new("chpasswd-user");
    let bob = {
        let db = temp.open();
        db.accounts().create("admin", "admin-password").unwrap();
        db.accounts().create("bob", "bob-password").unwrap()
    };

    let output = run(
        &["chpasswd", "--username", "bob", "--password", "bob-new"],
        &temp,
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Changing password for user: bob"),
        "{stdout}"
    );

    let db = temp.open();
    let admin = db.accounts().require_by_username("admin").unwrap();
    let bob = db.accounts().require_by_uuid(&bob.uuid).unwrap();
    assert!(komari_auth::is_bcrypt_hash(&admin.passwd));
    assert!(komari_auth::is_bcrypt_hash(&bob.passwd));
    assert!(komari_auth::verify_password("bob-new", &bob.passwd));
}

#[test]
fn chpasswd_defaults_to_the_lowest_uuid_not_insertion_order() {
    let temp = TempDb::new("chpasswd-default");
    {
        let db = temp.open();
        let mut inserted_first = User::new("first", "first-password");
        inserted_first.uuid = "ffffffff-ffff-4fff-8fff-ffffffffffff".to_owned();
        db.accounts().insert(&inserted_first).unwrap();

        let mut inserted_second = User::new("second", "second-password");
        inserted_second.uuid = "11111111-1111-4111-8111-111111111111".to_owned();
        db.accounts().insert(&inserted_second).unwrap();
    }

    let output = run(&["chpasswd", "--password", "changed"], &temp);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Changing password for user: second"),
        "{stdout}"
    );

    let db = temp.open();
    let first = db.accounts().require_by_username("first").unwrap();
    let second = db.accounts().require_by_username("second").unwrap();
    assert!(komari_auth::verify_password(
        "first-password",
        &first.passwd
    ));
    assert!(komari_auth::verify_password("changed", &second.passwd));
    assert!(komari_auth::is_bcrypt_hash(&second.passwd));
}

#[test]
fn chpasswd_with_no_users_matches_go_exit_and_message() {
    let temp = TempDb::new("chpasswd-empty");
    let output = run(&["chpasswd", "--password", "whatever"], &temp);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Changing password for user:"), "{stdout}");
    assert!(stdout.contains("Error: 无法找到用户名"), "{stdout}");
}

#[test]
fn chpasswd_missing_or_empty_password_prints_help_and_exits_zero() {
    let temp = TempDb::new("chpasswd-help");
    for args in [vec!["chpasswd"], vec!["chpasswd", "--password", ""]] {
        let output = run(&args, &temp);
        assert!(output.status.success(), "{args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage:"), "{args:?}:\n{stdout}");
    }
}

#[test]
fn disable_2fa_clears_the_secret_for_one_user() {
    let temp = TempDb::new("disable-2fa");
    let (alice, bob) = {
        let db = temp.open();
        let alice = db.accounts().create("alice", "password").unwrap();
        let bob = db.accounts().create("bob", "password").unwrap();
        let secret = komari_auth::generate_secret();
        db.accounts().enable_2fa(&alice.uuid, &secret).unwrap();
        db.accounts().enable_2fa(&bob.uuid, &secret).unwrap();
        (alice, bob)
    };

    let output = run(&["disable-2fa", "--username", "alice"], &temp);
    assert!(
        output.status.success(),
        "disable-2fa failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("2FA has been disabled."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let db = temp.open();
    assert!(
        db.accounts()
            .require_by_uuid(&alice.uuid)
            .unwrap()
            .two_factor
            .is_empty()
    );
    assert!(
        !db.accounts()
            .require_by_uuid(&bob.uuid)
            .unwrap()
            .two_factor
            .is_empty()
    );
}

#[test]
fn disable_2fa_without_username_clears_every_secret() {
    let temp = TempDb::new("disable-2fa-all");
    {
        let db = temp.open();
        let user = db.accounts().create("alice", "password").unwrap();
        db.accounts()
            .enable_2fa(&user.uuid, &komari_auth::generate_secret())
            .unwrap();
    }

    let output = run(&["disable-2fa"], &temp);
    assert!(output.status.success());

    let db = temp.open();
    let user = db.accounts().require_by_username("alice").unwrap();
    assert!(user.two_factor.is_empty());
}

#[test]
fn disable_2fa_creates_and_migrates_a_missing_database() {
    let temp = TempDb::new("disable-2fa-create");
    let path = temp.missing_path("created-2fa.db");
    assert!(!path.exists());

    let output = run_path(&["disable-2fa"], &path);
    assert!(
        output.status.success(),
        "disable-2fa failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.exists());

    let db = Db::open(&path).unwrap();
    assert!(db.table_exists("users").unwrap());
    assert!(db.table_exists("configs").unwrap());
}

#[test]
fn permit_login_enables_password_authentication() {
    let temp = TempDb::new("permit-login");
    {
        let db = temp.open();
        db.config().set("disable_password_login", &true).unwrap();
    }

    let output = run(&["permit-login"], &temp);
    assert!(
        output.status.success(),
        "permit-login failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Password login has been permitted."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let db = temp.open();
    let disabled = db
        .config()
        .get_as::<bool>("disable_password_login")
        .unwrap()
        .unwrap();
    assert!(!disabled);
}

#[test]
fn permit_login_creates_and_migrates_a_missing_database() {
    let temp = TempDb::new("permit-login-create");
    let path = temp.missing_path("created-login.db");
    assert!(!path.exists());

    let output = run_path(&["permit-login"], &path);
    assert!(
        output.status.success(),
        "permit-login failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.exists());

    let db = Db::open(&path).unwrap();
    assert!(db.table_exists("configs").unwrap());
    let disabled = db
        .config()
        .get_as::<bool>("disable_password_login")
        .unwrap()
        .unwrap();
    assert!(!disabled);
}

#[test]
fn chpasswd_migrates_an_existing_empty_database() {
    let temp = TempDb::new("chpasswd-empty-file");
    let path = temp.missing_path("empty.db");
    std::fs::File::create(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);

    let output = run_path(&["chpasswd", "--password", "whatever"], &path);
    assert!(
        output.status.success(),
        "chpasswd failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Changing password for user:"), "{stdout}");
    assert!(stdout.contains("Error: 无法找到用户名"), "{stdout}");

    let db = Db::open(&path).unwrap();
    assert!(db.table_exists("users").unwrap());
    assert!(db.table_exists("sessions").unwrap());
}

#[test]
fn chpasswd_migrates_a_database_missing_the_sessions_table() {
    let temp = TempDb::new("chpasswd-no-sessions");
    let path = temp.missing_path("no-sessions.db");
    {
        let db = Db::open(&path).unwrap();
        db.migrate().unwrap();
        db.accounts().create("admin", "old-password").unwrap();
        db.execute_batch("DROP TABLE sessions;").unwrap();
        assert!(!db.table_exists("sessions").unwrap());
    }

    let output = run_path(&["chpasswd", "--password", "new-password"], &path);
    assert!(
        output.status.success(),
        "chpasswd failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Unable to force logout"), "{stdout}");
    assert!(stdout.contains("Password changed successfully"), "{stdout}");

    let db = Db::open(&path).unwrap();
    assert!(db.table_exists("sessions").unwrap());
    let user = db.accounts().require_by_username("admin").unwrap();
    assert!(komari_auth::is_bcrypt_hash(&user.passwd));
    assert!(komari_auth::verify_password("new-password", &user.passwd));
}

#[test]
fn chpasswd_missing_database_matches_the_go_message() {
    let temp = TempDb::new("missing");
    let missing = temp.missing_path("missing.db");
    let output = run_path(&["chpasswd", "--password", "whatever"], &missing);
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Database file does not exist."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!missing.exists());
}
