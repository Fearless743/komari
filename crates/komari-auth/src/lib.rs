//! Authentication primitives shared by the HTTP API.
//!
//! The Go backend distinguishes three identities (`admin`, `client`, `guest`)
//! and authenticates agents with a per-client token. Tokens arrive either as a
//! `?token=` query parameter (the historical agent behaviour) or as an
//! `Authorization: Bearer <token>` header. This crate keeps that policy in one
//! place so every router applies it identically.

use http::HeaderMap;
use http::header::AUTHORIZATION;
use komari_db::{Db, DbError};

/// Identity used by the role guard, mirroring `api.RoleAdmin` and friends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    Client,
    Guest,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Client => "client",
            Self::Guest => "guest",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A successfully authenticated agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentity {
    pub uuid: String,
    pub role: Role,
}

impl AgentIdentity {
    pub fn client(uuid: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            role: Role::Client,
        }
    }
}

/// Reasons a request can fail authentication or authorisation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("missing credential")]
    Missing,
    #[error("invalid credential")]
    Invalid,
    #[error("forbidden")]
    Forbidden,
}

const BEARER: &str = "Bearer ";

/// Extract a bearer token from `Authorization`, matching the Go check
/// (`"Bearer "+key`) without accepting other schemes.
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix(BEARER)?.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

/// Extract the agent token from the query string or the bearer header.
///
/// The query parameter wins, matching `api.extractClientToken`.
pub fn extract_agent_token(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    if let Some(token) = query_token.map(str::trim).filter(|token| !token.is_empty()) {
        return Some(token.to_owned());
    }
    bearer_token(headers)
}

/// Look up the client that owns `token`.
pub fn authenticate_agent(db: &Db, token: &str) -> Result<Option<AgentIdentity>, DbError> {
    if token.is_empty() {
        return Ok(None);
    }
    Ok(db
        .clients()
        .get_by_token(token)?
        .map(|client| AgentIdentity::client(client.uuid)))
}

/// Resolve a token and enforce that the resulting role is allowed.
///
/// A missing or unknown token is reported as [`AuthError::Missing`] /
/// [`AuthError::Invalid`]; a known identity without the required role is
/// [`AuthError::Forbidden`].
pub fn require_role(
    db: &Db,
    token: Option<&str>,
    allowed: &[Role],
) -> Result<AgentIdentity, RequireRoleError> {
    let token = token.ok_or(AuthError::Missing)?;
    let identity = authenticate_agent(db, token)
        .map_err(RequireRoleError::Backend)?
        .ok_or(AuthError::Invalid)?;
    if !allowed.contains(&identity.role) {
        return Err(AuthError::Forbidden.into());
    }
    Ok(identity)
}

/// Error returned by [`require_role`].
#[derive(Debug, thiserror::Error)]
pub enum RequireRoleError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("database error: {0}")]
    Backend(DbError),
}

/// Check an already resolved identity against the allowed roles.
pub fn guard_role(
    identity: Option<AgentIdentity>,
    allowed: &[Role],
) -> Result<AgentIdentity, AuthError> {
    let identity = identity.ok_or(AuthError::Missing)?;
    if !allowed.contains(&identity.role) {
        return Err(AuthError::Forbidden);
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn bearer_tokens_are_extracted_strictly() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_token(&headers), None);

        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc"));

        headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer abc"));
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn query_token_takes_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer header"));
        assert_eq!(
            extract_agent_token(&headers, Some("query")).as_deref(),
            Some("query")
        );
        assert_eq!(
            extract_agent_token(&headers, None).as_deref(),
            Some("header")
        );
    }

    #[test]
    fn role_guard_rejects_guests() {
        assert_eq!(guard_role(None, &[Role::Client]), Err(AuthError::Missing));
        assert_eq!(
            guard_role(Some(AgentIdentity::client("a")), &[Role::Admin]),
            Err(AuthError::Forbidden)
        );
        assert!(guard_role(Some(AgentIdentity::client("a")), &[Role::Client]).is_ok());
    }
}
