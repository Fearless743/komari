//! Cancellable background work started by the HTTP server.
//!
//! Go starts these loops from `cmd/server.go`:
//!
//! * record retention + compaction and audit-log cleanup (30 minutes);
//! * traffic/load notification checks (one minute);
//! * daily expiry notifications at 09:00 local time;
//! * online/offline notification state driven by the WebSocket hub;
//! * DDNS synchronisation (`ddns_sync_interval`, default ten minutes);
//! * a static-asset configuration refresh (theme/site/temp share token).
//!
//! Every task is owned by [`BackgroundTasks`], so graceful shutdown can cancel
//! and join them. Failures are logged; a failing pass never panics the server.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures_util::FutureExt;
use komari_config::keys::{DDNS_SYNC_INTERVAL_KEY, RECORD_ENABLED_KEY};
use komari_db::models::{EventMessage, message_event};
use komari_db::{Db, LocalTime};
use komari_ddns::DdnsService;
use komari_jobs::CancelToken;
use komari_notify::NotificationService;
use komari_notify::notifier::offline::OfflineNotifier;
use komari_notify::notifier::traffic::{TrafficTracker, TrafficUsage};
use komari_notify::notifier::{
    OnlineDecision, check_expire, check_traffic, next_run, run_all_load_tasks,
};
use komari_web::WebAssets;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::ClientEventKind;
use crate::server::assets::refresh_web_assets;
use crate::state::ApiState;

/// Scheduling knobs for [`BackgroundTasks`].
#[derive(Clone, Debug)]
pub struct BackgroundConfig {
    /// Master switch. Tests usually disable it.
    pub enabled: bool,
    /// Record retention/compaction interval (Go: 30 minutes).
    pub record_maintenance_interval: Duration,
    /// Traffic and load notification interval (Go: one minute).
    pub notify_interval: Duration,
    /// Static asset configuration refresh interval.
    pub asset_refresh_interval: Duration,
    /// Fallback DDNS interval when `ddns_sync_interval` is unset.
    pub ddns_interval: Duration,
    /// How often pending offline notifications are checked.
    pub offline_check_interval: Duration,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            record_maintenance_interval: Duration::from_secs(30 * 60),
            notify_interval: Duration::from_secs(60),
            asset_refresh_interval: Duration::from_secs(60),
            ddns_interval: Duration::from_secs(10 * 60),
            offline_check_interval: Duration::from_secs(5),
        }
    }
}

/// Owns every task started by the server assembly.
pub struct BackgroundTasks {
    cancel: CancelToken,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for BackgroundTasks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundTasks")
            .field("cancelled", &self.is_cancelled())
            .field("tasks", &self.task_count())
            .finish()
    }
}

impl BackgroundTasks {
    /// Start the production loops.
    pub fn start(
        state: &ApiState,
        assets: Arc<WebAssets>,
        ddns: Arc<DdnsService>,
        config: &BackgroundConfig,
    ) -> Self {
        let tasks = Self {
            cancel: CancelToken::new(),
            handles: Mutex::new(Vec::new()),
        };
        if !config.enabled {
            return tasks;
        }

        tasks.spawn_record_maintenance(state.db.clone(), config);
        tasks.spawn_record_retention(state.db.clone());
        tasks.spawn_asset_refresh(assets, state.db.clone(), config);
        tasks.spawn_ddns_sync(ddns, state.db.clone(), config);
        tasks.spawn_notification_tick(state.clone(), config);
        tasks.spawn_offline_notifications(state.clone(), config);
        tasks.spawn_expire_notifications(state.db.clone(), config);
        tasks
    }

    /// An empty task set (used by tests that only exercise routing).
    pub fn disabled() -> Self {
        Self {
            cancel: CancelToken::new(),
            handles: Mutex::new(Vec::new()),
        }
    }

