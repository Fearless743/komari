//! Time-based one-time passwords (TOTP).
//!
//! The parameters match `github.com/pquerna/otp/totp` as used by the Go
//! backend: SHA-1, six digits, a 30-second period and a validation skew of one
//! period in each direction. Secrets are 20 random bytes encoded as
//! unpadded, uppercase base32 (32 characters), exactly like
//! `totp.Generate`.

use std::time::{SystemTime, UNIX_EPOCH};

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use komari_db::Db;
use rand::RngCore;
use sha1::Sha1;
use subtle::ConstantTimeEq;

use crate::error::{AuthError, Result};

type HmacSha1 = Hmac<Sha1>;

/// Issuer shown by authenticator apps (Go `accounts.TwoFactorIssuer`).
pub const TOTP_ISSUER: &str = "Komari Monitor";
/// Account name embedded in the otpauth URL.
pub const TOTP_ACCOUNT_NAME: &str = "komari";
/// TOTP period in seconds.
pub const TOTP_PERIOD: u64 = 30;
/// Number of digits in a code.
pub const TOTP_DIGITS: u32 = 6;
/// Accepted clock drift, in periods.
pub const TOTP_SKEW: i64 = 1;

/// A freshly generated 2FA enrolment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TwoFactorSetup {
    /// Base32 secret to store in `users.two_factor`.
    pub secret: String,
    /// `otpauth://` URL for a QR code / manual entry.
    pub otpauth_url: String,
    pub issuer: String,
    pub account_name: String,
}

/// Generate a new 20-byte base32 secret.
pub fn generate_secret() -> String {
    let mut secret = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut secret);
    BASE32_NOPAD.encode(&secret)
}

/// Build the `otpauth://totp/...` URL in the same form as pquerna/otp.
pub fn otpauth_url(secret: &str, issuer: &str, account_name: &str) -> String {
    // Go's `internal.EncodeQuery` sorts keys and uses %20 for spaces. Values
    // here are simple text (issuer/account), so a small encoder matches it.
    format!(
        "otpauth://totp/{}:{}?algorithm=SHA1&digits={}&issuer={}&period={}&secret={}",
        percent_encode(issuer),
        percent_encode(account_name),
        TOTP_DIGITS,
        percent_encode(issuer),
        TOTP_PERIOD,
        percent_encode(secret)
    )
}

/// Generate a secret and its enrolment URL.
pub fn generate_2fa() -> Result<TwoFactorSetup> {
    let secret = generate_secret();
    if secret.is_empty() {
        return Err(AuthError::InvalidTotpSecret(
            "random secret generation failed".to_owned(),
        ));
    }
    Ok(TwoFactorSetup {
        otpauth_url: otpauth_url(&secret, TOTP_ISSUER, TOTP_ACCOUNT_NAME),
        secret,
        issuer: TOTP_ISSUER.to_owned(),
        account_name: TOTP_ACCOUNT_NAME.to_owned(),
    })
}

/// Generate a six-digit code for the current time.
pub fn generate_code_at(secret: &str, unix_time: i64) -> Result<String> {
    generate_code_at_with(secret, unix_time, TOTP_DIGITS, TOTP_PERIOD)
}

/// Generate a code with explicit digits/period (used by RFC 6238 tests).
pub fn generate_code_at_with(
    secret: &str,
    unix_time: i64,
    digits: u32,
    period: u64,
) -> Result<String> {
    if digits == 0 || digits > 10 {
        return Err(AuthError::InvalidTotpSecret(format!(
            "unsupported digit count {digits}"
        )));
    }
    if period == 0 {
        return Err(AuthError::InvalidTotpSecret(
            "period must be greater than zero".to_owned(),
        ));
    }
    let counter = counter_at(unix_time, period);
    generate_code_for_counter(secret, counter, digits)
}

