//! Password hashing and verification.
//!
//! **Writes always use the Go-compatible format**:
//! `base64(SHA-256(password + "06Wm4Jv1Hkxx"))`, exactly like the Go
//! `accounts.hashPasswd`. That guarantees a password changed by the Rust CLI
//! can still be verified by the Go fallback binary.
//!
//! Verification additionally accepts bcrypt `$2a$`, `$2b$`, `$2x$` and `$2y$`
//! rows for read-only compatibility with databases written by older Rust
//! builds. When such a row authenticates successfully,
//! [`authenticate`] immediately rewrites it to the Go SHA-256 format.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use komari_db::{Db, LocalTime};
use sha2::{Digest, Sha256};

use crate::error::{AuthError, Result};

/// Bcrypt cost used by the read-only compatibility helpers.
pub const DEFAULT_BCRYPT_COST: u32 = 10;

/// Salt appended by the Go `accounts.hashPasswd` implementation.
const LEGACY_PASSWORD_SALT: &str = "06Wm4Jv1Hkxx";

/// Hash a password in the Go-compatible format.
///
/// This is the only hash format written by this crate.
pub fn hash_password(password: &str) -> Result<String> {
    Ok(komari_db::util::hash_password(password))
}

/// Hash a password with bcrypt.
///
/// Only for compatibility tests/fixtures; never use this when writing a
/// password row that the Go fallback must read.
pub fn hash_bcrypt_password(password: &str) -> Result<String> {
    hash_bcrypt_password_with_cost(password, DEFAULT_BCRYPT_COST)
}

/// Bcrypt hash with an explicit cost (4 through 31), for test fixtures.
pub fn hash_bcrypt_password_with_cost(password: &str, cost: u32) -> Result<String> {
    Ok(bcrypt::hash(password, cost)?)
}

/// Whether a stored hash uses bcrypt.
pub fn is_bcrypt_hash(hash: &str) -> bool {
    hash.starts_with("$2a$")
        || hash.starts_with("$2b$")
        || hash.starts_with("$2x$")
        || hash.starts_with("$2y$")
}

/// Verify a password against a stored hash.
///
/// Bcrypt rows are accepted for compatibility, legacy Go SHA-256 rows are the
/// normal path. Malformed hashes fail closed.
pub fn verify_password(password: &str, hash: &str) -> bool {
    if is_bcrypt_hash(hash) {
        return bcrypt::verify(password, hash).unwrap_or(false);
    }
    verify_legacy_password(password, hash)
}

/// Verify only the Go `base64(SHA-256(password + salt))` format.
///
/// This deliberately does not accept bcrypt; use [`verify_password`] for the
/// read-only compatibility union.
pub fn verify_legacy_password(password: &str, hash: &str) -> bool {
    komari_db::util::verify_sha256_password(password, hash)
}

/// Create the Go-compatible SHA-256 hash.
pub fn legacy_hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(LEGACY_PASSWORD_SALT.as_bytes());
    BASE64.encode(hasher.finalize())
}

/// Extract the cost from a bcrypt hash (read-only compatibility helper).
pub fn password_hash_cost(hash: &str) -> Option<u32> {
    use std::str::FromStr;
    bcrypt::HashParts::from_str(hash)
        .ok()
        .map(|parts| parts.get_cost())
}

/// Change a user's password by username and persist the Go SHA-256 hash.
pub fn change_password(db: &Db, username: &str, new_password: &str) -> Result<usize> {
    let hash = hash_password(new_password)?;
    update_hash(db, "username", username, &hash)
}

/// Change a user's password by UUID and persist the Go SHA-256 hash.
pub fn change_password_by_uuid(db: &Db, uuid: &str, new_password: &str) -> Result<usize> {
    let hash = hash_password(new_password)?;
    update_hash(db, "uuid", uuid, &hash)
}

fn update_hash(db: &Db, column: &str, value: &str, hash: &str) -> Result<usize> {
    debug_assert!(matches!(column, "username" | "uuid"));
    let now = LocalTime::now();
    let sql = format!("UPDATE users SET passwd = ?1, updated_at = ?2 WHERE {column} = ?3");
    let changed = db.with_connection(|conn| {
        let changed = conn.execute(&sql, (hash, now, value))?;
        Ok::<_, komari_db::DbError>(changed)
    })?;
    if changed == 0 {
        return Err(AuthError::UserNotFound(value.to_owned()));
    }
    Ok(changed)
}

