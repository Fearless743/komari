//! End-to-end checks across the public `komari-auth` API.

use std::time::{SystemTime, UNIX_EPOCH};

use komari_auth::{
    authenticate, change_password, change_password_by_uuid, create_session, delete_session,
    disable_2fa, enable_2fa, generate_code_at, hash_password, is_bcrypt_hash, legacy_hash_password,
    validate_session, verify_2fa, verify_password,
};
use komari_db::Db;
use komari_db::models::User;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn password_changes_are_readable_by_the_go_backend() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let user = db.accounts().create("admin", "old-password").unwrap();

    assert!(
        authenticate(&db, "admin", "old-password")
            .unwrap()
            .is_some()
    );
    change_password(&db, "admin", "new-password").unwrap();

    let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
    // Independent Go algorithm: base64(SHA-256(password + "06Wm4Jv1Hkxx")).
    assert_eq!(
        stored.passwd,
        "yqyhBswdm4vh/vRYv8vY9m3+t+ZQPxLQLv0FlN6FArI="
    );
    assert_eq!(stored.passwd, legacy_hash_password("new-password"));
    assert!(!is_bcrypt_hash(&stored.passwd));
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

    change_password_by_uuid(&db, &user.uuid, "another").unwrap();
    let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
    assert_eq!(stored.passwd, legacy_hash_password("another"));

    assert!(change_password(&db, "nobody", "pw").is_err());
    assert!(change_password_by_uuid(&db, "missing-uuid", "pw").is_err());
    assert_eq!(
        hash_password("admin").unwrap(),
        legacy_hash_password("admin")
    );
}

#[test]
fn bcrypt_rows_are_readable_and_upgraded_to_go_hashes() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let mut user = User::new("legacy", "ignored");
    // Bcrypt hash for "secret", as written by an earlier Rust build.
    user.passwd = "$2a$10$C4Gwv0BDoZO0LFJLCMwD/OcQy61qVqTGBbvcnPh0FiTT408C69pv.".to_owned();
    db.accounts().insert(&user).unwrap();

    let logged_in = authenticate(&db, "legacy", "secret").unwrap().unwrap();
    assert_eq!(logged_in.passwd, legacy_hash_password("secret"));
    let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
    assert_eq!(stored.passwd, legacy_hash_password("secret"));
    assert!(!is_bcrypt_hash(&stored.passwd));
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
