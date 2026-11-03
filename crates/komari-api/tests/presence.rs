mod common;

use std::sync::Arc;
use std::time::Duration;

use komari_api::{ClientEventKind, ManualClock, PresenceSource, PresenceTracker, WsHub};
use tokio::sync::broadcast::error::TryRecvError;

fn tracker() -> (Arc<ManualClock>, Arc<WsHub>, PresenceTracker) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let hub = Arc::new(WsHub::new(clock.clone()));
    let presence = PresenceTracker::new(clock.clone(), Duration::from_secs(11), hub.clone());
    (clock, hub, presence)
}

#[test]
fn post_presence_refreshes_ttl_and_expires() {
    let (clock, hub, presence) = tracker();
    let mut events = hub.subscribe();

    let first = presence.refresh("client-1");
    assert!(first.newly_online);
    assert!(hub.is_online("client-1"));
    assert_eq!(hub.online_uuids(), vec!["client-1".to_owned()]);
    let event = events.try_recv().expect("online event");
    assert_eq!(event.kind, ClientEventKind::Online);
    assert_eq!(event.source, PresenceSource::HttpPost);
    assert_eq!(event.connection_id, first.connection_id);

    clock.advance(5_000);
    let second = presence.refresh("client-1");
    assert!(!second.newly_online);
    assert_eq!(second.connection_id, first.connection_id);
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));

    clock.advance(10_999);
    assert!(presence.expire_due().is_empty());
    assert!(hub.is_online("client-1"));

    clock.advance(1);
    // The hub already filters expired entries even before the sweep.
    assert!(!hub.is_online("client-1"));
    let expired = presence.expire_due();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].uuid, "client-1");
    assert_eq!(expired[0].connection_id, first.connection_id);
    assert!(hub.online_uuids().is_empty());

    let event = events.try_recv().expect("offline event");
    assert_eq!(event.kind, ClientEventKind::Offline);
    assert_eq!(event.source, PresenceSource::HttpPost);
    assert_eq!(event.connection_id, first.connection_id);
    assert_eq!(presence.active_count(), 0);
}

#[test]
fn removing_presence_requires_the_current_connection() {
    let (_clock, hub, presence) = tracker();
    let mut events = hub.subscribe();
    let update = presence.refresh("client-1");
    let _ = events.try_recv();

    assert!(!presence.remove("client-1", update.connection_id + 1));
    assert!(hub.is_online("client-1"));
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));

    assert!(presence.remove("client-1", update.connection_id));
    assert!(!hub.is_online("client-1"));
    let event = events.try_recv().expect("offline event");
    assert_eq!(event.kind, ClientEventKind::Offline);
}

#[test]
fn hub_presence_merge_and_clear() {
    let (clock, hub, _presence) = tracker();
    hub.keep_alive_presence("post-only", 7, Duration::from_secs(11));
    assert!(hub.is_online("post-only"));
    assert_eq!(hub.online_uuids(), vec!["post-only".to_owned()]);

    assert!(!hub.clear_presence("post-only", 8));
    assert!(hub.is_online("post-only"));
    assert!(hub.clear_presence("post-only", 7));
    assert!(!hub.is_online("post-only"));

    clock.advance(60_000);
    assert!(!hub.is_online("post-only"));
}
