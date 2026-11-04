//! Compact JWT/JWKS verification with the same asymmetric algorithms as
//! `github.com/coreos/go-oidc` / `go-jose`.
//!
//! Komari only needs to verify signed identity tokens from OpenID Connect
//! providers and Cloudflare Access. This module implements the small,
//! well-defined subset:
//!
//! * compact JWS (`header.payload.signature`),
//! * `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`,
//!   `ES256`, `ES384`, `ES512` and `EdDSA` (Ed25519),
//! * RSA, EC (`P-256`, `P-384`, `P-521`) and OKP (`Ed25519`) keys published as
//!   JWKS,
//! * `iss`, `aud`, `exp` and `nbf` validation.
//!
//! RSA, P-256/P-384 ECDSA and Ed25519 verification use `ring`; P-521 (needed
//! for `ES512`) uses the RustCrypto `p521` crate. `alg = none`, unknown
//! algorithms and tokens whose algorithm does not match the selected key type
//! are always rejected.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use parking_lot::{Mutex, RwLock};
use ring::signature;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{OAuthError, Result, redact_secrets};

/// A parsed JWT header.
#[derive(Clone, Debug, Deserialize)]
pub struct JwtHeader {
    pub alg: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub typ: Option<String>,
}

/// The standard claims used by the OAuth crate.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct JwtClaims {
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default, deserialize_with = "deserialize_audience")]
    pub aud: Option<Vec<String>>,
    #[serde(default)]
    pub exp: Option<i64>,
    #[serde(default)]
    pub nbf: Option<i64>,
    #[serde(default)]
    pub iat: Option<i64>,
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A verified token.
#[derive(Clone, Debug)]
pub struct VerifiedJwt {
    pub header: JwtHeader,
    pub claims: JwtClaims,
    pub payload: Value,
}

/// One public key from a JWKS document.
///
/// RSA keys use `n`/`e`; EC keys use `crv`/`x`/`y`; OKP (Ed25519) keys use
/// `crv`/`x`. Fields that do not apply to the key type are ignored, matching
/// the behaviour of go-jose's `JSONWebKey`.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub key_ops: Option<Vec<String>>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

impl Jwk {
    /// Whether the JWK may be used to verify signatures.
    ///
    /// JWKS documents commonly contain encryption keys next to signing keys;
    /// `use`/`key_ops` restrict which operations are allowed.
    fn permits_verification(&self) -> bool {
        let use_allows = match self.use_.as_deref() {
            None => true,
            Some(value) => value.eq_ignore_ascii_case("sig"),
        };
        let ops_allow = match self.key_ops.as_deref() {
            None => true,
            Some(ops) => ops.iter().any(|op| op.eq_ignore_ascii_case("verify")),
        };
        use_allows && ops_allow
    }
}

/// A JWKS document.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct JwkSet {
    #[serde(default)]
    pub keys: Vec<Jwk>,
}

/// The key type required by a JOSE signature algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyKind {
    Rsa,
    EcP256,
    EcP384,
    EcP521,
    OkpEd25519,
}

impl KeyKind {
    fn matches(self, key: &Jwk) -> bool {
        match self {
            KeyKind::Rsa => key.kty.eq_ignore_ascii_case("RSA"),
            KeyKind::EcP256 => is_curve(key, "EC", "P-256"),
            KeyKind::EcP384 => is_curve(key, "EC", "P-384"),
            KeyKind::EcP521 => is_curve(key, "EC", "P-521"),
            KeyKind::OkpEd25519 => is_curve(key, "OKP", "Ed25519"),
        }
    }

    fn coordinate_len(self) -> Option<usize> {
        match self {
            KeyKind::Rsa | KeyKind::OkpEd25519 => None,
            KeyKind::EcP256 => Some(32),
            KeyKind::EcP384 => Some(48),
            KeyKind::EcP521 => Some(66),
        }
    }
}

fn is_curve(key: &Jwk, kty: &str, curve: &str) -> bool {
    key.kty.eq_ignore_ascii_case(kty)
        && key
            .crv
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(curve))
}

/// Asymmetric algorithms supported by go-oidc/go-jose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
    Es512,
    EdDsa,
}

impl Algorithm {
    fn from_jose(name: &str) -> Option<Self> {
        match name {
            "RS256" => Some(Self::Rs256),
            "RS384" => Some(Self::Rs384),
            "RS512" => Some(Self::Rs512),
            "PS256" => Some(Self::Ps256),
            "PS384" => Some(Self::Ps384),
            "PS512" => Some(Self::Ps512),
            "ES256" => Some(Self::Es256),
            "ES384" => Some(Self::Es384),
            "ES512" => Some(Self::Es512),
            "EdDSA" => Some(Self::EdDsa),
            _ => None,
        }
    }

