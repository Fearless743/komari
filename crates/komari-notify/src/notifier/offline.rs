//! Offline/online notification state machine (Go `utils/notifier/offline.go`).

use std::collections::HashMap;

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::Db;
use komari_db::models::{Client, OfflineNotification};

use crate::error::Result;

const DEFAULT_GRACE_SECONDS: i64 = 5 * 60;

#[derive(Clone, Debug)]
pub struct ClientState {
    pending_offline_since: Option<DateTime<Utc>>,
    is_first_connection: bool,
    is_conn_exist: bool,
    connection_id: i64,
    grace: TimeDelta,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            pending_offline_since: None,
            is_first_connection: true,
            is_conn_exist: false,
            connection_id: 0,
            grace: TimeDelta::seconds(DEFAULT_GRACE_SECONDS),
        }
    }
}

/// Result of an online transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineDecision {
    /// First connection after process start: never notify.
    FirstConnection,
    /// The client reconnected inside its grace period: no notification.
    ReconnectedWithinGrace,
    /// A connection is already marked as alive: no notification.
    AlreadyConnected,
    /// Client came back after the offline notification was delivered.
    Notify,
}

/// A due offline notification returned by [`OfflineTracker::take_due`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueOffline {
    pub client_id: String,
    pub connection_id: i64,
    /// Time the disconnect was observed; Go writes this to `last_notified`.
    pub scheduled_at: DateTime<Utc>,
}

/// Pure per-client state machine, extracted from Go so thresholds can be
/// tested with an injected clock.
#[derive(Default)]
pub struct OfflineTracker {
    states: HashMap<String, ClientState>,
}

impl OfflineTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self, client_id: &str) -> Option<&ClientState> {
        self.states.get(client_id)
    }

    pub fn has_pending(&self, client_id: &str) -> bool {
        self.states
            .get(client_id)
            .is_some_and(|state| state.pending_offline_since.is_some())
    }

    /// Go `OfflineNotification` state update. Returns `true` when a pending
    /// notification was scheduled.
    pub fn on_offline(
        &mut self,
        client_id: &str,
        connection_id: i64,
        now: DateTime<Utc>,
        grace_seconds: i64,
    ) -> bool {
        let grace = if grace_seconds <= 0 {
            TimeDelta::seconds(DEFAULT_GRACE_SECONDS)
        } else {
            TimeDelta::seconds(grace_seconds)
        };
        let state = self.states.entry(client_id.to_owned()).or_default();
        state.grace = grace;

        if state.pending_offline_since.is_some() || state.connection_id != connection_id {
            return false;
        }
        state.pending_offline_since = Some(now);
        true
    }

    /// Return and clear every pending notification whose grace period elapsed.
    pub fn take_due(&mut self, now: DateTime<Utc>) -> Vec<DueOffline> {
        let mut due = Vec::new();
        for (client_id, state) in &mut self.states {
            let Some(since) = state.pending_offline_since else {
                continue;
            };
            if now < since + state.grace {
                continue;
            }
            state.pending_offline_since = None;
            state.is_conn_exist = false;
            due.push(DueOffline {
                client_id: client_id.clone(),
                connection_id: state.connection_id,
                scheduled_at: since,
            });
        }
        due
    }

    /// Go `OnlineNotification` state transitions.
    pub fn on_online(&mut self, client_id: &str, connection_id: i64) -> OnlineDecision {
        let state = self.states.entry(client_id.to_owned()).or_default();
        state.connection_id = connection_id;

        if state.is_first_connection {
            state.is_first_connection = false;
            state.pending_offline_since = None;
            state.is_conn_exist = true;
            return OnlineDecision::FirstConnection;
        }

        let was_pending = state.pending_offline_since.take().is_some();
        if was_pending {
            return OnlineDecision::ReconnectedWithinGrace;
        }
        if state.is_conn_exist {
            return OnlineDecision::AlreadyConnected;
        }

        state.is_conn_exist = true;
        OnlineDecision::Notify
    }
}

/// Database-backed offline notifier.
pub struct OfflineNotifier<'a> {
    db: &'a Db,
    tracker: OfflineTracker,
}

