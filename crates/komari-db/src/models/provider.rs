use serde::{Deserialize, Serialize};

use super::FromRow;

/// Shared shape of `oidc_providers`, `message_sender_providers` and
/// `ddns_providers`: a primary-key name plus a JSON `addition` blob.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Provider {
    pub name: String,
    pub addition: String,
}

impl Provider {
    pub fn new(name: impl Into<String>, addition: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            addition: addition.into(),
        }
    }
}

impl FromRow for Provider {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get("name")?,
            addition: row.get("addition")?,
        })
    }
}

/// OAuth/OIDC provider configuration (`oidc_providers`).
pub type OidcProvider = Provider;
/// Notification provider configuration (`message_sender_providers`).
pub type MessageSenderProvider = Provider;
/// DDNS provider configuration (`ddns_providers`).
pub type DdnsProvider = Provider;
