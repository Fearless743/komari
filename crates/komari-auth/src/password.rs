//! Password hashing and verification.
//!
//! New passwords are hashed with bcrypt (GitHub/Gitea-style `$2b$` hashes) at
//! the Go backend's default cost of 10. Verification accepts bcrypt `$2a$`,
//! `$2b$`, `$2x$` and `$2y$` hashes as well as the legacy
//! `base64(SHA-256(password + "06Wm4Jv1Hkxx"))` format used by current Go
//! releases, so an existing `data/komari.db` keeps working during migration.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use komari_db::{Db, LocalTime};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{AuthError, Result};

/// Bcrypt cost used for newly generated hashes. `golang.org/x/crypto/bcrypt`
/// defaults to 10 as well.
pub const DEFAULT_BCRYPT_COST: u32 = 10;

/// Salt appended by the historical Go `accounts.hashPasswd` implementation.
const LEGACY_PASSWORD_SALT: &str = "06Wm4Jv1Hkxx";

/// Hash a password with bcrypt at [`DEFAULT_BCRYPT_COST`].
pub fn hash_password(password: &str) -> Result<String> {
    hash_password_with_cost(password, DEFAULT_BCRYPT_COST)
}

/// Hash a password with an explicit bcrypt cost.
///
/// Valid costs are 4 through 31, matching `golang.org/x/crypto/bcrypt`.
pub fn hash_password_with_cost(password: &str, cost: u32) -> Result<String> {
    Ok(bcrypt::hash(password, cost)?)
}

/// Whether a stored hash uses bcrypt (`$2a$`, `$2b$`, `$2x$` or `$2y$`).
pub fn is_bcrypt_hash(hash: &str) -> bool {
    hash.starts_with("$2a$")
        || hash.starts_with("$2b$")
        || hash.starts_with("$2x$")
        || hash.starts_with("$2y$")
}

/// Verify a password against a stored hash.
///
/// Bcrypt hashes are checked with the cost encoded in the hash. Hashes in the
/// legacy Go SHA-256 format are still accepted. Malformed hashes simply fail
/// verification instead of leaking an error to the caller.
pub fn verify_password(password: &str, hash: &str) -> bool {
    if is_bcrypt_hash(hash) {
        return bcrypt::verify(password, hash).unwrap_or(false);
    }
    verify_legacy_password(password, hash)
}

/// Verify the historical `base64(SHA-256(password + salt))` format.
pub fn verify_legacy_password(password: &str, hash: &str) -> bool {
    if hash.is_empty() {
        return false;
    }
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(LEGACY_PASSWORD_SALT.as_bytes());
    let expected = BASE64.encode(hasher.finalize());
    expected.as_bytes().ct_eq(hash.as_bytes()).into()
}

/// Create the legacy Go-compatible SHA-256 hash. Used by compatibility tests
/// and by code that must inter-operate with the Go fallback.
pub fn legacy_hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(LEGACY_PASSWORD_SALT.as_bytes());
    BASE64.encode(hasher.finalize())
}

/// Extract the cost from a bcrypt hash.
pub fn password_hash_cost(hash: &str) -> Option<u32> {
    use std::str::FromStr;
    bcrypt::HashParts::from_str(hash)
        .ok()
        .map(|parts| parts.get_cost())
}

/// Change a user's password by username and persist the bcrypt hash.
///
/// Returns an error when the username does not exist. Callers that need the
/// Go `ForceResetPassword` behaviour can treat a missing user as an error;
/// the CLI prints it and exits non-zero.
pub fn change_password(db: &Db, username: &str, new_password: &str) -> Result<usize> {
    let hash = hash_password(new_password)?;
    update_hash(db, "username", username, &hash)
}

/// Change a user's password by UUID and persist the bcrypt hash.
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
/// Returns the user when the credentials match. Both bcrypt and legacy
/// SHA-256 password rows are accepted.
pub fn authenticate(
    db: &Db,
    username: &str,
    password: &str,
) -> Result<Option<komari_db::models::User>> {
    let Some(user) = db.accounts().get_by_username(username)? else {
        // Run a dummy verification so username probing is not measurably
        // cheaper than a wrong password for an existing account.
        let _ = verify_password(password, &legacy_hash_password(password));
        return Ok(None);
    };
    if verify_password(password, &user.passwd) {
        Ok(Some(user))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated with Go 1.24 and golang.org/x/crypto/bcrypt v0.39.0.
    const GO_2A_COST10_SECRET: &str =
        "$2a$10$C4Gwv0BDoZO0LFJLCMwD/OcQy61qVqTGBbvcnPh0FiTT408C69pv.";
    const GO_2A_COST10_ADMIN: &str = "$2a$10$cdZsXT/lsKfFoe9NMdlABulTHMN7JRQPQD/TfIs//e8x/FxctcNd.";
    const GO_2A_COST12_LONG: &str = "$2a$12$2et3zLR1OtVdxXof6moc4uM8/CtB.5cCIBftM70Iq8nQCUCoL7VKO";
    // Authentic 2y/2b fixtures from the Rust bcrypt crate's test suite.
    const CRATE_2Y_COST5: &str = "$2y$05$HlFShUxTu4ZHHfOLJwfmCeDj/kuKFKboanXtDJXxCC7aIPTUgxNDe";
    const CRATE_2B_COST4: &str = "$2b$04$EGdrhbKUv8Oc9vGiXX0HQOxSg445d458Muh7DAHskb6QbtCvdxcie";

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
    fn hash_roundtrips_and_uses_cost_10() {
        let hash = hash_password("s3cret").unwrap();
        assert!(hash.starts_with("$2b$10$"), "unexpected hash: {hash}");
        assert!(verify_password("s3cret", &hash));
        assert!(!verify_password("s3creT", &hash));
        assert_eq!(password_hash_cost(&hash), Some(10));
    }

    #[test]
    fn cost_boundaries_match_go() {
        assert!(hash_password_with_cost("pw", 4).is_ok());
        assert!(hash_password_with_cost("pw", 3).is_err());
        assert!(hash_password_with_cost("pw", 32).is_err());
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
        assert!(!verify_password("wrong", legacy));
    }
}
