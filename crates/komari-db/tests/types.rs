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