    pub fn task_count(&self) -> usize {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Request cancellation without waiting.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Cancel and wait for every task, bounded by five seconds per task.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let handles = {
            let mut handles = self
                .handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *handles)
        };
        for handle in handles {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
    }

    fn spawn<F>(&self, name: &'static str, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let handle = tokio::spawn(async move {
            tracing::info!(task = name, "background task started");
            if AssertUnwindSafe(future).catch_unwind().await.is_err() {
                tracing::error!(task = name, "background task panicked");
            }
            tracing::debug!(task = name, "background task stopped");
        });
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(handle);
    }

    fn spawn_record_maintenance(&self, db: Arc<Db>, config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        let interval = config.record_maintenance_interval;
        self.spawn("record-maintenance", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        // Go calls CompactRecord() once at startup and then on
                        // every 30-minute tick; interval() also fires first
                        // immediately, so this matches the startup pass.
                        match komari_jobs::run_once(&db, Utc::now()) {
                            Ok(report) => tracing::debug!(
                                cleanup = ?report.cleanup,
                                compaction = ?report.compaction,
                                "record maintenance completed"
                            ),
                            Err(error) => {
                                tracing::error!(%error, "record maintenance failed");
                            }
                        }
                        if let Err(error) = db.logs().remove_old(30) {
                            tracing::warn!(%error, "failed to remove old audit logs");
                        }
                    }
                }
            }
        });
    }

    /// Go deletes every record once a minute while `record_enabled` is false.
    fn spawn_record_retention(&self, db: Arc<Db>) {
        let cancel = self.cancel.clone();
        self.spawn("record-retention", async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let enabled = db
                            .config()
                            .get_as::<bool>(RECORD_ENABLED_KEY)
                            .ok()
                            .flatten()
                            .unwrap_or(true);
                        if !enabled {
                            if let Err(error) = db.records().delete_all() {
                                tracing::warn!(%error, "failed to clear records while recording is disabled");
                            }
                        }
                    }
                }
            }
        });
    }

    fn spawn_asset_refresh(&self, assets: Arc<WebAssets>, db: Arc<Db>, config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        let interval = config.asset_refresh_interval;
        self.spawn("asset-refresh", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Startup already refreshed once.
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        if let Err(error) = refresh_web_assets(&assets, &db) {
                            tracing::warn!(%error, "failed to refresh web assets");
                        }
                    }
                }
            }
        });
    }

    fn spawn_ddns_sync(&self, ddns: Arc<DdnsService>, db: Arc<Db>, config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        let fallback = config.ddns_interval;
        self.spawn("ddns-sync", async move {
            loop {
                let interval = db
                    .config()
                    .get_as::<i64>(DDNS_SYNC_INTERVAL_KEY)
                    .ok()
                    .flatten()
                    .filter(|minutes| *minutes > 0)
                    .map(|minutes| Duration::from_secs(minutes as u64 * 60))
                    .unwrap_or(fallback);
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {}
                }
                match db.clients().list() {
                    Ok(clients) => ddns.sync_all(&clients, "schedule", false).await,
                    Err(error) => tracing::warn!(%error, "failed to list clients for DDNS"),
                }
            }
        });
    }

    fn spawn_notification_tick(&self, state: ApiState, config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        let interval = config.notify_interval;
        self.spawn("notification-tick", async move {
            let mut tracker = TrafficTracker::new();
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let now = Utc::now();
                        let reports: HashMap<String, TrafficUsage> = state
                            .hub
                            .latest_reports()
                            .into_iter()
                            .map(|(uuid, report)| {
                                (
                                    uuid,
                                    TrafficUsage {
                                        total_up: report.network.total_up,
                                        total_down: report.network.total_down,
                                    },
                                )
                            })
                            .collect();

                        let mut events = Vec::new();
                        match check_traffic(&state.db, &reports, &mut tracker, now) {
                            Ok(alerts) => events.extend(alerts.into_iter().map(|alert| alert.event)),
                            Err(error) => {
                                tracing::warn!(%error, "traffic notification check failed");
                            }
                        }
                        match run_all_load_tasks(&state.db, now) {
                            Ok(mut load_events) => events.append(&mut load_events),
                            Err(error) => {
                                tracing::warn!(%error, "load notification check failed");
                            }
                        }
                        deliver_all(&state.db, &events).await;
                    }
                }
            }
        });
    }

    fn spawn_offline_notifications(&self, state: ApiState, config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        let interval = config.offline_check_interval;
        self.spawn("offline-notifications", async move {
            let db = state.db.clone();
            let mut notifier = OfflineNotifier::new(&db);
            let mut events = state.hub.subscribe();
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    event = events.recv() => match event {
                        Ok(event) => {
                            match event.kind {
                                ClientEventKind::Online => {
                                    match notifier.handle_online(&event.uuid, event.connection_id) {
                                        Ok(Some(OnlineDecision::Notify)) => {
                                            if let Ok(Some(client)) = db.clients().get(&event.uuid) {
                                                deliver(
                                                    &db,
                                                    &EventMessage {
                                                        event: message_event::ONLINE.to_owned(),
                                                        clients: vec![client],
                                                        time: LocalTime::now(),
                                                        message: String::new(),
                                                        emoji: "🟢".to_owned(),
                                                    },
                                                )
                                                .await;
                                            }
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            tracing::warn!(%error, "online notification check failed");
                                        }
                                    }
                                }
                                ClientEventKind::Offline => {
                                    if let Err(error) = notifier.handle_offline(
                                        &event.uuid,
                                        event.connection_id,
                                        Utc::now(),
                                    ) {
                                        tracing::warn!(%error, "offline notification check failed");
                                    }
                                }
                            }
                        }
                        Err(RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "offline notification task lagged");
                        }
                        Err(RecvError::Closed) => break,
                    },
                    _ = ticker.tick() => {
                        match notifier.take_due(Utc::now()) {
                            Ok(due) => {
                                for (due, client) in due {
                                    deliver(
                                        &db,
                                        &EventMessage {
                                            event: message_event::OFFLINE.to_owned(),
                                            clients: vec![client],
                                            time: LocalTime::now(),
                                            message: String::new(),
                                            emoji: "🔴".to_owned(),
                                        },
                                    )
                                    .await;
                                    if let Err(error) = notifier.mark_notified(&due) {
                                        tracing::warn!(%error, "failed to record offline notification");
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "failed to collect due offline notifications");
                            }
                        }
                    }
                }
            }
        });
    }

    fn spawn_expire_notifications(&self, db: Arc<Db>, _config: &BackgroundConfig) {
        let cancel = self.cancel.clone();
        self.spawn("expire-notifications", async move {
            loop {
                let now = Utc::now();
                let delay = (next_run(now) - now)
                    .to_std()
                    .unwrap_or(Duration::from_secs(60));
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                match check_expire(&db, Utc::now()) {
                    Ok(Some(check)) => deliver(&db, &check.event).await,
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "expiry notification check failed"),
                }
            }
        });
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        self.cancel.cancel();
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for handle in handles.iter() {
            handle.abort();
        }
    }
}

async fn deliver_all(db: &Db, events: &[EventMessage]) {
    if events.is_empty() {
        return;
    }
    match NotificationService::from_db(db).await {
        Ok(service) => {
            for event in events {
                if let Err(error) = service.send_event(event).await {
                    tracing::warn!(%error, event = event.event, "failed to send notification");
                }
            }
        }
        Err(error) => tracing::warn!(%error, "failed to load notification provider"),
    }
}

async fn deliver(db: &Db, event: &EventMessage) {
    match NotificationService::from_db(db).await {
        Ok(service) => {
            if let Err(error) = service.send_event(event).await {
                tracing::warn!(%error, event = event.event, "failed to send notification");
            }
        }
        Err(error) => tracing::warn!(%error, "failed to load notification provider"),
    }
}
