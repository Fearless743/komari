//! Login sessions stored in the `sessions` table.
//!
//! Session identifiers are 32-character high-entropy strings from the same
//! 62-character alphabet as Go `utils.GenerateRandomString(32)`, and expire at
//! an absolute instant, exactly like `accounts.CreateSession`.

use komari_db::Db;
use komari_db::models::{Session, User};

use crate::error::{AuthError, Result};
use crate::role::Role;

/// Session lifetime used by the password login endpoint in Go (30 days).
pub const DEFAULT_SESSION_TTL_SECONDS: i64 = 2_592_000;

/// Result of validating a session cookie.
#[derive(Clone, Debug)]
pub struct SessionValidation {
    /// The session row as stored in SQLite.
    pub session: Session,
    /// The owning user.
    pub user: User,
    /// Session logins are administrator sessions in the Go backend.
    pub role: Role,
}

impl SessionValidation {
    pub fn user_uuid(&self) -> &str {
        &self.user.uuid
    }

    pub fn username(&self) -> &str {
        &self.user.username
    }

    pub fn token(&self) -> &str {
        &self.session.session
    }
}

/// Create a session with the default 30-day absolute lifetime.
pub fn create_session(
    db: &Db,
    user_uuid: &str,
    user_agent: &str,
    ip: &str,
    login_method: &str,
) -> Result<String> {
    create_session_with_expiry(
        db,
        user_uuid,
        DEFAULT_SESSION_TTL_SECONDS,
        user_agent,
        ip,
        login_method,
    )
}

/// Create a session with an explicit lifetime in seconds.
pub fn create_session_with_expiry(
    db: &Db,
    user_uuid: &str,
    expires_in_seconds: i64,
    user_agent: &str,
    ip: &str,
    login_method: &str,
) -> Result<String> {
    let token =
        db.sessions()
            .create_id(user_uuid, expires_in_seconds, user_agent, ip, login_method)?;
    Ok(token)
}

/// Validate a session token and return the user/role.
///
/// Expired tokens are deleted. `latest_online` is refreshed and, because this
/// entry point has no request metadata, `latest_user_agent`/`latest_ip` are
/// set to the values captured when the session was created. HTTP middleware
/// should use [`validate_session_with`] to record the current request instead.
pub fn validate_session(db: &Db, token: &str) -> Result<Option<SessionValidation>> {
    validate_session_inner(db, token, None, None)
}

/// Validate a session token while recording the current request metadata.
pub fn validate_session_with(
    db: &Db,
    token: &str,
    user_agent: &str,
    ip: &str,
) -> Result<Option<SessionValidation>> {
    validate_session_inner(db, token, Some(user_agent), Some(ip))
}

