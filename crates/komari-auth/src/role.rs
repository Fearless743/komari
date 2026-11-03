//! Request roles, matching `api.RoleAdmin`, `api.RoleClient` and `api.RoleGuest`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::AuthError;

/// The role attached to an authenticated request.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Unauthenticated visitor.
    #[default]
    Guest,
    /// Agent/client token holder.
    Client,
    /// Administrator session or API key holder.
    Admin,
}

impl Role {
    /// The lowercase wire/JSON name used by the Go backend.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Client => "client",
            Self::Admin => "admin",
        }
    }

    /// Whether this role is one of the `allowed` roles.
    ///
    /// This mirrors Go `RequireRole`, which performs an exact membership test
    /// rather than a hierarchy comparison. Routes that admins and agents may
    /// both use therefore pass `[Role::Admin, Role::Client]`.
    pub fn allows(self, allowed: &[Role]) -> bool {
        allowed.contains(&self)
    }

    /// Whether this role is at least as privileged as `other`.
    ///
    /// Useful for display logic; authorization itself uses [`Role::allows`]
    /// so it stays byte-for-byte compatible with the Go middleware.
    pub fn at_least(self, other: Role) -> bool {
        self >= other
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = AuthError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "guest" => Ok(Self::Guest),
            "client" => Ok(Self::Client),
            "admin" => Ok(Self::Admin),
            other => Err(AuthError::InvalidRole(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_parsing_match_go() {
        assert_eq!(Role::Admin.as_str(), "admin");
        assert_eq!(Role::Client.as_str(), "client");
        assert_eq!(Role::Guest.as_str(), "guest");
        assert_eq!("ADMIN".parse::<Role>().unwrap(), Role::Admin);
        assert_eq!("client".parse::<Role>().unwrap(), Role::Client);
        assert!("root".parse::<Role>().is_err());
    }

    #[test]
    fn allows_uses_exact_membership() {
        assert!(Role::Admin.allows(&[Role::Admin]));
        assert!(Role::Admin.allows(&[Role::Admin, Role::Client]));
        assert!(!Role::Client.allows(&[Role::Admin]));
        assert!(Role::Client.allows(&[Role::Admin, Role::Client]));
        assert!(!Role::Guest.allows(&[Role::Admin, Role::Client]));
    }

    #[test]
    fn ordering_matches_privilege() {
        assert!(Role::Admin.at_least(Role::Client));
        assert!(Role::Client.at_least(Role::Guest));
        assert!(!Role::Guest.at_least(Role::Client));
    }
}
