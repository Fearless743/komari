//! End-to-end checks across the public `komari-auth` API.

use std::time::{SystemTime, UNIX_EPOCH};

use komari_auth::{
    authenticate, change_password, change_password_by_uuid, create_session, delete_session,
    disable_2fa, enable_2fa, generate_code_at, hash_password, is_bcrypt_hash, validate_session,
    verify_2fa, verify_password,
};
use komari_db::Db;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn legacy_account_can_log_in_and_migrates_to_bcrypt() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    // `Accounts::create` uses the historical Go SHA-256 format.
    let user = db.accounts().create("admin", "old-password").unwrap();

    let logged_in = authenticate(&db, "admin", "old-password").unwrap().unwrap();
    assert_eq!(logged_in.uuid, user.uuid);

    change_password(&db, "admin", "new-password").unwrap();
    let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
    assert!(is_bcrypt_hash(&stored.passwd));
    assert!(verify_password("new-password", &stored.passwd));
    assert!(
        authenticate(&db, "admin", "old-password")
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate(&db, "admin", "new-password")
            .unwrap()
            .is_some()
    );
}

#[test]
fn password_change_by_uuid_reports_unknown_users() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let user = db.accounts().create("alice", "password").unwrap();

    let bcrypt = hash_password("another").unwrap();
    change_password_by_uuid(&db, &user.uuid, "another").unwrap();
    assert!(verify_password(
        "another",
        &db.accounts().require_by_uuid(&user.uuid).unwrap().passwd
    ));

    assert!(change_password(&db, "nobody", "pw").is_err());
    assert!(change_password_by_uuid(&db, "missing-uuid", "pw").is_err());
    assert!(bcrypt.starts_with("$2b$"));
}

#[test]
fn sessions_and_2fa_round_trip() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let user = db.accounts().create("alice", "password").unwrap();

    let token = create_session(
        &db,
        &user.uuid,
        "komari-agent/1.0",
        "203.0.113.9",
        "password",
    )
    .unwrap();
    let validated = validate_session(&db, &token).unwrap().unwrap();
    assert_eq!(validated.user.uuid, user.uuid);
    assert_eq!(validated.username(), "alice");
    assert!(delete_session(&db, &token).unwrap());
    assert!(validate_session(&db, &token).unwrap().is_none());

    let secret = komari_auth::generate_secret();
    let code = generate_code_at(&secret, now_unix()).unwrap();
    enable_2fa(&db, &user.uuid, &secret).unwrap();
    assert!(verify_2fa(&db, &user.uuid, &code).unwrap());
    disable_2fa(&db, &user.uuid).unwrap();
    assert!(!verify_2fa(&db, &user.uuid, &code).unwrap());
}