fn validate_session_inner(
    db: &Db,
    token: &str,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> Result<Option<SessionValidation>> {
    if token.is_empty() {
        return Ok(None);
    }

    let Some(mut session) = db.sessions().get(token)? else {
        return Ok(None);
    };
    if session.is_expired() {
        db.sessions().delete(token)?;
        return Ok(None);
    }

    let Some(user) = db.accounts().get_by_uuid(&session.uuid)? else {
        // Orphaned session (user deleted without the foreign-key cascade).
        db.sessions().delete(token)?;
        return Ok(None);
    };

    let latest_user_agent = user_agent
        .filter(|value| !value.is_empty())
        .unwrap_or(&session.user_agent);
    let latest_ip = ip.filter(|value| !value.is_empty()).unwrap_or(&session.ip);
    db.sessions()
        .update_latest(token, latest_user_agent, latest_ip)?;
    session.latest_online = komari_db::LocalTime::now();
    session.latest_user_agent = latest_user_agent.to_owned();
    session.latest_ip = latest_ip.to_owned();

    Ok(Some(SessionValidation {
        session,
        user,
        role: Role::Admin,
    }))
}

/// Delete one session. Returns whether a row was removed.
pub fn delete_session(db: &Db, token: &str) -> Result<bool> {
    if token.is_empty() {
        return Ok(false);
    }
    Ok(db.sessions().delete(token)?)
}

/// Delete every session that has already expired.
pub fn delete_expired_sessions(db: &Db) -> Result<usize> {
    Ok(db.sessions().delete_expired()?)
}

/// Delete all sessions, matching Go `accounts.DeleteAllSessions`.
pub fn delete_all_sessions(db: &Db) -> Result<usize> {
    Ok(db.sessions().delete_all()?)
}

/// Load a session's owning user without refreshing activity metadata.
pub fn session_user(db: &Db, token: &str) -> Result<Option<User>> {
    Ok(db.sessions().user_by_session(token)?)
}

/// Delete a session and surface a [`AuthError::UserNotFound`] if it was absent.
pub fn require_session(db: &Db, token: &str) -> Result<SessionValidation> {
    validate_session(db, token)?.ok_or_else(|| AuthError::UserNotFound(token.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;

    fn database() -> (Db, User) {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let user = db.accounts().create("alice", "password").unwrap();
        (db, user)
    }

    #[test]
    fn creates_and_validates_a_session() {
        let (db, user) = database();
        let token = create_session(
            &db,
            &user.uuid,
            "komari-agent/1.0",
            "192.0.2.10",
            "password",
        )
        .unwrap();

        assert_eq!(token.len(), 32);
        let validated = validate_session_with(&db, &token, "browser/2.0", "198.51.100.7")
            .unwrap()
            .expect("session must be valid");
        assert_eq!(validated.user.username, "alice");
        assert_eq!(validated.role, Role::Admin);
        assert_eq!(validated.session.login_method, "password");
        assert_eq!(validated.session.latest_user_agent, "browser/2.0");
        assert_eq!(validated.session.latest_ip, "198.51.100.7");
        assert!(validated.session.expires > komari_db::LocalTime::now());

        let stored = db.sessions().get(&token).unwrap().unwrap();
        assert_eq!(stored.latest_user_agent, "browser/2.0");
        assert_eq!(stored.latest_ip, "198.51.100.7");
    }

    #[test]
    fn unknown_and_empty_tokens_are_rejected() {
        let (db, _) = database();
        assert!(validate_session(&db, "does-not-exist").unwrap().is_none());
        assert!(validate_session(&db, "").unwrap().is_none());
        assert!(!delete_session(&db, "does-not-exist").unwrap());
    }

    #[test]
    fn expired_sessions_are_deleted() {
        let (db, user) = database();
        let token =
            create_session_with_expiry(&db, &user.uuid, -1, "ua", "ip", "password").unwrap();
        assert!(validate_session(&db, &token).unwrap().is_none());
        assert!(db.sessions().get(&token).unwrap().is_none());
        assert_eq!(db.sessions().count().unwrap(), 0);
    }

    #[test]
    fn deletes_sessions_and_expired_rows() {
        let (db, user) = database();
        let live =
            create_session_with_expiry(&db, &user.uuid, 3600, "ua", "ip", "password").unwrap();
        let expired =
            create_session_with_expiry(&db, &user.uuid, -3600, "ua", "ip", "password").unwrap();

        assert_eq!(delete_expired_sessions(&db).unwrap(), 1);
        assert!(db.sessions().get(&expired).unwrap().is_none());
        assert!(delete_session(&db, &live).unwrap());
        assert_eq!(db.sessions().count().unwrap(), 0);
    }

    #[test]
    fn concurrent_creation_produces_unique_tokens() {
        let (db, user) = database();
        let db = Arc::new(db);
        let uuid = user.uuid.clone();

        let tokens: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..64)
                .map(|worker| {
                    let db = Arc::clone(&db);
                    let uuid = uuid.clone();
                    scope.spawn(move || {
                        (0..8)
                            .map(|index| {
                                create_session(
                                    &db,
                                    &uuid,
                                    &format!("agent/{worker}"),
                                    "127.0.0.1",
                                    "password",
                                )
                                .unwrap_or_else(|error| {
                                    panic!("session {worker}/{index} failed: {error}")
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().expect("worker thread"))
                .collect()
        });

        assert_eq!(tokens.len(), 64 * 8);
        let unique: HashSet<_> = tokens.iter().collect();
        assert_eq!(unique.len(), tokens.len());
        assert_eq!(db.sessions().count().unwrap() as usize, tokens.len());
    }

    #[test]
    fn session_rows_use_absolute_expiry() {
        let (db, user) = database();
        let before = komari_db::LocalTime::now();
        let token =
            create_session_with_expiry(&db, &user.uuid, 60, "ua", "ip", "password").unwrap();
        let session = db.sessions().get(&token).unwrap().unwrap();
        let created = session.created_at.timestamp().unwrap();
        let expires = session.expires.timestamp().unwrap();
        assert_eq!(expires - created, 60);
        assert!(session.created_at >= before);
    }
}