    fn key_kind(self) -> KeyKind {
        match self {
            Self::Rs256 | Self::Rs384 | Self::Rs512 | Self::Ps256 | Self::Ps384 | Self::Ps512 => {
                KeyKind::Rsa
            }
            Self::Es256 => KeyKind::EcP256,
            Self::Es384 => KeyKind::EcP384,
            Self::Es512 => KeyKind::EcP521,
            Self::EdDsa => KeyKind::OkpEd25519,
        }
    }

    /// `ring` verification algorithm for everything except `ES512`.
    fn ring_algorithm(self) -> Option<&'static dyn signature::VerificationAlgorithm> {
        match self {
            Self::Rs256 => Some(&signature::RSA_PKCS1_2048_8192_SHA256),
            Self::Rs384 => Some(&signature::RSA_PKCS1_2048_8192_SHA384),
            Self::Rs512 => Some(&signature::RSA_PKCS1_2048_8192_SHA512),
            Self::Ps256 => Some(&signature::RSA_PSS_2048_8192_SHA256),
            Self::Ps384 => Some(&signature::RSA_PSS_2048_8192_SHA384),
            Self::Ps512 => Some(&signature::RSA_PSS_2048_8192_SHA512),
            Self::Es256 => Some(&signature::ECDSA_P256_SHA256_FIXED),
            Self::Es384 => Some(&signature::ECDSA_P384_SHA384_FIXED),
            Self::Es512 => None,
            Self::EdDsa => Some(&signature::ED25519),
        }
    }
}

impl JwkSet {
    fn candidates(&self, key_kind: KeyKind, kid: Option<&str>) -> Vec<&Jwk> {
        self.keys
            .iter()
            .filter(|key| key_kind.matches(key))
            .filter(|key| match kid {
                Some(kid) => key.kid.as_deref() == Some(kid),
                None => true,
            })
            .collect()
    }
}

/// Fetch and parse a JWKS document.
pub async fn fetch_jwks(client: &reqwest::Client, url: &str) -> Result<JwkSet> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| OAuthError::Jwks(redact_secrets(&error.to_string(), &[])))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| OAuthError::Jwks(redact_secrets(&error.to_string(), &[])))?;
    if !status.is_success() {
        return Err(OAuthError::Jwks(format!(
            "HTTP {status}: {}",
            redact_secrets(&body, &[])
        )));
    }
    serde_json::from_str(&body)
        .map_err(|error| OAuthError::Jwks(format!("{error}: {}", redact_secrets(&body, &[]))))
}

/// Verify a compact JWT against a JWKS document.
///
/// `issuer` and `audience` are checked when supplied. `exp` is mandatory (as
/// required by OpenID Connect) and `nbf` is checked when present.
pub fn verify_jwt(
    jwks: &JwkSet,
    token: &str,
    issuer: Option<&str>,
    audience: Option<&str>,
) -> Result<VerifiedJwt> {
    let mut parts = token.split('.');
    let (header_b64, payload_b64, signature_b64) =
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(header), Some(payload), Some(signature), None) => (header, payload, signature),
            _ => {
                return Err(OAuthError::InvalidToken(
                    "expected three JWT segments".to_owned(),
                ));
            }
        };

    let header_bytes = decode_base64url(header_b64)?;
    let payload_bytes = decode_base64url(payload_b64)?;
    let signature_bytes = decode_base64url(signature_b64)?;
    let header: JwtHeader = serde_json::from_slice(&header_bytes)
        .map_err(|error| OAuthError::InvalidToken(format!("invalid header: {error}")))?;
    let payload: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|error| OAuthError::InvalidToken(format!("invalid payload: {error}")))?;
    let claims: JwtClaims = serde_json::from_value(payload.clone())
        .map_err(|error| OAuthError::InvalidToken(format!("invalid claims: {error}")))?;

    if header.alg.eq_ignore_ascii_case("none") {
        return Err(OAuthError::InvalidToken(
            "unsigned JWTs (alg=none) are not accepted".to_owned(),
        ));
    }
    let algorithm = Algorithm::from_jose(&header.alg).ok_or_else(|| {
        OAuthError::InvalidToken(format!("unsupported signing algorithm {:?}", header.alg))
    })?;

    let signing_input = format!("{header_b64}.{payload_b64}");
    let candidates = jwks.candidates(algorithm.key_kind(), header.kid.as_deref());
    if candidates.is_empty() {
        return Err(OAuthError::InvalidToken(
            "no matching key in JWKS".to_owned(),
        ));
    }

    let mut verified = false;
    for key in candidates {
        if !key.permits_verification() {
            continue;
        }
        if let Some(alg) = &key.alg {
            if !alg.eq_ignore_ascii_case(&header.alg) {
                continue;
            }
        }
        if verify_signature(key, algorithm, signing_input.as_bytes(), &signature_bytes) {
            verified = true;
            break;
        }
    }
    if !verified {
        return Err(OAuthError::InvalidToken(
            "signature verification failed".to_owned(),
        ));
    }

    validate_claims(&claims, issuer, audience)?;
    Ok(VerifiedJwt {
        header,
        claims,
        payload,
    })
}

