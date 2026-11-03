//! Round-trip tests for the custom SQL types.

use komari_db::{Db, LocalTime, StringArray, time::timezone_is_utc};

fn scratch_db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.with_connection(|conn| {
        conn.execute(
            "CREATE TABLE scratch_values (id INTEGER PRIMARY KEY, ts, arr longtext)",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    db
}

#[test]
fn local_time_round_trips_through_sqlite() {
    let db = scratch_db();
    let ts = LocalTime::parse("2026-06-23 01:00:31.6481286").unwrap();

    db.with_connection(|conn| {
        conn.execute("INSERT INTO scratch_values (id, ts) VALUES (1, ?1)", [ts])?;
        Ok(())
    })
    .unwrap();

    let loaded: LocalTime = db
        .with_connection(|conn| {
            Ok(
                conn.query_row("SELECT ts FROM scratch_values WHERE id = 1", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(loaded, ts);
    assert_eq!(loaded.to_storage_string(), "2026-06-23 01:00:31.6481286");
}

#[test]
fn zero_local_time_is_stored_as_null() {
    let db = scratch_db();
    db.with_connection(|conn| {
        conn.execute(
            "INSERT INTO scratch_values (id, ts) VALUES (1, ?1)",
            [LocalTime::default()],
        )?;
        Ok(())
    })
    .unwrap();

    let (is_null, loaded): (bool, LocalTime) = db
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT ts IS NULL, ts FROM scratch_values WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert!(is_null);
    assert!(loaded.is_zero());
}

#[test]
fn string_array_round_trips_as_json() {
    let db = scratch_db();
    let array = StringArray::new(["alpha", "beta"]);

    db.with_connection(|conn| {
        conn.execute(
            "INSERT INTO scratch_values (id, arr) VALUES (1, ?1)",
            [array.clone()],
        )?;
        Ok(())
    })
    .unwrap();

    let (raw, loaded): (String, StringArray) = db
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT arr, arr FROM scratch_values WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(raw, r#"["alpha","beta"]"#);
    assert_eq!(loaded, array);
    assert_eq!(loaded.as_slice(), &["alpha".to_owned(), "beta".to_owned()]);
}

#[test]
fn string_array_reads_go_nil_text() {
    let db = scratch_db();
    db.with_connection(|conn| {
        conn.execute(
            "INSERT INTO scratch_values (id, arr) VALUES (1, 'null')",
            [],
        )?;
        conn.execute("INSERT INTO scratch_values (id, arr) VALUES (2, NULL)", [])?;
        Ok(())
    })
    .unwrap();

    for id in [1, 2] {
        let loaded: StringArray = db
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT arr FROM scratch_values WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert!(loaded.is_empty(), "row {id}");
    }
}

#[test]
fn local_time_parsing_accepts_every_go_layout() {
    let cases = [
        ("2026-06-23 01:00:31.6481286", "2026-06-23 01:00:31.6481286"),
        ("2026-06-23 01:00:31", "2026-06-23 01:00:31.0000000"),
        ("2026-06-23", "2026-06-23 00:00:00.0000000"),
        ("2026-06-23T01:00:31Z", "2026-06-23 01:00:31.0000000"),
        ("2026-06-23T01:00:31+08:00", "2026-06-23 01:00:31.0000000"),
        (
            "2026-06-23 01:01:32.488977903+00:00",
            "2026-06-23 01:01:32.4889779",
        ),
    ];
    for (input, expected) in cases {
        let parsed = LocalTime::parse(input).unwrap_or_else(|err| panic!("{input}: {err}"));
        assert_eq!(parsed.to_storage_string(), expected, "input {input}");
    }
}

#[test]
fn local_time_json_matches_go_semantics() {
    let zero = LocalTime::default();
    assert_eq!(serde_json::to_string(&zero).unwrap(), "null");

    let timestamp = LocalTime::parse("2026-06-23 01:00:31").unwrap();
    let json = serde_json::to_string(&timestamp).unwrap();
    assert!(json.starts_with("\"2026-06-23T01:00:31"));
    if timezone_is_utc() {
        assert!(json.ends_with("Z\""));
    }
    let decoded: LocalTime = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, timestamp);

    let decoded: LocalTime = serde_json::from_str("null").unwrap();
    assert!(decoded.is_zero());
}

#[test]
fn unix_timestamp_helpers_are_consistent() {
    let timestamp = LocalTime::from_unix_timestamp(1_782_180_031).expect("valid timestamp");
    assert!(!timestamp.is_zero());
    assert_eq!(timestamp.timestamp(), Some(1_782_180_031));
}

#[test]
fn from_instant_converts_to_the_application_timezone() {
    // 01:00 at +08:00 is an absolute instant; conversion must preserve it
    // regardless of whether TZ is UTC or a named zone.
    let source = chrono::DateTime::parse_from_rfc3339("2026-06-23T01:00:31+08:00").unwrap();
    let converted = LocalTime::from_datetime(source);
    assert_eq!(converted.timestamp(), Some(source.timestamp()));
    assert_eq!(
        converted.add_seconds(0).timestamp(),
        Some(source.timestamp())
    );
}

#[test]
fn adding_seconds_uses_absolute_time() {
    let start = LocalTime::parse("2026-01-15 12:00:00").unwrap();
    let later = start.add_seconds(3600);
    assert_eq!(
        later.timestamp().unwrap() - start.timestamp().unwrap(),
        3600
    );

    let (now, later) = LocalTime::now_and_after(3600);
    assert_eq!((later - now).num_seconds(), 3600);

    let session = komari_db::models::Session::new("user", 3600, "ua", "127.0.0.1", "password");
    let expires_instant = session.expires_instant.expect("absolute expiry");
    let remaining = (expires_instant - chrono::Utc::now()).num_seconds();
    assert!((3598..=3600).contains(&remaining));
    assert!(!session.is_expired());
}

#[test]
fn autumn_fold_is_preserved_by_instant_conversion() {
    if std::env::var("TZ").ok().as_deref() != Some("America/New_York") {
        eprintln!("TZ is not America/New_York; skipping fall-back assertion");
        return;
    }

    // 2026-11-01: 05:30 UTC = 01:30 EDT, 06:30 UTC = 01:30 EST.
    let before = chrono::DateTime::parse_from_rfc3339("2026-11-01T05:30:00Z").unwrap();
    let after = chrono::DateTime::parse_from_rfc3339("2026-11-01T06:30:00Z").unwrap();
    let before_local = LocalTime::from_instant(before);
    let after_local = LocalTime::from_instant(after);
    assert_eq!(
        before_local.to_storage_string(),
        "2026-11-01 01:30:00.0000000"
    );
    assert_eq!(
        after_local.to_storage_string(),
        "2026-11-01 01:30:00.0000000"
    );

    // Adding one absolute hour to the EDT instant lands on the EST instant,
    // whose wall clock is also 01:30 (the fold is retained).
    let before_utc = before.with_timezone(&chrono::Utc);
    let (_, later_absolute) = LocalTime::absolute_after(before_utc, 3600);
    assert_eq!((later_absolute - before_utc).num_seconds(), 3600);
    assert_eq!(
        LocalTime::from_instant(later_absolute).to_storage_string(),
        after_local.to_storage_string()
    );
    // The DB stores wall clocks only, so both sides format as 01:30; the
    // absolute expiry kept on `Session::expires_instant` retains the 3600s
    // duration for in-process expiry checks.
    assert_eq!(after.timestamp() - before.timestamp(), 3600);
}

#[test]
fn dst_boundary_is_handled_when_the_host_uses_a_dst_zone() {
    // The assertion only makes sense with a known DST timezone. CI normally
    // runs with TZ unset (UTC) and skips; developers can run
    // `TZ=America/New_York cargo test -p komari-db dst_boundary`.
    if std::env::var("TZ").ok().as_deref() != Some("America/New_York") {
        eprintln!("TZ is not America/New_York; skipping DST assertion");
        return;
    }
    // 2026-03-08 is the US DST start. 48 absolute hours after noon on the 7th
    // is 13:00 local on the 9th, not 12:00.
    let start = LocalTime::parse("2026-03-07 12:00:00").unwrap();
    let later = start.add_seconds(48 * 3600);
    assert_eq!(later.to_storage_string(), "2026-03-09 13:00:00.0000000");
}
