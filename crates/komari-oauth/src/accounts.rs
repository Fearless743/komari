//! External-account lookup and binding.
//!
//! The Go backend stores the OAuth identity as
//! `fmt.Sprintf("%s_%s", providerName, userId)` in `users.sso_id` and leaves
//! `users.sso_type` for the admin UI. This module keeps that convention for
//! drop-in compatibility, while also accepting rows where `sso_id` contains
//! only the provider subject and `sso_type` names the provider.

use komari_db::Db;
use komari_db::models::User;
use komari_db::util::generate_password;

use crate::error::{OAuthError, Result};
use crate::types::OAuthUser;

/// An external identity, composed of the provider name and its stable subject.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SsoIdentity {
    pub provider: String,
    pub subject: String,
}

impl SsoIdentity {
    pub fn new(provider: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            subject: subject.into(),
        }
    }

    /// Go-compatible value stored in `users.sso_id`.
    pub fn combined_id(&self) -> String {
        format!("{}_{}", self.provider, self.subject)
    }

    fn validate(&self) -> Result<()> {
        if self.provider.trim().is_empty() {
            return Err(OAuthError::InvalidIdentity(
                "provider must not be empty".to_owned(),
            ));
        }
        if self.subject.trim().is_empty() {
            return Err(OAuthError::InvalidIdentity(
                "subject must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The information the session layer needs after a successful OAuth login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionUser {
    pub uuid: String,
    pub username: String,
    pub sso_type: String,
    pub sso_id: String,
}

impl From<&User> for SessionUser {
    fn from(user: &User) -> Self {
        Self {
            uuid: user.uuid.clone(),
            username: user.username.clone(),
            sso_type: user.sso_type.clone(),
            sso_id: user.sso_id.clone(),
        }
    }
}

/// Result of resolving an external identity to a local account.
#[derive(Clone, Debug)]
pub struct OAuthAccount {
    pub user: User,
    /// `true` when a new local account was created for this identity.
    pub created: bool,
}

impl OAuthAccount {
    pub fn session_user(&self) -> SessionUser {
        SessionUser::from(&self.user)
    }
}

/// Account operations backed by [`komari_db::Accounts`].
pub struct AccountLinker<'a> {
    db: &'a Db,
}

impl<'a> AccountLinker<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Find the local account bound to an external identity.
    pub fn find(&self, identity: &SsoIdentity) -> Result<Option<User>> {
        identity.validate()?;
        let accounts = self.db.accounts();
        let combined = identity.combined_id();
        if let Some(user) = accounts.get_by_sso(&combined)? {
            return Ok(Some(user));
        }

        // Compatibility with rows written by older integrations that store the
        // bare subject in `sso_id` and the provider in `sso_type`.
        if let Some(user) = accounts.get_by_sso(&identity.subject)? {
            if user.sso_type.is_empty() || user.sso_type == identity.provider {
                return Ok(Some(user));
            }
        }
        Ok(None)
    }

    /// Find an account or create one with a random local password.
    ///
    /// Existing accounts are never modified, so a local password is preserved
    /// even when the same account later signs in through OAuth.
    pub fn find_or_create(
        &self,
        identity: &SsoIdentity,
        preferred_username: Option<&str>,
    ) -> Result<OAuthAccount> {
        if let Some(user) = self.find(identity)? {
            return Ok(OAuthAccount {
                user,
                created: false,
            });
        }

        let username = self.unique_username(identity, preferred_username)?;
        let mut user = User::new(username, &generate_password());
        user.sso_type = identity.provider.clone();
        user.sso_id = identity.combined_id();
        self.db.accounts().insert(&user)?;
        Ok(OAuthAccount {
            user,
            created: true,
        })
    }

    /// Resolve an [`OAuthUser`] to a local account, creating it when needed.
    pub fn find_or_create_from_user(&self, oauth_user: &OAuthUser) -> Result<OAuthAccount> {
        let preferred = oauth_user
            .username
            .as_deref()
            .or_else(|| oauth_user.email.as_deref().and_then(email_local_part));
        self.find_or_create(&oauth_user.identity(), preferred)
    }

    /// Bind an external identity to an existing account.
    ///
    /// The local password hash is not touched. Binding fails when the identity
    /// already belongs to another account or when the target account already
    /// has a different identity bound.
    pub fn bind(&self, uuid: &str, identity: &SsoIdentity) -> Result<User> {
        self.bind_inner(uuid, identity, false)
    }

    /// Replace the target account's existing binding with a new identity.
    ///
    /// An identity owned by a *different* account is still rejected, so two
    /// accounts can never share an external login.
    pub fn rebind(&self, uuid: &str, identity: &SsoIdentity) -> Result<User> {
        self.bind_inner(uuid, identity, true)
    }

    fn bind_inner(&self, uuid: &str, identity: &SsoIdentity, replace: bool) -> Result<User> {
        identity.validate()?;
        if let Some(existing) = self.find(identity)? {
            if existing.uuid != uuid {
                return Err(OAuthError::AlreadyBound);
            }
        }

        let user = self.db.accounts().require_by_uuid(uuid)?;
        let combined = identity.combined_id();
        let already_bound =
            !user.sso_id.is_empty() && user.sso_id != combined && user.sso_id != identity.subject;
        if already_bound && !replace {
            return Err(OAuthError::UserAlreadyBound);
        }

        self.db.accounts().bind_sso(uuid, &combined)?;
        self.db
            .accounts()
            .update_profile(uuid, None, Some(&identity.provider))?;
        Ok(self.db.accounts().require_by_uuid(uuid)?)
    }

    /// Remove an external account binding.
    pub fn unbind(&self, uuid: &str) -> Result<User> {
        self.db.accounts().unbind_sso(uuid)?;
        Ok(self.db.accounts().require_by_uuid(uuid)?)
    }

    fn unique_username(
        &self,
        identity: &SsoIdentity,
        preferred_username: Option<&str>,
    ) -> Result<String> {
        let base = preferred_username
            .map(sanitize_username)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                sanitize_username(&format!("{}_{}", identity.provider, identity.subject))
            });
        let base = if base.is_empty() {
            "oauth-user".to_owned()
        } else {
            base
        };

        let accounts = self.db.accounts();
        if accounts.get_by_username(&base)?.is_none() {
            return Ok(base);
        }
        for suffix in 1..=1000 {
            let candidate = format!("{base}-{suffix}");
            if accounts.get_by_username(&candidate)?.is_none() {
                return Ok(candidate);
            }
        }
        Err(OAuthError::InvalidIdentity(
            "could not allocate a unique username".to_owned(),
        ))
    }
}

