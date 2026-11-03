//! Database-backed notifier tests.

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use komari_db::models::{LoadNotification, Record};
use komari_db::{Db, LocalTime};
use komari_notify::notifier::load::execute_load_task;
use komari_notify::notifier::offline::OfflineNotifier;
use komari_notify::{NotificationService, NotificationSettings};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
}

fn enabled_db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db.config().set("notification_enabled", &true).unwrap();
    db
}

#[test]
fn offline_config_row_is_created_with_go_defaults() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let client = db.clients().create_named("offline").unwrap();

    let notifier = OfflineNotifier::new(&db);
    assert!(
        notifier
            .notification_config(&client.uuid)
            .unwrap()
            .is_none()
    );
    let row = notifier.ensure_offline_config(&client.uuid).unwrap();
    assert_eq!(row.grace_period, 180);
    assert!(!row.enable);
}

#[test]
fn offline_notification_requires_both_switches() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let client = db.clients().create_named("offline").unwrap();

    let notifier = OfflineNotifier::new(&db);
    let mut row = notifier.ensure_offline_config(&client.uuid).unwrap();
    row.enable = true;
    db.notifications().offline_save(&row).unwrap();
    // Global switch is still off.
    assert!(
        notifier
            .notification_config(&client.uuid)
            .unwrap()
            .is_none()
    );

    db.config().set("notification_enabled", &true).unwrap();
    assert!(
        notifier
            .notification_config(&client.uuid)
            .unwrap()
            .is_some()
    );
}

#[test]
fn due_offline_updates_last_notified_with_disconnect_time() {
    let db = enabled_db();
    let client = db.clients().create_named("offline").unwrap();
    let notifier = OfflineNotifier::new(&db);
    let mut row = notifier.ensure_offline_config(&client.uuid).unwrap();
    row.enable = true;
    row.grace_period = 60;
    db.notifications().offline_save(&row).unwrap();

    let mut notifier = OfflineNotifier::new(&db);
    assert_eq!(
        notifier.handle_online(&client.uuid, 7).unwrap(),
        Some(komari_notify::notifier::OnlineDecision::FirstConnection)
    );
    assert!(notifier.handle_offline(&client.uuid, 7, now()).unwrap());

    // Still inside the grace period.
    assert!(
        notifier
            .take_due(now() + TimeDelta::seconds(59))
            .unwrap()
            .is_empty()
    );

    let due = notifier.take_due(now() + TimeDelta::seconds(60)).unwrap();
    assert_eq!(due.len(), 1);
    notifier.mark_notified(&due[0].0).unwrap();

    let stored = db
        .notifications()
        .offline_get(&client.uuid)
        .unwrap()
        .unwrap();
    assert_eq!(stored.last_notified.timestamp(), Some(now().timestamp()));
    // Emitted once only.
    assert!(
        notifier
            .take_due(now() + TimeDelta::seconds(120))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn load_task_emits_event_and_updates_cooldown() {
    let db = enabled_db();
    let client = db.clients().create_named("loaded").unwrap();
    let second = db.clients().create_named("loaded-2").unwrap();
    // The combined record query clamps its live window against the real clock,
    // so this test must use the current instant as well.
    let now = Utc::now();

    for uuid in [&client.uuid, &second.uuid] {
        let mut record = Record::new(uuid);
        record.time = LocalTime::from_instant(now - TimeDelta::minutes(1));
        record.cpu = 95.0;
        db.records().insert(&record).unwrap();
    }

    let mut task = LoadNotification::new("cpu high", "cpu");
    task.clients = komari_db::StringArray::new([client.uuid.clone()]);
    task.threshold = 80.0;
    task.ratio = 0.8;
    task.interval = 15;
    db.notifications().load_save(&mut task).unwrap();
    let stored = db.notifications().load_get(task.id).unwrap().unwrap();

    // Mutating the in-memory copy must not overwrite other database columns.
    task.name = "mutated in memory".to_owned();
    task.clients = komari_db::StringArray::new([client.uuid.clone(), second.uuid.clone()]);
    let event = execute_load_task(&db, &mut task, now).unwrap().unwrap();
    assert_eq!(event.event, "Alert");
    assert_eq!(event.clients.len(), 2);
    assert_eq!(event.message, "mutated in memory");
    assert!(!task.last_notified.is_zero());

    let reloaded = db.notifications().load_get(task.id).unwrap().unwrap();
    assert_eq!(reloaded.name, stored.name);
    assert_eq!(reloaded.clients.as_slice(), stored.clients.as_slice());
    assert_eq!(reloaded.metric, stored.metric);
    assert_eq!(reloaded.threshold, stored.threshold);
    assert_eq!(reloaded.ratio, stored.ratio);
    assert_eq!(reloaded.interval, stored.interval);
    assert_eq!(reloaded.last_notified.timestamp(), Some(now.timestamp()));

    // A second run inside the interval is skipped without sending.
    assert!(execute_load_task(&db, &mut task, now).unwrap().is_none());
}

#[test]
fn load_task_without_a_stored_id_still_returns_the_event() {
    let db = enabled_db();
    let client = db.clients().create_named("loaded").unwrap();
    let now = Utc::now();

    let mut record = Record::new(&client.uuid);
    record.time = LocalTime::from_instant(now - TimeDelta::minutes(1));
    record.cpu = 95.0;
    db.records().insert(&record).unwrap();

    let mut task = LoadNotification::new("cpu high", "cpu");
    task.clients = komari_db::StringArray::new([client.uuid]);
    task.threshold = 80.0;
    task.ratio = 0.8;
    task.interval = 15;
    assert_eq!(task.id, 0);

    let event = execute_load_task(&db, &mut task, now).unwrap().unwrap();
    assert_eq!(event.event, "Alert");
}

#[tokio::test]
async fn service_falls_back_to_empty_and_reports_disabled() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    let service = NotificationService::from_db(&db).await.unwrap();
    assert_eq!(service.sender().unwrap().name(), "empty");
    assert!(!service.send_text("hello", "title").await.unwrap());
}

#[tokio::test]
async fn service_selects_the_configured_provider() {
    let db = Db::open_in_memory().unwrap();
    db.migrate().unwrap();
    db.message_senders()
        .save(&komari_db::models::MessageSenderProvider::new(
            "bark",
            r#"{"device_key":"x"}"#,
        ))
        .unwrap();
    db.config().set("notification_method", &"bark").unwrap();

    let service = NotificationService::from_db(&db).await.unwrap();
    assert_eq!(service.sender().unwrap().name(), "bark");
    assert_eq!(service.settings().template, komari_notify::DEFAULT_TEMPLATE);
    assert!(!NotificationSettings::from_db(&db).unwrap().enabled);
}
