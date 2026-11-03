//! HTTP POST presence tracking.
//!
//! Agents may report over plain HTTP. Each successful report refreshes a TTL
//! timer; when the timer expires the client is marked offline and an offline
//! event is published. This mirrors `refreshPostPresence` /
//! `postPresenceExpired` in `api/client/report.go`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::clock::Clock;
use crate::ws::{PresenceSource, WsHub};

struct Entry {
    connection_id: i64,
    expires_at_millis: i64,
}

/// Result of refreshing a POST report session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceUpdate {
    pub connection_id: i64,
    /// `true` when this was the first report of a new session and an online
    /// event was published.
    pub newly_online: bool,
}

/// A POST session that expired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceExpiry {
    pub uuid: String,
    pub connection_id: i64,
}

/// Tracks TTL-based presence for clients reporting over HTTP.
pub struct PresenceTracker {
    clock: Arc<dyn Clock>,
    ttl: Duration,
    hub: Arc<WsHub>,
    entries: Mutex<HashMap<String, Entry>>,
}

impl PresenceTracker {
    pub fn new(clock: Arc<dyn Clock>, ttl: Duration, hub: Arc<WsHub>) -> Self {
        Self {
            clock,
            ttl,
            hub,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Refresh the session for `uuid`, publishing an online event the first
    /// time a session appears.
    pub fn refresh(&self, uuid: &str) -> PresenceUpdate {
        let now = self.clock.now_millis();
        let expires_at_millis = now.saturating_add(self.ttl.as_millis() as i64);
        let (connection_id, newly_online) = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            match entries.get_mut(uuid) {
                Some(entry) => {
                    entry.expires_at_millis = expires_at_millis;
                    (entry.connection_id, false)
                }
                None => {
                    let connection_id = self.hub.next_connection_id();
                    entries.insert(
                        uuid.to_owned(),
                        Entry {
                            connection_id,
                            expires_at_millis,
                        },
                    );
                    (connection_id, true)
                }
            }
        };

        self.hub.keep_alive_presence(uuid, connection_id, self.ttl);
        if newly_online {
            self.hub
                .emit_online(uuid, connection_id, PresenceSource::HttpPost);
        }
        PresenceUpdate {
            connection_id,
            newly_online,
        }
    }

    /// Remove sessions whose TTL elapsed. This is called by the background
    /// sweeper in production and directly by tests.
    pub fn expire_due(&self) -> Vec<PresenceExpiry> {
        let now = self.clock.now_millis();
        let expired = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            let due: Vec<String> = entries
                .iter()
                .filter(|(_, entry)| entry.expires_at_millis <= now)
                .map(|(uuid, _)| uuid.clone())
                .collect();
            let mut removed = Vec::with_capacity(due.len());
            for uuid in due {
                if let Some(entry) = entries.remove(&uuid) {
                    removed.push(PresenceExpiry {
                        uuid,
                        connection_id: entry.connection_id,
                    });
                }
            }
            removed
        };

        let mut confirmed = Vec::with_capacity(expired.len());
        for expiry in expired {
            if self.hub.clear_presence(&expiry.uuid, expiry.connection_id) {
                // A live WebSocket keeps the client online; only emit the
                // offline event when no other presence source remains.
                if !self.hub.is_ws_connected(&expiry.uuid) {
                    self.hub.emit_offline(
                        expiry.uuid.clone(),
                        expiry.connection_id,
                        PresenceSource::HttpPost,
                    );
                }
                confirmed.push(expiry);
            }
        }
        confirmed
    }

    /// Remove one session explicitly (for example when a client is deleted).
    pub fn remove(&self, uuid: &str, connection_id: i64) -> bool {
        let removed = {
            let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            let matches = entries
                .get(uuid)
                .is_some_and(|entry| entry.connection_id == connection_id);
            if matches {
                entries.remove(uuid);
            }
            matches
        };
        if removed {
            self.hub.clear_presence(uuid, connection_id);
            if !self.hub.is_ws_connected(uuid) {
                self.hub
                    .emit_offline(uuid, connection_id, PresenceSource::HttpPost);
            }
        }
        removed
    }

    pub fn connection_id(&self, uuid: &str) -> Option<i64> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(uuid)
            .map(|entry| entry.connection_id)
    }

    pub fn active_count(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}