/// Look up a user by username and verify the supplied password.
///
/// Successful bcrypt authentication transparently rewrites the row to the Go
/// SHA-256 format, so the Go fallback can log in again after a downgrade.
pub fn authenticate(
    db: &Db,
    username: &str,
    password: &str,
) -> Result<Option<komari_db::models::User>> {
    let Some(mut user) = db.accounts().get_by_username(username)? else {
        // Run a verification so username probing is not measurably cheaper.
        let _ = verify_password(password, &legacy_hash_password(password));
        return Ok(None);
    };
    if !verify_password(password, &user.passwd) {
        return Ok(None);
    }

    if is_bcrypt_hash(&user.passwd) {
        let upgraded = hash_password(password)?;
        update_hash(db, "uuid", &user.uuid, &upgraded)?;
        user.passwd = upgraded;
    }
    Ok(Some(user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komari_db::models::User;

    // Generated with Go 1.24 and golang.org/x/crypto/bcrypt v0.39.0.
    const GO_2A_COST10_SECRET: &str =
        "$2a$10$C4Gwv0BDoZO0LFJLCMwD/OcQy61qVqTGBbvcnPh0FiTT408C69pv.";
    const GO_2A_COST10_ADMIN: &str = "$2a$10$cdZsXT/lsKfFoe9NMdlABulTHMN7JRQPQD/TfIs//e8x/FxctcNd.";
    const GO_2A_COST12_LONG: &str = "$2a$12$2et3zLR1OtVdxXof6moc4uM8/CtB.5cCIBftM70Iq8nQCUCoL7VKO";
    // Authentic 2y/2b fixtures from the Rust bcrypt crate's test suite.
    const CRATE_2Y_COST5: &str = "$2y$05$HlFShUxTu4ZHHfOLJwfmCeDj/kuKFKboanXtDJXxCC7aIPTUgxNDe";
    const CRATE_2B_COST4: &str = "$2b$04$EGdrhbKUv8Oc9vGiXX0HQOxSg445d458Muh7DAHskb6QbtCvdxcie";

    fn database() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    #[test]
    fn verifies_go_generated_bcrypt_hashes() {
        assert!(verify_password("secret", GO_2A_COST10_SECRET));
        assert!(verify_password("admin", GO_2A_COST10_ADMIN));
        assert!(verify_password(
            "correct horse battery staple",
            GO_2A_COST12_LONG
        ));
        assert!(!verify_password("Secret", GO_2A_COST10_SECRET));
        assert!(!verify_password("", GO_2A_COST10_SECRET));
    }

    #[test]
    fn verifies_2y_and_2b_hashes() {
        assert!(verify_password("My S3cre7 P@55w0rd!", CRATE_2Y_COST5));
        assert!(verify_password(
            "correctbatteryhorsestapler",
            CRATE_2B_COST4
        ));
        assert_eq!(password_hash_cost(CRATE_2Y_COST5), Some(5));
        assert_eq!(password_hash_cost(CRATE_2B_COST4), Some(4));
    }

    #[test]
    fn hash_password_writes_go_sha256_not_bcrypt() {
        let hash = hash_password("s3cret").unwrap();
        assert_eq!(hash, legacy_hash_password("s3cret"));
        assert_eq!(hash, "afNoIJvN1YwBajO9sRUt1zuHqOjGfIxEe9qKAgOKT0Y=");
        assert!(!is_bcrypt_hash(&hash));
        assert!(verify_password("s3cret", &hash));
        assert!(!verify_password("s3creT", &hash));
    }

    #[test]
    fn bcrypt_helper_respects_cost_boundaries() {
        let hash = hash_bcrypt_password_with_cost("pw", 4).unwrap();
        assert!(hash.starts_with("$2b$04$"));
        assert!(hash_bcrypt_password_with_cost("pw", 3).is_err());
        assert!(hash_bcrypt_password_with_cost("pw", 32).is_err());
    }

    #[test]
    fn rejects_malformed_hashes() {
        assert!(!verify_password("pw", ""));
        assert!(!verify_password("pw", "not-a-hash"));
        assert!(!verify_password("pw", "$2b$10$too-short"));
        assert!(!is_bcrypt_hash(
            "NwK0chtAiNgPs5S3fCOjr2s6C61k8x+k/s9Y0PesSp4="
        ));
    }

    #[test]
    fn legacy_go_sha256_hashes_still_verify() {
        // `echo -n 'admin06Wm4Jv1Hkxx' | sha256sum | base64`
        let legacy = "NwK0chtAiNgPs5S3fCOjr2s6C61k8x+k/s9Y0PesSp4=";
        assert_eq!(legacy_hash_password("admin"), legacy);
        assert!(verify_password("admin", legacy));
        assert!(verify_legacy_password("admin", legacy));
        assert!(!verify_password("wrong", legacy));
        // The legacy-only helper must not accept bcrypt rows.
        assert!(!verify_legacy_password("secret", GO_2A_COST10_SECRET));
    }

    #[test]
    fn change_password_writes_the_go_hash() {
        let db = database();
        let user = db.accounts().create("alice", "old").unwrap();
        change_password(&db, "alice", "new-password").unwrap();
        let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
        assert_eq!(
            stored.passwd,
            "yqyhBswdm4vh/vRYv8vY9m3+t+ZQPxLQLv0FlN6FArI="
        );
        assert!(!is_bcrypt_hash(&stored.passwd));
    }

    #[test]
    fn bcrypt_row_is_upgraded_during_login() {
        let db = database();
        let mut user = User::new("legacy-rust", "whatever");
        user.passwd = GO_2A_COST10_SECRET.to_owned();
        db.accounts().insert(&user).unwrap();

        let logged_in = authenticate(&db, "legacy-rust", "secret").unwrap().unwrap();
        assert_eq!(logged_in.passwd, legacy_hash_password("secret"));
        let stored = db.accounts().require_by_uuid(&user.uuid).unwrap();
        assert_eq!(
            stored.passwd,
            "LKWZkh0XkW+IwPl4Zv7eHJmKmv2WvTk34iBzq0oefQM="
        );
        assert!(
            authenticate(&db, "legacy-rust", "secret")
                .unwrap()
                .is_some()
        );
        assert!(authenticate(&db, "legacy-rust", "wrong").unwrap().is_none());
    }
}
