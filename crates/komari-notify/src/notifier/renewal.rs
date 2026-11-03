//! Automatic billing-cycle renewal (Go `utils/renewal`).
//!
//! Go renews a client when it is online, `auto_renewal` is enabled, the expiry
//! date is today or in the past, and the billing cycle is non-zero. Long-term
//! (more than 100 years) and zero expiry dates are ignored.

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveDateTime, Utc};
use komari_db::models::{Client, EventMessage, message_event};
use komari_db::{ClientPatch, Db, LocalTime};

use crate::error::Result;

/// Check one client and renew it when the Go conditions are met.
///
/// `online` must be the WebSocket-connected state (HTTP-only presence does not
/// renew, matching `ws.GetConnectedClients()` in Go). Returns the `Renew`
/// event when the expiry date was advanced.
pub fn check_and_auto_renewal(
    db: &Db,
    client: &Client,
    online: bool,
    now: DateTime<Utc>,
) -> Result<Option<EventMessage>> {
    if !client.auto_renewal || !online {
        return Ok(None);
    }
    if client.expired_at.is_zero() {
        return Ok(None);
    }

    let now_local = LocalTime::from_instant(now).to_naive();
    let expiry = client.expired_at.to_naive();
    if expiry.year() < 2 {
        return Ok(None);
    }

    // A date more than 100 years away is a lifetime/one-off purchase.
    let Some(hundred_years) = add_months(now_local.date(), 100 * 12) else {
        return Ok(None);
    };
    if expiry > hundred_years.and_time(now_local.time()) {
        return Ok(None);
    }

    // Renew when already expired or expiring today.
    let expiring_today = expiry.date() == now_local.date();
    if expiry >= now_local && !expiring_today {
        return Ok(None);
    }
    if client.billing_cycle <= 0 {
        return Ok(None);
    }

    // A very old expiry starts from now instead of producing another stale
    // date, matching Go's `baseTime = now` branch.
    let stale = now_local
        .checked_sub_signed(chrono::TimeDelta::days(30))
        .is_some_and(|threshold| expiry < threshold);
    let base = if stale { now_local } else { expiry };
    let Some(new_date) = advance(base.date(), client.billing_cycle) else {
        return Ok(None);
    };
    let new_expiry = new_date.and_time(base.time());
    let new_local = LocalTime::from_naive(new_expiry);

    db.clients().patch(
        &client.uuid,
        &ClientPatch {
            expired_at: Some(new_local),
            ..ClientPatch::default()
        },
    )?;

    let date = new_date.format("%Y-%m-%d");
    let _ = db.logs().log(
        "",
        "",
        format!("Auto-renewed client: {} until {date}", client.name),
        "renewal",
    );

    Ok(Some(EventMessage {
        event: message_event::RENEW.to_owned(),
        clients: vec![client.clone()],
        time: LocalTime::from_instant(now),
        message: format!("• {} until {date}\n", client.name),
        emoji: "🔄".to_owned(),
    }))
}

/// Apply Go's `time.AddDate` month/year semantics.
///
/// Go normalizes overflowing days (Jan 31 + 1 month is Mar 3), while chrono's
/// `checked_add_months` clamps to the last day. Build the first day of the
/// target month and add `day - 1` days to reproduce Go.
fn add_months(date: NaiveDate, months: i32) -> Option<NaiveDate> {
    let total = i64::from(date.year()) * 12 + i64::from(date.month0()) + i64::from(months);
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = u32::try_from(total.rem_euclid(12)).ok()? + 1;
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    first.checked_add_days(Days::new(u64::from(date.day() - 1)))
}

fn advance(date: NaiveDate, billing_cycle: i64) -> Option<NaiveDate> {
    match billing_cycle {
        27..=32 => add_months(date, 1),
        87..=95 => add_months(date, 3),
        175..=185 => add_months(date, 6),
        360..=370 => add_months(date, 12),
        720..=750 => add_months(date, 24),
        1080..=1150 => add_months(date, 36),
        1800..=1850 => add_months(date, 60),
        days if days > 0 => date.checked_add_days(Days::new(days as u64)),
        _ => None,
    }
}