fn sanitize_username(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(50)
        .collect::<String>();
    sanitized.trim_matches('_').to_owned()
}

fn email_local_part(email: &str) -> Option<&str> {
    email.split('@').next().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use komari_db::Db;
    use komari_db::models::User;

    use super::{AccountLinker, SsoIdentity};

    fn database() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    #[test]
    fn find_or_create_is_idempotent_and_does_not_reuse_passwords() {
        let db = database();
        let linker = AccountLinker::new(&db);
        let identity = SsoIdentity::new("github", "42");

        let first = linker.find_or_create(&identity, Some("octocat")).unwrap();
        assert!(first.created);
        assert_eq!(first.user.username, "octocat");
        assert_eq!(first.user.sso_id, "github_42");
        assert!(!first.user.passwd.is_empty());

        let second = linker.find_or_create(&identity, Some("other")).unwrap();
        assert!(!second.created);
        assert_eq!(second.user.uuid, first.user.uuid);
        assert_eq!(second.user.passwd, first.user.passwd);
    }

    #[test]
    fn binding_preserves_existing_password_and_rejects_duplicates() {
        let db = database();
        let local = db.accounts().create("admin", "correct horse").unwrap();
        let linker = AccountLinker::new(&db);
        let identity = SsoIdentity::new("github", "99");

        let bound = linker.bind(&local.uuid, &identity).unwrap();
        assert_eq!(bound.sso_id, "github_99");
        assert_eq!(bound.sso_type, "github");
        assert!(bound.check_password("correct horse"));
        assert_eq!(bound.passwd, local.passwd);

        let other = db.accounts().create("other", "password").unwrap();
        assert!(matches!(
            linker.bind(&other.uuid, &identity),
            Err(crate::OAuthError::AlreadyBound)
        ));
        let stored: User = db.accounts().require_by_uuid(&other.uuid).unwrap();
        assert!(stored.sso_id.is_empty());
    }

    #[test]
    fn duplicate_username_gets_a_suffix() {
        let db = database();
        db.accounts().create("octocat", "password").unwrap();
        let linker = AccountLinker::new(&db);
        let account = linker
            .find_or_create(&SsoIdentity::new("github", "7"), Some("octocat"))
            .unwrap();
        assert_eq!(account.user.username, "octocat-1");
    }
}
