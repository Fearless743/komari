//! Random tokens and password hashing, matching the Go helpers.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::Rng;
use rand::distributions::Alphanumeric;
use sha2::{Digest, Sha256};

/// The constant salt used by the Go backend for password hashing.
pub const PASSWORD_SALT: &str = "06Wm4Jv1Hkxx";

/// Port of `utils.GenerateRandomString` (62-character alphanumeric alphabet).
pub fn generate_random_string(length: usize) -> String {
    if length == 0 {
        return String::new();
    }
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

/// Port of `utils.GenerateToken` (22 random characters).
pub fn generate_token() -> String {
    generate_random_string(22)
}

/// Port of `utils.GeneratePassword` (12 random characters).
pub fn generate_password() -> String {
    generate_random_string(12)
}

/// Port of `accounts.hashPasswd`: base64(SHA-256(password + constantSalt)).
pub fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(PASSWORD_SALT.as_bytes());
    BASE64.encode(hasher.finalize())
}

/// Whether a stored hash is bcrypt (`$2a$`, `$2b$`, `$2x$`, `$2y$`).
///
/// Komari only writes the Go SHA-256 format. Bcrypt is accepted here for
/// read-only compatibility with databases written by older Rust builds; the
/// auth layer upgrades such rows to SHA-256 on successful login.
pub fn is_bcrypt_hash(hash: &str) -> bool {
    hash.starts_with("$2a$")
        || hash.starts_with("$2b$")
        || hash.starts_with("$2x$")
        || hash.starts_with("$2y$")
}

/// Verify only the Go `base64(SHA-256(password + salt))` format.
///
/// Unlike [`verify_password`] this never accepts bcrypt, so callers can enforce
/// the exact Go fallback contract.
pub fn verify_sha256_password(password: &str, hash: &str) -> bool {
    !hash.is_empty() && hash_password(password) == hash
}

/// Verify a stored password hash.
///
/// Writes always use [`hash_password`] (SHA-256); bcrypt rows are accepted for
/// read compatibility so `Accounts::check_password` and `User::check_password`
/// do not reject credentials written by an older Rust version.
pub fn verify_password(password: &str, hash: &str) -> bool {
    if is_bcrypt_hash(hash) {
        return bcrypt::verify(password, hash).unwrap_or(false);
    }
    verify_sha256_password(password, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_go_implementation() {
        // `echo -n 'admin06Wm4Jv1Hkxx' | sha256sum | base64`
        assert_eq!(
            hash_password("admin"),
            "NwK0chtAiNgPs5S3fCOjr2s6C61k8x+k/s9Y0PesSp4="
        );
    }

    #[test]
    fn bcrypt_hashes_are_read_only_compatible() {
        // Bcrypt hash of "secret", as written by an older Rust build.
        let hash = "$2a$10$C4Gwv0BDoZO0LFJLCMwD/OcQy61qVqTGBbvcnPh0FiTT408C69pv.";
        assert!(verify_password("secret", hash));
        assert!(!verify_password("wrong", hash));
        assert!(!verify_sha256_password("secret", hash));
        assert!(is_bcrypt_hash(hash));
        assert!(!is_bcrypt_hash(&hash_password("secret")));
        // Writing still produces the Go SHA-256 format.
        assert_eq!(
            hash_password("secret"),
            "LKWZkh0XkW+IwPl4Zv7eHJmKmv2WvTk34iBzq0oefQM="
        );
    }

    #[test]
    fn tokens_have_expected_length() {
        assert_eq!(generate_token().len(), 22);
        assert_eq!(generate_password().len(), 12);
    }
}