fn verify_signature(
    key: &Jwk,
    algorithm: Algorithm,
    message: &[u8],
    signature_bytes: &[u8],
) -> bool {
    if algorithm == Algorithm::Es512 {
        return verify_p521(key, message, signature_bytes);
    }
    let Some(ring_algorithm) = algorithm.ring_algorithm() else {
        return false;
    };

    let public_key = match algorithm.key_kind() {
        KeyKind::Rsa => {
            let (Some(n), Some(e)) = (key.n.as_deref(), key.e.as_deref()) else {
                return false;
            };
            let (Ok(modulus), Ok(exponent)) = (decode_base64url(n), decode_base64url(e)) else {
                return false;
            };
            rsa_public_key_der(&modulus, &exponent)
        }
        KeyKind::OkpEd25519 => {
            let Some(x) = key.x.as_deref() else {
                return false;
            };
            let Ok(public_key) = decode_base64url(x) else {
                return false;
            };
            if public_key.len() != 32 {
                return false;
            }
            public_key
        }
        kind => {
            let Some(coordinate_len) = kind.coordinate_len() else {
                return false;
            };
            let Some(public_key) = ec_public_key(key, coordinate_len) else {
                return false;
            };
            public_key
        }
    };

    signature::UnparsedPublicKey::new(ring_algorithm, public_key)
        .verify(message, signature_bytes)
        .is_ok()
}

/// Build the uncompressed SEC1 point (`0x04 || x || y`) used by `ring`.
fn ec_public_key(key: &Jwk, coordinate_len: usize) -> Option<Vec<u8>> {
    let x = decode_base64url(key.x.as_deref()?).ok()?;
    let y = decode_base64url(key.y.as_deref()?).ok()?;
    if x.len() != coordinate_len || y.len() != coordinate_len {
        return None;
    }
    let mut point = Vec::with_capacity(1 + coordinate_len * 2);
    point.push(0x04);
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    Some(point)
}

/// Verify `ES512` (ECDSA P-521 / SHA-512), which `ring` does not implement.
fn verify_p521(key: &Jwk, message: &[u8], signature_bytes: &[u8]) -> bool {
    use p521::ecdsa::signature::Verifier;
    use p521::ecdsa::{Signature, VerifyingKey};

    let Some(point) = ec_public_key(key, 66) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_sec1_bytes(&point) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(signature_bytes) else {
        return false;
    };
    verifying_key.verify(message, &signature).is_ok()
}

fn validate_claims(claims: &JwtClaims, issuer: Option<&str>, audience: Option<&str>) -> Result<()> {
    let now = Utc::now().timestamp();
    let expires = claims
        .exp
        .ok_or_else(|| OAuthError::InvalidToken("missing exp claim".to_owned()))?;
    if expires <= now - 60 {
        return Err(OAuthError::InvalidToken("token has expired".to_owned()));
    }
    if let Some(not_before) = claims.nbf {
        if not_before > now + 60 {
            return Err(OAuthError::InvalidToken(
                "token is not valid yet".to_owned(),
            ));
        }
    }
    if let Some(expected) = issuer {
        if claims.iss.as_deref() != Some(expected) {
            return Err(OAuthError::InvalidToken(format!(
                "issuer mismatch: expected {expected:?}"
            )));
        }
    }
    if let Some(expected) = audience {
        let matches = claims
            .aud
            .as_ref()
            .is_some_and(|values| values.iter().any(|value| value == expected));
        if !matches {
            return Err(OAuthError::InvalidToken(format!(
                "audience mismatch: expected {expected:?}"
            )));
        }
    }
    Ok(())
}

fn decode_base64url(value: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(value))
        .map_err(|error| OAuthError::InvalidToken(format!("invalid base64url: {error}")))
}

fn deserialize_audience<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(vec![value])),
        Some(Value::Array(values)) => {
            let mut audience = Vec::with_capacity(values.len());
            for value in values {
                if let Value::String(value) = value {
                    audience.push(value);
                }
            }
            Ok(Some(audience))
        }
        Some(_) => Err(serde::de::Error::custom(
            "aud must be a string or an array of strings",
        )),
    }
}

