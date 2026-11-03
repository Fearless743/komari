//! Minimal RS256 JWT/JWKS verification.
//!
//! Komari only needs to verify signed identity tokens from OpenID Connect
//! providers and Cloudflare Access. Rather than pulling in a full JOSE stack,
//! this module implements the small, well-defined subset:
//!
//! * compact JWS (`header.payload.signature`),
//! * `RS256`, `RS384` and `RS512`,
//! * RSA keys published as JWKS (`kty = "RSA"`, `n`, `e`),
//! * `iss`, `aud`, `exp` and `nbf` validation.
//!
//! Signature verification uses `ring`, which is already part of the workspace's
//! TLS stack.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use parking_lot::RwLock;
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

/// One RSA key from a JWKS document.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
}

/// A JWKS document.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct JwkSet {
    #[serde(default)]
    pub keys: Vec<Jwk>,
}

impl JwkSet {
    fn candidates(&self, kid: Option<&str>) -> Vec<&Jwk> {
        let rsa = self
            .keys
            .iter()
            .filter(|key| key.kty.eq_ignore_ascii_case("RSA"))
            .collect::<Vec<_>>();
        match kid {
            Some(kid) => rsa
                .into_iter()
                .filter(|key| key.kid.as_deref() == Some(kid))
                .collect(),
            None => rsa,
        }
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

    let algorithm = match header.alg.as_str() {
        "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
        "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
        "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
        other => {
            return Err(OAuthError::InvalidToken(format!(
                "unsupported signing algorithm {other:?}"
            )));
        }
    };

    let signing_input = format!("{header_b64}.{payload_b64}");
    let candidates = jwks.candidates(header.kid.as_deref());
    if candidates.is_empty() {
        return Err(OAuthError::InvalidToken(
            "no matching RSA key in JWKS".to_owned(),
        ));
    }

    let mut verified = false;
    for key in candidates {
        if let Some(alg) = &key.alg {
            if !alg.eq_ignore_ascii_case(&header.alg) {
                continue;
            }
        }
        let (Some(n), Some(e)) = (key.n.as_deref(), key.e.as_deref()) else {
            continue;
        };
        let modulus = decode_base64url(n)?;
        let exponent = decode_base64url(e)?;
        let der = rsa_public_key_der(&modulus, &exponent);
        let public_key = signature::UnparsedPublicKey::new(algorithm, der);
        if public_key
            .verify(signing_input.as_bytes(), &signature_bytes)
            .is_ok()
        {
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
/// its `RSA_PKCS1_*` verification algorithms.
fn rsa_public_key_der(modulus: &[u8], exponent: &[u8]) -> Vec<u8> {
    let mut rsa_key = Vec::new();
    rsa_key.extend(der_integer(modulus));
    rsa_key.extend(der_integer(exponent));
    der_tlv(0x30, &rsa_key)
}

/// A cached JWKS document that refreshes once when a token does not match.
pub struct JwksCache {
    url: String,
    client: reqwest::Client,
    keys: RwLock<Option<Arc<JwkSet>>>,
}

impl JwksCache {
    pub fn new(client: reqwest::Client, url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client,
            keys: RwLock::new(None),
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

    /// Verify a token, refreshing the JWKS once if the first attempt fails.
    pub async fn verify(&self, token: &str) -> Result<VerifiedJwt> {
        let keys = self.cache.get().await?;
        match verify_jwt(
            &keys,
            token,
            self.issuer.as_deref(),
            self.audience.as_deref(),
        ) {
            Ok(verified) => Ok(verified),
            Err(first_error) => {
                let keys = self.cache.refresh().await?;
                verify_jwt(
                    &keys,
                    token,
                    self.issuer.as_deref(),
                    self.audience.as_deref(),
                )
                .map_err(|_| first_error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Jwk, JwkSet, JwtClaims, validate_claims};
    use crate::error::OAuthError;

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
    fn selects_keys_by_kid() {
        let set = JwkSet {
            keys: vec![
                Jwk {
                    kty: "RSA".to_owned(),
                    kid: Some("one".to_owned()),
                    alg: Some("RS256".to_owned()),
                    n: None,
                    e: None,
                },
                Jwk {
                    kty: "RSA".to_owned(),
                    kid: Some("two".to_owned()),
                    alg: Some("RS256".to_owned()),
                    n: None,
                    e: None,
                },
            ],
        };
        assert_eq!(set.candidates(Some("two")).len(), 1);
        assert_eq!(set.candidates(None).len(), 2);
    }
}
