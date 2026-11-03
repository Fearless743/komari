//! CSRF `state` storage and PKCE helpers.
//!
//! The Go backend keeps a `go-cache` entry per authorization request. The Rust
//! implementation uses the same model (random state, five minute TTL) but
//! *consumes* the entry on the callback, so a state value cannot be replayed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use rand::Rng;
use rand::distributions::Alphanumeric;
use sha2::{Digest, Sha256};

use crate::error::{OAuthError, Result};

/// Default lifetime of an authorization request (Go uses five minutes).
pub const DEFAULT_STATE_TTL: Duration = Duration::from_secs(5 * 60);

/// One pending authorization request.
#[derive(Clone)]
pub struct StateEntry {
    pub redirect_uri: String,
    pub pkce_verifier: Option<String>,
    pub nonce: Option<String>,
    created_at: Instant,
}

impl std::fmt::Debug for StateEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateEntry")
            .field("redirect_uri", &self.redirect_uri)
            .field(
                "pkce_verifier",
                &self
                    .pkce_verifier
                    .as_deref()
                    .map(crate::error::debug_secret),
            )
            .field(
                "nonce",
                &self.nonce.as_deref().map(crate::error::debug_secret),
            )
            .finish_non_exhaustive()
    }
}

/// Thread-safe state store with a fixed TTL.
pub struct StateStore {
    ttl: Duration,
    entries: Mutex<HashMap<String, StateEntry>>,
}

impl std::fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("ttl", &self.ttl)
            .field("entries", &self.len())
            .finish()
    }
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new(DEFAULT_STATE_TTL)
    }
}

impl StateStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Generate a random state, remember the request and return the state.
    pub fn issue(
        &self,
        redirect_uri: impl Into<String>,
        pkce_verifier: Option<String>,
        nonce: Option<String>,
    ) -> String {
        let state = random_string(16);
        self.insert(
            &state,
            StateEntry {
                redirect_uri: redirect_uri.into(),
                pkce_verifier,
                nonce,
                created_at: Instant::now(),
            },
        );
        state
    }

    /// Insert a specific state value. Primarily useful in tests.
    pub fn insert(&self, state: &str, entry: StateEntry) {
        let now = Instant::now();
        let ttl = self.ttl;
        let mut entries = self.entries.lock();
        entries.retain(|_, value| now.duration_since(value.created_at) <= ttl);
        entries.insert(state.to_owned(), entry);
    }

    /// Register an already-generated state value.
    pub fn issue_with_state(&self, redirect_uri: impl Into<String>, state: &str) -> String {
        self.insert(
            state,
            StateEntry {
                redirect_uri: redirect_uri.into(),
                pkce_verifier: None,
                nonce: None,
                created_at: Instant::now(),
            },
        );
        state.to_owned()
    }

    /// Remove and validate a state value.
    ///
    /// The entry is removed even when validation fails, matching the
    /// single-use semantics of an authorization request.
    pub fn consume(&self, state: &str, redirect_uri: &str) -> Result<StateEntry> {
        if state.is_empty() {
            return Err(OAuthError::InvalidState);
        }
        let entry = self
            .entries
            .lock()
            .remove(state)
            .ok_or(OAuthError::InvalidState)?;
        if entry.created_at.elapsed() > self.ttl {
            return Err(OAuthError::InvalidState);
        }
        if entry.redirect_uri != redirect_uri {
            return Err(OAuthError::RedirectUriMismatch);
        }
        Ok(entry)
    }

    /// Number of pending requests (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.entries.lock().clear();
    }
}

/// A PKCE verifier/challenge pair (RFC 7636, `S256`).
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pkce")
            .field("verifier", &crate::error::debug_secret(&self.verifier))
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// Generate a 64-character verifier and its SHA-256 challenge.
    pub fn generate() -> Self {
        let verifier = random_string(64);
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }
}

/// Random alphanumeric string with `length` characters, matching the shape of
/// Go's `utils.GenerateRandomString`.
pub fn random_string(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Pkce, StateStore};
    use std::time::Duration;

    #[test]
    fn state_is_single_use_and_bound_to_redirect_uri() {
        let store = StateStore::default();
        let state = store.issue("https://app.example/callback", None, None);
        assert_eq!(store.len(), 1);

        assert!(
            store
                .consume(&state, "https://evil.example/callback")
                .is_err()
        );
        assert_eq!(store.len(), 0);
        assert!(
            store
                .consume(&state, "https://app.example/callback")
                .is_err()
        );
    }

    #[test]
    fn expired_state_is_rejected() {
        let store = StateStore::new(Duration::from_millis(1));
        let state = store.issue("https://app.example/callback", None, None);
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            store
                .consume(&state, "https://app.example/callback")
                .is_err()
        );
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = Pkce::generate();
        assert_eq!(pkce.verifier.len(), 64);
        assert_eq!(pkce.challenge.len(), 43);
    }

    #[test]
    fn debug_output_hides_state_and_pkce_verifier() {
        let store = StateStore::default();
        let state = store.issue(
            "https://app.example/callback",
            Some("verifier-secret".to_owned()),
            Some("nonce-secret".to_owned()),
        );
        let store_debug = format!("{store:?}");
        assert!(!store_debug.contains(&state));
        assert!(!store_debug.contains("verifier-secret"));

        let entry = store
            .consume(&state, "https://app.example/callback")
            .unwrap();
        let entry_debug = format!("{entry:?}");
        assert!(!entry_debug.contains("verifier-secret"));
        assert!(!entry_debug.contains("nonce-secret"));

        let pkce = Pkce::generate();
        let pkce_debug = format!("{pkce:?}");
        assert!(!pkce_debug.contains(&pkce.verifier));
        assert!(pkce_debug.contains(&pkce.challenge));
    }
}