// --- RSA public key DER encoding -----------------------------------------

fn der_length(length: usize) -> Vec<u8> {
    if length < 0x80 {
        return vec![length as u8];
    }
    let bytes = length.to_be_bytes();
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    let significant = &bytes[first_non_zero..];
    let mut output = Vec::with_capacity(significant.len() + 1);
    output.push(0x80 | significant.len() as u8);
    output.extend_from_slice(significant);
    output
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(content.len() + 4);
    output.push(tag);
    output.extend(der_length(content.len()));
    output.extend_from_slice(content);
    output
}

fn der_integer(value: &[u8]) -> Vec<u8> {
    let mut value = value.to_vec();
    while value.len() > 1 && value[0] == 0 {
        value.remove(0);
    }
    if value.first().is_some_and(|byte| byte & 0x80 != 0) {
        value.insert(0, 0);
    }
    der_tlv(0x02, &value)
}

/// Encode `(n, e)` as a DER `RSAPublicKey`, which is what `ring` expects for
/// its `RSA_PKCS1_*` and `RSA_PSS_*` verification algorithms.
fn rsa_public_key_der(modulus: &[u8], exponent: &[u8]) -> Vec<u8> {
    let mut rsa_key = Vec::new();
    rsa_key.extend(der_integer(modulus));
    rsa_key.extend(der_integer(exponent));
    der_tlv(0x30, &rsa_key)
}

/// Minimum interval between two JWKS refreshes caused by an unknown `kid`.
const KEY_MISS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// A cached JWKS document that refreshes once when a token's `kid` is unknown.
pub struct JwksCache {
    url: String,
    client: reqwest::Client,
    keys: RwLock<Option<Arc<JwkSet>>>,
    last_key_miss_refresh: Mutex<Option<Instant>>,
}

impl JwksCache {
    pub fn new(client: reqwest::Client, url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client,
            keys: RwLock::new(None),
            last_key_miss_refresh: Mutex::new(None),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn get(&self) -> Result<Arc<JwkSet>> {
        if let Some(keys) = self.keys.read().clone() {
            return Ok(keys);
        }
        self.refresh().await
    }

    pub async fn refresh(&self) -> Result<Arc<JwkSet>> {
        let keys = Arc::new(fetch_jwks(&self.client, &self.url).await?);
        *self.keys.write() = Some(Arc::clone(&keys));
        Ok(keys)
    }

    /// Refresh after an unknown `kid`, at most once per interval.
    ///
    /// This prevents a stream of invalid tokens from turning into a JWKS
    /// request storm while still allowing a rotated key to be picked up.
    async fn refresh_after_key_miss(&self) -> Result<Arc<JwkSet>> {
        {
            let mut last = self.last_key_miss_refresh.lock();
            let now = Instant::now();
            if let Some(previous) = *last {
                if now.duration_since(previous) < KEY_MISS_REFRESH_INTERVAL {
                    if let Some(keys) = self.keys.read().clone() {
                        return Ok(keys);
                    }
                }
            }
            *last = Some(now);
        }
        self.refresh().await
    }
}

/// Verifier combining a [`JwksCache`] with expected issuer/audience values.
pub struct JwtVerifier {
    cache: JwksCache,
    issuer: Option<String>,
    audience: Option<String>,
}

impl JwtVerifier {
    pub fn new(
        client: reqwest::Client,
        jwks_url: impl Into<String>,
        issuer: Option<String>,
        audience: Option<String>,
    ) -> Self {
        Self {
            cache: JwksCache::new(client, jwks_url),
            issuer,
            audience,
        }
    }

    pub fn jwks_url(&self) -> &str {
        self.cache.url()
    }