impl<'a> OfflineNotifier<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self {
            db,
            tracker: OfflineTracker::new(),
        }
    }

    pub fn tracker(&self) -> &OfflineTracker {
        &self.tracker
    }

    /// Fetch or create the `offline_notifications` row for a client.
    pub fn ensure_offline_config(&self, client_id: &str) -> Result<OfflineNotification> {
        if let Some(config) = self.db.notifications().offline_get(client_id)? {
            return Ok(config);
        }
        let config = OfflineNotification::new(client_id);
        self.db.notifications().offline_save(&config)?;
        Ok(config)
    }

    /// Go `getNotificationConfig`: global switch plus the per-client enable.
    pub fn notification_config(&self, client_id: &str) -> Result<Option<OfflineNotification>> {
        let globally_enabled = self
            .db
            .config()
            .get_as::<bool>("notification_enabled")?
            .unwrap_or(false);
        if !globally_enabled {
            return Ok(None);
        }

        let config = self.ensure_offline_config(client_id)?;
        Ok(config.enable.then_some(config))
    }

    /// Handle a disconnect. Returns `true` when a notification was scheduled.
    pub fn handle_offline(
        &mut self,
        client_id: &str,
        connection_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        if self.db.clients().get(client_id)?.is_none() {
            return Ok(false);
        }
        let Some(config) = self.notification_config(client_id)? else {
            return Ok(false);
        };
        let grace = if config.grace_period <= 0 {
            DEFAULT_GRACE_SECONDS
        } else {
            config.grace_period
        };
        Ok(self
            .tracker
            .on_offline(client_id, connection_id, now, grace))
    }

    /// Return due notifications together with their client records.
    pub fn take_due(&mut self, now: DateTime<Utc>) -> Result<Vec<(DueOffline, Client)>> {
        let mut result = Vec::new();
        for due in self.tracker.take_due(now) {
            if let Some(client) = self.db.clients().get(&due.client_id)? {
                result.push((due, client));
            }
        }
        Ok(result)
    }

    /// Handle a connect. `None` means the client is unknown or notifications
    /// are disabled (Go returns without touching state in both cases).
    pub fn handle_online(
        &mut self,
        client_id: &str,
        connection_id: i64,
    ) -> Result<Option<OnlineDecision>> {
        if self.db.clients().get(client_id)?.is_none() {
            return Ok(None);
        }
        if self.notification_config(client_id)?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.tracker.on_online(client_id, connection_id)))
    }

    /// Record the delivery time used by a due notification.
    pub fn mark_notified(&self, due: &DueOffline) -> Result<()> {
        let at = komari_db::LocalTime::from_instant(due.scheduled_at);
        self.db
            .notifications()
            .offline_set_last_notified(&due.client_id, at)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap() + TimeDelta::seconds(seconds)
    }

    #[test]
    fn first_online_is_silent() {
        let mut tracker = OfflineTracker::new();
        assert_eq!(tracker.on_online("a", 1), OnlineDecision::FirstConnection);
    }

    #[test]
    fn reconnect_inside_grace_cancels_offline() {
        let mut tracker = OfflineTracker::new();
        assert_eq!(tracker.on_online("a", 1), OnlineDecision::FirstConnection);
        assert!(tracker.on_offline("a", 1, at(0), 180));
        assert_eq!(
            tracker.on_online("a", 2),
            OnlineDecision::ReconnectedWithinGrace
        );
        assert!(tracker.take_due(at(1000)).is_empty());
    }

    #[test]
    fn due_notification_waits_for_grace_and_is_emitted_once() {
        let mut tracker = OfflineTracker::new();
        tracker.on_online("a", 1);
        assert!(tracker.on_offline("a", 1, at(0), 180));
        assert!(tracker.take_due(at(179)).is_empty());
        let due = tracker.take_due(at(180));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].client_id, "a");
        assert!(tracker.take_due(at(360)).is_empty());
        assert_eq!(tracker.on_online("a", 2), OnlineDecision::Notify);
    }

    #[test]
    fn stale_connection_id_does_not_schedule() {
        let mut tracker = OfflineTracker::new();
        tracker.on_online("a", 5);
        assert!(!tracker.on_offline("a", 4, at(0), 10));
        assert!(!tracker.has_pending("a"));
    }
}
