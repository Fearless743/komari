use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use komari_auth::verify_password;
use komari_db::Db;

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

fn run(args: &[&str], db: &TempDb) -> Output {
    bin()
        .args(args)
        .args(["--database", db.path().to_str().unwrap()])
        .output()
        .expect("run komari")
}

#[test]
fn chpasswd_updates_the_password_and_revokes_sessions() {
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
        stdout.contains("Password changed successfully, new password: new-password"),
        "{stdout}"
    );
    assert!(stdout.contains("Please restart the server"), "{stdout}");

    let db = temp.open();
    let user = db.accounts().require_by_uuid(&uuid).unwrap();
    assert!(verify_password("new-password", &user.passwd));
    assert!(!verify_password("old-password", &user.passwd));
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
    assert!(verify_password("admin-password", &admin.passwd));
    assert!(verify_password("bob-new", &bob.passwd));
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
fn missing_database_matches_the_go_message() {
    let temp = TempDb::new("missing");
    let missing = temp.path().with_file_name("missing.db");
    let output = bin()
        .args(["chpasswd", "--password", "whatever"])
        .args(["--database", missing.to_str().unwrap()])
        .output()
        .expect("run komari");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Database file does not exist."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