    /// Verify a token.
    ///
    /// The JWKS is refreshed only when the token references a key that is not
    /// in the cached set. Invalid signatures, expired tokens and issuer or
    /// audience mismatches do not trigger a network request.
    pub async fn verify(&self, token: &str) -> Result<VerifiedJwt> {
        let keys = self.cache.get().await?;
        match verify_jwt(
            &keys,
            token,
            self.issuer.as_deref(),
            self.audience.as_deref(),
        ) {
            Ok(verified) => Ok(verified),
            Err(error) if is_key_miss(&error) => {
                let keys = match self.cache.refresh_after_key_miss().await {
                    Ok(keys) => keys,
                    Err(_) => return Err(error),
                };
                verify_jwt(
                    &keys,
                    token,
                    self.issuer.as_deref(),
                    self.audience.as_deref(),
                )
                .map_err(|_| error)
            }
            Err(error) => Err(error),
        }
    }
}

fn is_key_miss(error: &OAuthError) -> bool {
    matches!(
        error,
        OAuthError::InvalidToken(message) if message.contains("no matching key")
    )
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use p256::ecdsa::SigningKey as P256SigningKey;
    use p384::ecdsa::SigningKey as P384SigningKey;
    use p521::ecdsa::SigningKey as P521SigningKey;
    use ring::signature::KeyPair;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey as RsaPkcs1SigningKey;
    use rsa::pss::SigningKey as RsaPssSigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::traits::PublicKeyParts;
    use serde_json::{Value, json};
    use sha2::{Sha256, Sha384, Sha512};

    use super::{Algorithm, Jwk, JwkSet, JwtClaims, KeyKind, validate_claims, verify_jwt};
    use crate::error::OAuthError;

    fn b64(bytes: impl AsRef<[u8]>) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn jwk(kty: &str, kid: &str, alg: &str) -> Jwk {
        Jwk {
            kty: kty.to_owned(),
            kid: Some(kid.to_owned()),
            alg: Some(alg.to_owned()),
            use_: Some("sig".to_owned()),
            key_ops: None,
            n: None,
            e: None,
            crv: None,
            x: None,
            y: None,
        }
    }

    fn rsa_jwk(key: &RsaPrivateKey, kid: &str, alg: &str) -> Jwk {
        let public = rsa::RsaPublicKey::from(key);
        Jwk {
            n: Some(b64(public.n().to_bytes_be())),
            e: Some(b64(public.e().to_bytes_be())),
            ..jwk("RSA", kid, alg)
        }
    }

    fn ec_jwk(kid: &str, alg: &str, curve: &str, point: &[u8]) -> Jwk {
        let coordinate_len = (point.len() - 1) / 2;
        Jwk {
            crv: Some(curve.to_owned()),
            x: Some(b64(&point[1..1 + coordinate_len])),
            y: Some(b64(&point[1 + coordinate_len..])),
            ..jwk("EC", kid, alg)
        }
    }

    fn okp_jwk(kid: &str, alg: &str, public_key: &[u8]) -> Jwk {
        Jwk {
            crv: Some("Ed25519".to_owned()),
            x: Some(b64(public_key)),
            ..jwk("OKP", kid, alg)
        }
    }

    fn sign_compact(
        alg: &str,
        kid: &str,
        claims: Value,
        sign: impl FnOnce(&[u8]) -> Vec<u8>,
    ) -> String {
        let header = json!({"alg": alg, "typ": "JWT", "kid": kid});
        let header = b64(serde_json::to_vec(&header).unwrap());
        let payload = b64(serde_json::to_vec(&claims).unwrap());
        let input = format!("{header}.{payload}");
        let signature = sign(input.as_bytes());
        format!("{input}.{}", b64(signature))
    }

    fn sign_with_bytes(alg: &str, kid: &str, claims: Value, signature: &[u8]) -> String {
        let header = json!({"alg": alg, "typ": "JWT", "kid": kid});
        let header = b64(serde_json::to_vec(&header).unwrap());
        let payload = b64(serde_json::to_vec(&claims).unwrap());
        format!("{header}.{payload}.{}", b64(signature))
    }

    fn sign_eddsa(pair: &ring::signature::Ed25519KeyPair, kid: &str, claims: Value) -> String {
        let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": kid});
        let header = b64(serde_json::to_vec(&header).unwrap());
        let payload = b64(serde_json::to_vec(&claims).unwrap());
        let input = format!("{header}.{payload}");
        format!("{input}.{}", b64(pair.sign(input.as_bytes()).as_ref()))
    }

    fn sign_p256(key: &P256SigningKey, input: &[u8]) -> Vec<u8> {
        use p256::ecdsa::signature::Signer;
        let signature: p256::ecdsa::Signature = key.sign(input);
        signature.to_bytes().to_vec()
    }

    fn sign_p384(key: &P384SigningKey, input: &[u8]) -> Vec<u8> {
        use p384::ecdsa::signature::Signer;
        let signature: p384::ecdsa::Signature = key.sign(input);
        signature.to_bytes().to_vec()
    }

    fn sign_p521(key: &P521SigningKey, input: &[u8]) -> Vec<u8> {
        use p521::ecdsa::signature::Signer;
        let signature: p521::ecdsa::Signature = key.sign(input);
        signature.to_bytes().to_vec()
    }

    fn claims() -> Value {
        json!({
            "iss": "https://idp.example",
            "aud": "client",
            "sub": "user",
            "exp": chrono::Utc::now().timestamp() + 600
        })
    }

    fn rsa_key() -> RsaPrivateKey {
        RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap()
    }

    #[test]
    fn verifies_rs256_rs384_and_rs512() {
        let key = rsa_key();
        let cases = [
            ("RS256", Algorithm::Rs256),
            ("RS384", Algorithm::Rs384),
            ("RS512", Algorithm::Rs512),
        ];
        for (alg, expected) in cases {
            assert_eq!(Algorithm::from_jose(alg), Some(expected));
            let token = match alg {
                "RS256" => sign_compact(alg, "key", claims(), |input| {
                    RsaPkcs1SigningKey::<Sha256>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
                "RS384" => sign_compact(alg, "key", claims(), |input| {
                    RsaPkcs1SigningKey::<Sha384>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
                _ => sign_compact(alg, "key", claims(), |input| {
                    RsaPkcs1SigningKey::<Sha512>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
            };
            let set = JwkSet {
                keys: vec![rsa_jwk(&key, "key", alg)],
            };
            let verified = verify_jwt(&set, &token, Some("https://idp.example"), Some("client"))
                .unwrap_or_else(|error| panic!("{alg} failed: {error}"));
            assert_eq!(verified.header.alg, alg);
        }
    }

    #[test]
    fn verifies_ps256_ps384_and_ps512() {
        let key = rsa_key();
        let cases = [
            ("PS256", Algorithm::Ps256),
            ("PS384", Algorithm::Ps384),
            ("PS512", Algorithm::Ps512),
        ];
        for (alg, expected) in cases {
            assert_eq!(Algorithm::from_jose(alg), Some(expected));
            let token = match alg {
                "PS256" => sign_compact(alg, "key", claims(), |input| {
                    RsaPssSigningKey::<Sha256>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
                "PS384" => sign_compact(alg, "key", claims(), |input| {
                    RsaPssSigningKey::<Sha384>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
                _ => sign_compact(alg, "key", claims(), |input| {
                    RsaPssSigningKey::<Sha512>::new(key.clone())
                        .sign(input)
                        .to_vec()
                }),
            };
            let set = JwkSet {
                keys: vec![rsa_jwk(&key, "key", alg)],
            };
            verify_jwt(&set, &token, None, None)
                .unwrap_or_else(|error| panic!("{alg} failed: {error}"));
        }
    }

    #[test]
    fn verifies_es256_es384_and_es512() {
        let p256 = P256SigningKey::random(&mut rand::thread_rng());
        let p384 = P384SigningKey::random(&mut rand::thread_rng());
        let p521 = P521SigningKey::random(&mut rand::thread_rng());

        let cases: Vec<(&str, Algorithm, Jwk, String)> = vec![
            (
                "ES256",
                Algorithm::Es256,
                ec_jwk(
                    "key",
                    "ES256",
                    "P-256",
                    p256.verifying_key().to_encoded_point(false).as_bytes(),
                ),
                sign_compact("ES256", "key", claims(), |input| sign_p256(&p256, input)),
            ),
            (
                "ES384",
                Algorithm::Es384,
                ec_jwk(
                    "key",
                    "ES384",
                    "P-384",
                    p384.verifying_key().to_encoded_point(false).as_bytes(),
                ),
                sign_compact("ES384", "key", claims(), |input| sign_p384(&p384, input)),
            ),
            (
                "ES512",
                Algorithm::Es512,
                ec_jwk(
                    "key",
                    "ES512",
                    "P-521",
                    p521::ecdsa::VerifyingKey::from(&p521)
                        .to_encoded_point(false)
                        .as_bytes(),
                ),
                sign_compact("ES512", "key", claims(), |input| sign_p521(&p521, input)),
            ),
        ];
        for (alg, expected, key, token) in cases {
            assert_eq!(Algorithm::from_jose(alg), Some(expected));
            let set = JwkSet { keys: vec![key] };
            verify_jwt(&set, &token, None, None)
                .unwrap_or_else(|error| panic!("{alg} failed: {error}"));
        }
    }

    #[test]
    fn verifies_eddsa_with_an_okp_key() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();

        let token = sign_eddsa(&pair, "key", claims());

        let set = JwkSet {
            keys: vec![okp_jwk("key", "EdDSA", pair.public_key().as_ref())],
        };
        let verified = verify_jwt(&set, &token, None, None).unwrap();
        assert_eq!(verified.header.alg, "EdDSA");
    }

    #[test]
    fn rejects_alg_none_and_unknown_algorithms() {
        let key = rsa_key();
        let set = JwkSet {
            keys: vec![rsa_jwk(&key, "key", "RS256")],
        };
        let unsigned = sign_with_bytes("none", "key", claims(), b"");
        let error = verify_jwt(&set, &unsigned, None, None).unwrap_err();
        assert!(error.to_string().contains("alg=none"), "{error}");

        let unknown = sign_with_bytes("HS256", "key", claims(), b"signature");
        let error = verify_jwt(&set, &unknown, None, None).unwrap_err();
        assert!(error.to_string().contains("unsupported signing algorithm"));

        assert_eq!(Algorithm::from_jose("none"), None);
        assert_eq!(Algorithm::from_jose("HS256"), None);
        assert_eq!(Algorithm::from_jose("rs256"), None);
    }

    #[test]
    fn rejects_algorithm_and_key_type_confusion() {
        let rsa = rsa_key();
        let ec = P256SigningKey::random(&mut rand::thread_rng());
        let rs_token = sign_compact("RS256", "shared", claims(), |input| {
            RsaPkcs1SigningKey::<Sha256>::new(rsa.clone())
                .sign(input)
                .to_vec()
        });
        let es_token = sign_compact("ES256", "shared", claims(), |input| sign_p256(&ec, input));

        let ec_set = JwkSet {
            keys: vec![ec_jwk(
                "shared",
                "ES256",
                "P-256",
                ec.verifying_key().to_encoded_point(false).as_bytes(),
            )],
        };
        let error = verify_jwt(&ec_set, &rs_token, None, None).unwrap_err();
        assert!(error.to_string().contains("no matching key"), "{error}");

        let rsa_set = JwkSet {
            keys: vec![rsa_jwk(&rsa, "shared", "RS256")],
        };
        let error = verify_jwt(&rsa_set, &es_token, None, None).unwrap_err();
        assert!(error.to_string().contains("no matching key"), "{error}");
    }

    #[test]
    fn rejects_unknown_kid() {
        let key = rsa_key();
        let set = JwkSet {
            keys: vec![rsa_jwk(&key, "known", "RS256")],
        };
        let token = sign_compact("RS256", "rotated", claims(), |input| {
            RsaPkcs1SigningKey::<Sha256>::new(key.clone())
                .sign(input)
                .to_vec()
        });
        let error = verify_jwt(&set, &token, None, None).unwrap_err();
        assert!(error.to_string().contains("no matching key"), "{error}");
    }

    #[test]
    fn rejects_a_jwk_alg_that_differs_from_the_header() {
        let key = rsa_key();
        let set = JwkSet {
            keys: vec![rsa_jwk(&key, "key", "RS256")],
        };
        let token = sign_compact("RS384", "key", claims(), |input| {
            RsaPkcs1SigningKey::<Sha384>::new(key.clone())
                .sign(input)
                .to_vec()
        });
        let error = verify_jwt(&set, &token, None, None).unwrap_err();
        assert!(
            error.to_string().contains("signature verification failed"),
            "{error}"
        );
    }

    #[test]
    fn rejects_wrong_signatures_for_every_key_family() {
        let rsa = rsa_key();
        let other_rsa = rsa_key();
        let p256 = P256SigningKey::random(&mut rand::thread_rng());
        let other_p256 = P256SigningKey::random(&mut rand::thread_rng());
        let p521 = P521SigningKey::random(&mut rand::thread_rng());
        let other_p521 = P521SigningKey::random(&mut rand::thread_rng());

        let rng = ring::rand::SystemRandom::new();
        let ed_pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let ed = ring::signature::Ed25519KeyPair::from_pkcs8(ed_pkcs8.as_ref()).unwrap();
        let other_ed_pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let other_ed =
            ring::signature::Ed25519KeyPair::from_pkcs8(other_ed_pkcs8.as_ref()).unwrap();

        let cases: Vec<(String, Jwk)> = vec![
            (
                sign_compact("RS256", "key", claims(), |input| {
                    RsaPkcs1SigningKey::<Sha256>::new(other_rsa.clone())
                        .sign(input)
                        .to_vec()
                }),
                rsa_jwk(&rsa, "key", "RS256"),
            ),
            (
                sign_compact("PS256", "key", claims(), |input| {
                    RsaPssSigningKey::<Sha256>::new(other_rsa.clone())
                        .sign(input)
                        .to_vec()
                }),
                rsa_jwk(&rsa, "key", "PS256"),
            ),
            (
                sign_compact("ES256", "key", claims(), |input| {
                    sign_p256(&other_p256, input)
                }),
                ec_jwk(
                    "key",
                    "ES256",
                    "P-256",
                    p256.verifying_key().to_encoded_point(false).as_bytes(),
                ),
            ),
            (
                sign_compact("ES512", "key", claims(), |input| {
                    sign_p521(&other_p521, input)
                }),
                ec_jwk(
                    "key",
                    "ES512",
                    "P-521",
                    p521::ecdsa::VerifyingKey::from(&p521)
                        .to_encoded_point(false)
                        .as_bytes(),
                ),
            ),
            (
                sign_eddsa(&other_ed, "key", claims()),
                okp_jwk("key", "EdDSA", ed.public_key().as_ref()),
            ),
        ];

        for (token, key) in cases {
            let set = JwkSet { keys: vec![key] };
            assert!(verify_jwt(&set, &token, None, None).is_err());
        }
    }

    #[test]
    fn parses_rsa_ec_and_okp_jwks() {
        let set: JwkSet = serde_json::from_value(json!({
            "keys": [
                {"kty": "RSA", "kid": "rsa", "alg": "RS256", "n": "AQAB", "e": "AQAB"},
                {"kty": "EC", "kid": "p256", "crv": "P-256", "x": "AA", "y": "AA"},
                {"kty": "EC", "kid": "p384", "crv": "P-384", "x": "AA", "y": "AA"},
                {"kty": "EC", "kid": "p521", "crv": "P-521", "x": "AA", "y": "AA"},
                {"kty": "OKP", "kid": "ed", "crv": "Ed25519", "x": "AA"}
            ]
        }))
        .unwrap();

        assert_eq!(set.candidates(KeyKind::Rsa, None).len(), 1);
        assert_eq!(set.candidates(KeyKind::EcP256, None).len(), 1);
        assert_eq!(set.candidates(KeyKind::EcP384, None).len(), 1);
        assert_eq!(set.candidates(KeyKind::EcP521, None).len(), 1);
        assert_eq!(set.candidates(KeyKind::OkpEd25519, None).len(), 1);
        assert_eq!(set.candidates(KeyKind::EcP256, Some("p384")).len(), 0);
    }

    #[test]
    fn ignores_keys_that_are_not_for_signature_verification() {
        let key = rsa_key();
        let mut encryption_key = rsa_jwk(&key, "key", "RS256");
        encryption_key.use_ = Some("enc".to_owned());
        let token = sign_compact("RS256", "key", claims(), |input| {
            RsaPkcs1SigningKey::<Sha256>::new(key.clone())
                .sign(input)
                .to_vec()
        });
        let set = JwkSet {
            keys: vec![encryption_key],
        };
        assert!(verify_jwt(&set, &token, None, None).is_err());

        let mut verify_only = rsa_jwk(&key, "key", "RS256");
        verify_only.key_ops = Some(vec!["verify".to_owned()]);
        let set = JwkSet {
            keys: vec![verify_only],
        };
        assert!(verify_jwt(&set, &token, None, None).is_ok());
    }

    #[test]
    fn rejects_malformed_ec_and_okp_coordinates() {
        let p256 = P256SigningKey::random(&mut rand::thread_rng());
        let token = sign_compact("ES256", "key", claims(), |input| sign_p256(&p256, input));
        let mut key = ec_jwk(
            "key",
            "ES256",
            "P-256",
            p256.verifying_key().to_encoded_point(false).as_bytes(),
        );
        key.x = Some(b64([0u8; 31]));
        let set = JwkSet { keys: vec![key] };
        assert!(verify_jwt(&set, &token, None, None).is_err());

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut key = okp_jwk("key", "EdDSA", pair.public_key().as_ref());
        key.x = Some(b64([0u8; 31]));
        let token = sign_with_bytes("EdDSA", "key", claims(), b"not-a-signature");
        let set = JwkSet { keys: vec![key] };
        assert!(verify_jwt(&set, &token, None, None).is_err());
    }

    #[test]
    fn rejects_expired_and_mismatched_claims() {
        let expired = JwtClaims {
            iss: Some("https://idp.example".to_owned()),
            aud: Some(vec!["client".to_owned()]),
            exp: Some(1),
            ..JwtClaims::default()
        };
        assert!(matches!(
            validate_claims(&expired, None, None),
            Err(OAuthError::InvalidToken(_))
        ));

        let claims = JwtClaims {
            exp: Some(chrono::Utc::now().timestamp() + 300),
            ..expired
        };
        assert!(validate_claims(&claims, Some("https://idp.example"), Some("client")).is_ok());
        assert!(validate_claims(&claims, Some("https://other.example"), None).is_err());
        assert!(validate_claims(&claims, None, Some("other-client")).is_err());
    }

    #[test]
    fn rejects_missing_exp_and_future_nbf() {
        let now = chrono::Utc::now().timestamp();
        let missing_exp = JwtClaims::default();
        assert!(
            validate_claims(&missing_exp, None, None)
                .unwrap_err()
                .to_string()
                .contains("missing exp")
        );

        let future_nbf = JwtClaims {
            exp: Some(now + 600),
            nbf: Some(now + 600),
            ..JwtClaims::default()
        };
        assert!(
            validate_claims(&future_nbf, None, None)
                .unwrap_err()
                .to_string()
                .contains("not valid yet")
        );
    }
}