/// Days left until expiry, used by tests and callers that want a display value.
pub fn days_left(expiry: NaiveDateTime, now: NaiveDateTime) -> i64 {
    if expiry <= now {
        return 0;
    }
    let duration = expiry - now;
    (duration.num_seconds() + 86_399) / 86_400
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use komari_db::models::Client;

    fn fixture(billing_cycle: i64) -> (Db, Client) {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let mut client = Client::new("renew");
        client.auto_renewal = true;
        client.billing_cycle = billing_cycle;
        db.clients().create(&client).unwrap();
        (db, client)
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 31, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn expiry_before_now() -> LocalTime {
        let now_local = LocalTime::from_instant(now()).to_naive();
        LocalTime::from_naive(
            (now_local.date() - Days::new(10))
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        )
    }

    fn stored_expiry(db: &Db, uuid: &str) -> LocalTime {
        db.clients().require(uuid).unwrap().expired_at
    }

    #[test]
    fn monthly_quarterly_and_yearly_cycles_use_calendar_arithmetic() {
        for (cycle, months) in [(30, 1), (90, 3), (365, 12)] {
            let (db, mut client) = fixture(cycle);
            client.expired_at = expiry_before_now();
            db.clients().update(&client).unwrap();
            let event = check_and_auto_renewal(&db, &client, true, now())
                .unwrap()
                .expect("renewed");
            let expected = add_months(client.expired_at.to_naive().date(), months).unwrap();
            assert_eq!(
                stored_expiry(&db, &client.uuid).to_naive().date(),
                expected,
                "cycle {cycle}"
            );
            assert!(
                event
                    .message
                    .contains(&expected.format("%Y-%m-%d").to_string())
            );
        }
    }

    #[test]
    fn day_based_cycle_adds_days() {
        let (db, mut client) = fixture(45);
        client.expired_at = expiry_before_now();
        db.clients().update(&client).unwrap();
        check_and_auto_renewal(&db, &client, true, now()).unwrap();
        let expected = client
            .expired_at
            .to_naive()
            .date()
            .checked_add_days(Days::new(45))
            .unwrap();
        assert_eq!(stored_expiry(&db, &client.uuid).to_naive().date(), expected);
    }

    #[test]
    fn only_online_clients_are_renewed() {
        let (db, mut client) = fixture(30);
        client.expired_at = expiry_before_now();
        db.clients().update(&client).unwrap();
        let before = stored_expiry(&db, &client.uuid);
        assert!(
            check_and_auto_renewal(&db, &client, false, now())
                .unwrap()
                .is_none()
        );
        assert_eq!(stored_expiry(&db, &client.uuid), before);
    }

    #[test]
    fn same_day_expiry_is_renewed() {
        let (db, mut client) = fixture(30);
        let now_local = LocalTime::from_instant(now()).to_naive();
        client.expired_at = LocalTime::from_naive(now_local);
        db.clients().update(&client).unwrap();
        assert!(
            check_and_auto_renewal(&db, &client, true, now())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn lifetime_and_disabled_clients_are_ignored() {
        let (db, mut client) = fixture(30);
        let now_local = LocalTime::from_instant(now()).to_naive();
        client.expired_at = LocalTime::from_naive(
            add_months(now_local.date(), 100 * 12 + 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        );
        db.clients().update(&client).unwrap();
        assert!(
            check_and_auto_renewal(&db, &client, true, now())
                .unwrap()
                .is_none()
        );

        client.auto_renewal = false;
        client.expired_at = expiry_before_now();
        db.clients().update(&client).unwrap();
        assert!(
            check_and_auto_renewal(&db, &client, true, now())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn january_31_plus_one_month_matches_go_normalization() {
        let date = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(
            add_months(date, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 3).unwrap()
        );
    }

    #[test]
    fn days_left_rounds_up() {
        let now =
            NaiveDateTime::parse_from_str("2026-01-01 00:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let expiry = now + chrono::TimeDelta::hours(25);
        assert_eq!(days_left(expiry, now), 2);
    }
}
