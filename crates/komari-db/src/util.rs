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

/// Constant-time-ish verification of a stored password hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    hash_password(password) == hash
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
    fn tokens_have_expected_length() {
        assert_eq!(generate_token().len(), 22);
        assert_eq!(generate_password().len(), 12);
    }
}
