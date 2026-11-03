//! Expiry notifications (Go `utils/notifier/expire.go`).

use chrono::{DateTime, TimeDelta, Utc};
use komari_db::models::{Client, EventMessage, message_event};
use komari_db::{Db, LocalTime};

use crate::error::Result;

/// One client that will expire inside the lead window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpireAlert {
    pub client_id: String,
    pub name: String,
    pub days_left: i64,
}

/// Result of one expiry check.
#[derive(Clone, Debug)]
pub struct ExpireCheck {
    pub clients: Vec<ExpireAlert>,
    pub event: EventMessage,
}

/// Evaluate clients that expire between `now` and `now + lead_days`.
///
/// Returns `None` when expiry notifications are disabled or no client matches.
pub fn check_expire(db: &Db, now: DateTime<Utc>) -> Result<Option<ExpireCheck>> {
    let enabled = db
        .config()
        .get_as::<bool>("expire_notification_enabled")?
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }
    let lead_days = db
        .config()
        .get_as::<i64>("expire_notification_lead_days")?
        .unwrap_or(7);

    let check_time = LocalTime::from_instant(now);
    let threshold = LocalTime::from_instant(now + TimeDelta::days(lead_days));

    let mut alerts = Vec::new();
    for client in db.clients().list()? {
        if client.expired_at.is_zero() || client.expired_at < check_time {
            continue;
        }
        if client.expired_at <= threshold {
            alerts.push(ExpireAlert {
                client_id: client.uuid.clone(),
                name: client.name.clone(),
                days_left: days_left(&client, now),
            });
        }
    }

    if alerts.is_empty() {
        return Ok(None);
    }

    let mut message = String::new();
    for alert in &alerts {
        message.push_str(&format!("• {} ({}d)\n", alert.name, alert.days_left));
    }

    Ok(Some(ExpireCheck {
        clients: alerts,
        event: EventMessage {
            event: message_event::EXPIRE.to_owned(),
            clients: Vec::new(),
            time: check_time,
            message,
            emoji: "⏳".to_owned(),
        },
    }))
}

/// Days left rounded up, matching `math.Ceil(remainingDuration.Hours()/24)`.
fn days_left(client: &Client, now: DateTime<Utc>) -> i64 {
    // Go subtracts two `time.Time` values (nanosecond resolution) and applies
    // math.Ceil to Duration.Hours()/24. Use the wall-clock pair so a duration
    // that is an exact multiple of 24h remains exact.
    let now_naive = LocalTime::from_instant(now).to_naive();
    let remaining = client.expired_at.to_naive() - now_naive;
    let Some(nanos) = remaining.num_nanoseconds() else {
        return 0;
    };
    if nanos <= 0 {
        return 0;
    }
    (nanos as f64 / 86_400_000_000_000.0).ceil() as i64
}

/// Next local 09:00 boundary used by Go's daily expiry loop.
pub fn next_run(now: DateTime<Utc>) -> DateTime<Utc> {
    let wall_now = LocalTime::from_instant(now).to_naive();
    let today_nine_wall = wall_now
        .date()
        .and_hms_opt(9, 0, 0)
        .expect("09:00 is a valid time");
    let today_nine = LocalTime::from_naive(today_nine_wall)
        .timestamp()
        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
        .unwrap_or(now);
    next_run_from(now, today_nine)
}

/// Apply Go's rule to an already-resolved local 09:00 instant:
/// `if now.After(next) { next = next.Add(24 * time.Hour) }`.
///
/// The addition is an absolute 24 hours, so crossing a DST boundary changes
/// the wall-clock hour, exactly like Go.
pub fn next_run_from(now: DateTime<Utc>, today_nine: DateTime<Utc>) -> DateTime<Utc> {
    if now > today_nine {
        today_nine + TimeDelta::hours(24)
    } else {
        today_nine
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single().unwrap()
    }

    fn db_with_expiry(days: i64, enabled: bool, lead_days: i64) -> (Db, String) {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let mut client = Client::new("expiring");
        client.expired_at = LocalTime::from_instant(now() + TimeDelta::hours(days * 24 + 1));
        db.clients().create(&client).unwrap();
        db.config()
            .set("expire_notification_enabled", &enabled)
            .unwrap();
        db.config()
            .set("expire_notification_lead_days", &lead_days)
            .unwrap();
        (db, client.uuid)
    }

    #[test]
    fn disabled_returns_nothing() {
        let (db, _) = db_with_expiry(3, false, 7);
        assert!(check_expire(&db, now()).unwrap().is_none());
    }

    #[test]
    fn inside_lead_window_is_reported_with_ceiled_days() {
        let (db, uuid) = db_with_expiry(3, true, 7);
        let check = check_expire(&db, now()).unwrap().unwrap();
        assert_eq!(check.clients.len(), 1);
        assert_eq!(check.clients[0].client_id, uuid);
        assert_eq!(check.clients[0].days_left, 4);
        assert!(check.event.message.contains("(4d)"));
        assert_eq!(check.event.emoji, "⏳");
    }

    #[test]
    fn outside_lead_window_or_already_expired_is_ignored() {
        let (db, _) = db_with_expiry(10, true, 7);
        assert!(check_expire(&db, now()).unwrap().is_none());

        let (db, _) = db_with_expiry(-1, true, 7);
        assert!(check_expire(&db, now()).unwrap().is_none());
    }

    #[test]
    fn days_left_uses_nanosecond_precision() {
        let base = now();
        let mut client = Client::new("expiring");
        client.expired_at = LocalTime::from_instant(base + TimeDelta::days(3));
        assert_eq!(days_left(&client, base), 3);

        client.expired_at =
            LocalTime::from_instant(base + TimeDelta::days(3) + TimeDelta::nanoseconds(1));
        assert_eq!(days_left(&client, base), 4);
    }

    #[test]
    fn next_run_after_nine_adds_an_absolute_day() {
        let today_nine = Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap();
        let after_nine = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        assert_eq!(
            next_run_from(after_nine, today_nine),
            Utc.with_ymd_and_hms(2026, 1, 2, 9, 0, 0).unwrap()
        );
        let before_nine = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();
        assert_eq!(next_run_from(before_nine, today_nine), today_nine);
    }

    #[test]
    fn next_run_crosses_dst_as_an_absolute_duration() {
        // 13:00Z is 09:00 EDT on the US fall-back day; the next run is
        // 13:00Z the following day (08:00 EST), exactly 24 hours later.
        let today_nine = Utc.with_ymd_and_hms(2026, 11, 1, 13, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 14, 0, 0).unwrap();
        assert_eq!(
            next_run_from(now, today_nine),
            today_nine + TimeDelta::hours(24)
        );
    }

    #[test]
    fn next_run_is_in_the_future() {
        let after_nine = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        assert!(next_run(after_nine) > after_nine);
    }
}