fn generate_code_for_counter(secret: &str, counter: u64, digits: u32) -> Result<String> {
    let key = decode_secret(secret)?;
    let mut mac = HmacSha1::new_from_slice(&key)
        .map_err(|error| AuthError::InvalidTotpSecret(error.to_string()))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let value = (((digest[offset] & 0x7f) as u32) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | (digest[offset + 3] as u32);
    let modulo = 10u32.pow(digits);
    Ok(format!(
        "{:0width$}",
        value % modulo,
        width = digits as usize
    ))
}

/// Verify a six-digit code against the current time, allowing ±1 period.
pub fn validate_code(secret: &str, code: &str) -> bool {
    verify_code_at(secret, code, now_unix()).unwrap_or(false)
}

/// Verify a six-digit code at a specific Unix timestamp, allowing ±1 period.
pub fn verify_code_at(secret: &str, code: &str, unix_time: i64) -> Result<bool> {
    verify_code_at_with(secret, code, unix_time, TOTP_DIGITS, TOTP_PERIOD, TOTP_SKEW)
}

/// Verify a code with explicit parameters.
pub fn verify_code_at_with(
    secret: &str,
    code: &str,
    unix_time: i64,
    digits: u32,
    period: u64,
    skew: i64,
) -> Result<bool> {
    let code = code.trim();
    if code.len() != digits as usize || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(false);
    }

    // Reject malformed secrets even when the code length is wrong, so
    // configuration errors are visible to callers that care.
    let _ = decode_secret(secret)?;

    let counter = counter_at(unix_time, period);
    let mut candidates = Vec::with_capacity((skew * 2 + 1) as usize);
    candidates.push(counter);
    for offset in 1..=skew.max(0) {
        candidates.push(counter.saturating_add(offset as u64));
        candidates.push(counter.saturating_sub(offset as u64));
    }

    for candidate in candidates {
        let expected = generate_code_for_counter(secret, candidate, digits)?;
        if expected.as_bytes().ct_eq(code.as_bytes()).into() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Store an enabled 2FA secret for a user.
pub fn enable_2fa(db: &Db, user_uuid: &str, secret: &str) -> Result<usize> {
    decode_secret(secret)?;
    Ok(db.accounts().enable_2fa(user_uuid, secret)?)
}

/// Validate `code` and, when it matches, store the 2FA secret.
pub fn enable_2fa_with_code(db: &Db, user_uuid: &str, secret: &str, code: &str) -> Result<bool> {
    if !validate_code(secret, code) {
        return Ok(false);
    }
    enable_2fa(db, user_uuid, secret)?;
    Ok(true)
}

/// Disable 2FA for one user.
pub fn disable_2fa(db: &Db, user_uuid: &str) -> Result<usize> {
    Ok(db.accounts().disable_2fa(user_uuid)?)
}

/// Disable 2FA for every user (Go `cmd/disable2FA.go`).
pub fn disable_all_2fa(db: &Db) -> Result<usize> {
    db.with_connection(|conn| {
        let changed = conn.execute(
            "UPDATE users SET two_factor = '', updated_at = ?1 \
             WHERE two_factor IS NOT NULL AND two_factor != ''",
            (komari_db::LocalTime::now(),),
        )?;
        Ok::<_, komari_db::DbError>(changed)
    })
    .map_err(AuthError::from)
}

/// Verify a user's current TOTP code. Returns `false` when 2FA is not enabled,
/// matching Go `accounts.Verify2Fa`.
pub fn verify_2fa(db: &Db, user_uuid: &str, code: &str) -> Result<bool> {
    let user = db.accounts().require_by_uuid(user_uuid)?;
    if user.two_factor.is_empty() {
        return Ok(false);
    }
    verify_code_at(&user.two_factor, code, now_unix())
}

fn counter_at(unix_time: i64, period: u64) -> u64 {
    if unix_time <= 0 {
        return 0;
    }
    (unix_time as u64) / period
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn decode_secret(secret: &str) -> Result<Vec<u8>> {
    let normalized: String = secret
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '=')
        .collect::<String>()
        .to_ascii_uppercase();
    if normalized.is_empty() {
        return Err(AuthError::InvalidTotpSecret(secret.to_owned()));
    }
    BASE32_NOPAD
        .decode(normalized.as_bytes())
        .map_err(|error| AuthError::InvalidTotpSecret(format!("{secret:?}: {error}")))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b':'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => encoded.push(byte as char),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    const RFC_SECRET_8: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    fn database_with_user() -> (Db, String) {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let user = db.accounts().create("alice", "password").unwrap();
        (db, user.uuid)
    }

    #[test]
    fn matches_rfc_6238_sha1_vectors() {
        // RFC 6238 Appendix B, SHA-1, eight digits, secret "12345678901234567890".
        let vectors = [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ];
        for (timestamp, expected) in vectors {
            assert_eq!(
                generate_code_at_with(RFC_SECRET_8, timestamp, 8, 30).unwrap(),
                expected,
                "wrong code at {timestamp}"
            );
        }
    }

    #[test]
    fn accepts_one_period_of_clock_skew() {
        let timestamp = 1_111_111_110; // exactly on a 30-second boundary
        let code = generate_code_at(RFC_SECRET, timestamp).unwrap();
        assert!(verify_code_at(RFC_SECRET, &code, timestamp).unwrap());
        assert!(verify_code_at(RFC_SECRET, &code, timestamp + 29).unwrap());
        assert!(verify_code_at(RFC_SECRET, &code, timestamp + 30).unwrap());
        assert!(verify_code_at(RFC_SECRET, &code, timestamp - 30).unwrap());
        assert!(!verify_code_at(RFC_SECRET, &code, timestamp + 60).unwrap());
        assert!(!verify_code_at(RFC_SECRET, &code, timestamp - 60).unwrap());
    }

    #[test]
    fn rejects_wrong_or_malformed_codes() {
        let timestamp = 1_111_111_110;
        assert!(!verify_code_at(RFC_SECRET, "000000", timestamp).unwrap());
        assert!(!verify_code_at(RFC_SECRET, "12345", timestamp).unwrap());
        assert!(!verify_code_at(RFC_SECRET, "abcdef", timestamp).unwrap());
        assert!(!verify_code_at(RFC_SECRET, "", timestamp).unwrap());
        assert!(verify_code_at("not base32!", "123456", timestamp).is_err());
    }

    #[test]
    fn generates_pquerna_compatible_secret_and_url() {
        let setup = generate_2fa().unwrap();
        assert_eq!(setup.secret.len(), 32);
        assert_eq!(decode_secret(&setup.secret).unwrap().len(), 20);
        assert_eq!(
            otpauth_url(
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
                TOTP_ISSUER,
                TOTP_ACCOUNT_NAME
            ),
            "otpauth://totp/Komari%20Monitor:komari?algorithm=SHA1&digits=6&issuer=Komari%20Monitor&period=30&secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
        assert!(setup.otpauth_url.contains(&setup.secret));
    }

    #[test]
    fn enables_disables_and_verifies_user_2fa() {
        let (db, uuid) = database_with_user();
        let secret = generate_secret();
        let code = generate_code_at(&secret, now_unix()).unwrap();

        assert!(!verify_2fa(&db, &uuid, &code).unwrap());
        assert!(!enable_2fa_with_code(&db, &uuid, &secret, "000000").unwrap());
        assert!(enable_2fa_with_code(&db, &uuid, &secret, &code).unwrap());
        assert_eq!(
            db.accounts().require_by_uuid(&uuid).unwrap().two_factor,
            secret
        );
        assert!(verify_2fa(&db, &uuid, &code).unwrap());

        assert_eq!(disable_2fa(&db, &uuid).unwrap(), 1);
        assert!(
            db.accounts()
                .require_by_uuid(&uuid)
                .unwrap()
                .two_factor
                .is_empty()
        );
        assert!(!verify_2fa(&db, &uuid, &code).unwrap());
    }

    #[test]
    fn disables_2fa_for_all_users() {
        let (db, first) = database_with_user();
        let second = db.accounts().create("bob", "password").unwrap();
        db.accounts()
            .enable_2fa(&first, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
            .unwrap();
        db.accounts()
            .enable_2fa(&second.uuid, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
            .unwrap();
        assert_eq!(disable_all_2fa(&db).unwrap(), 2);
        assert_eq!(db.accounts().count().unwrap(), 2);
        assert!(
            db.accounts()
                .require_by_uuid(&first)
                .unwrap()
                .two_factor
                .is_empty()
        );
        assert!(
            db.accounts()
                .require_by_uuid(&second.uuid)
                .unwrap()
                .two_factor
                .is_empty()
        );
    }
}
