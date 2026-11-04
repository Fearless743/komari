//! Core CRUD coverage for every repository.

mod support;

use std::collections::HashMap;

use serde::Deserialize;

use komari_db::models::{
    Client, Clipboard, DdnsSyncHistory, GPURecord, LoadNotification, Log, OfflineNotification,
    Provider, Record, User,
};
use komari_db::{
    AggregateFn, ClientPatch, ClipboardPatch, LocalTime, RecordMetric, RecordTable, StringArray,
};
use support::memory_db;

fn recent(seconds_ago: i64) -> LocalTime {
    let now = LocalTime::now().timestamp().expect("current time");
    LocalTime::from_unix_timestamp(now - seconds_ago).expect("valid timestamp")
}

#[test]
fn accounts_check_password_accepts_bcrypt_rows_read_only() {
    let db = memory_db();
    let mut user = User::new("legacy-rust", "ignored");
    user.passwd = "$2a$10$C4Gwv0BDoZO0LFJLCMwD/OcQy61qVqTGBbvcnPh0FiTT408C69pv.".to_owned();
    db.accounts().insert(&user).unwrap();

    assert_eq!(
        db.accounts()
            .check_password("legacy-rust", "secret")
            .unwrap(),
        Some(user.uuid.clone())
    );
    assert_eq!(
        db.accounts()
            .check_password("legacy-rust", "wrong")
            .unwrap(),
        None
    );
    assert!(
        db.accounts()
            .require_by_username("legacy-rust")
            .unwrap()
            .check_password("secret")
    );
}

#[test]
fn clients_crud_and_lookup() {
    let db = memory_db();
    let clients = db.clients();

    let client = clients.create_named("web-1").unwrap();
    assert_eq!(client.name, "web-1");
    assert_eq!(client.currency, "$");
    assert_eq!(client.ddns_record_type, "inherit");
    assert_eq!(client.traffic_limit_type, "max");
    assert_eq!(client.token.len(), 22);

    // A hand-built row gets GORM's database defaults and timestamps.
    let plain = Client {
        uuid: "plain-uuid".to_owned(),
        token: "plain-token".to_owned(),
        ..Client::default()
    };
    clients.create(&plain).unwrap();
    let plain = clients.require("plain-uuid").unwrap();
    assert_eq!(plain.currency, "$");
    assert_eq!(plain.ddns_record_type, "inherit");
    assert_eq!(plain.traffic_limit_type, "max");
    assert!(!plain.created_at.is_zero());
    assert_eq!(clients.count().unwrap(), 2);
    clients.delete("plain-uuid").unwrap();

    assert_eq!(
        clients.get(&client.uuid).unwrap().unwrap().uuid,
        client.uuid
    );
    assert_eq!(
        clients.get_by_token(&client.token).unwrap().unwrap().uuid,
        client.uuid
    );
    assert!(clients.get("missing").unwrap().is_none());
    assert!(clients.require("missing").is_err());

    let updated = clients
        .patch(
            &client.uuid,
            &ClientPatch {
                name: Some("web-2".to_owned()),
                remark: Some("rack 4".to_owned()),
                hidden: Some(true),
                price: Some(12.5),
                traffic_limit: Some(1024),
                ..ClientPatch::default()
            },
        )
        .unwrap();
    assert_eq!(updated, 1);

    let loaded = clients.require(&client.uuid).unwrap();
    assert_eq!(loaded.name, "web-2");
    assert_eq!(loaded.remark, "rack 4");
    assert!(loaded.hidden);
    assert_eq!(loaded.price, 12.5);
    assert_eq!(loaded.traffic_limit, 1024);

    clients.rename(&client.uuid, "web-3").unwrap();
    clients.set_token(&client.uuid, "new-token").unwrap();
    let loaded = clients.require(&client.uuid).unwrap();
    assert_eq!(loaded.name, "web-3");
    assert_eq!(loaded.token, "new-token");

    assert_eq!(clients.list().unwrap().len(), 1);
    assert_eq!(clients.count().unwrap(), 1);

    assert!(clients.delete(&client.uuid).unwrap());
    assert!(!clients.delete(&client.uuid).unwrap());
    assert_eq!(clients.count().unwrap(), 0);
}

#[test]
fn client_full_update_round_trips() {
    let db = memory_db();
    let clients = db.clients();
    let mut client = clients.create_named("old").unwrap();
    client.name = "new".to_owned();
    client.cpu_cores = 8;
    client.mem_total = 16 * 1024 * 1024 * 1024;
    client.group = "prod".to_owned();
    client.tags = "a;b".to_owned();
    client.expired_at = recent(-30 * 24 * 3600);
    assert_eq!(clients.update(&client).unwrap(), 1);

    let loaded = clients.require(&client.uuid).unwrap();
    assert_eq!(loaded.name, "new");
    assert_eq!(loaded.group, "prod");
    assert_eq!(loaded.cpu_cores, 8);
    assert_eq!(loaded.tags, "a;b");
    assert_eq!(loaded.expired_at, client.expired_at);
}

#[test]
fn accounts_create_check_and_change_password() {
    let db = memory_db();
    let accounts = db.accounts();

    let user = accounts.create("admin", "secret").unwrap();
    assert_eq!(user.username, "admin");
    assert_eq!(accounts.count().unwrap(), 1);
    assert_eq!(
        accounts.check_password("admin", "secret").unwrap(),
        Some(user.uuid.clone())
    );
    assert_eq!(accounts.check_password("admin", "wrong").unwrap(), None);
    assert_eq!(accounts.check_password("nobody", "secret").unwrap(), None);

    assert!(accounts.get_by_uuid(&user.uuid).unwrap().is_some());
    assert_eq!(
        accounts.get_by_username("admin").unwrap().unwrap().uuid,
        user.uuid
    );
    assert!(accounts.require_by_username("missing").is_err());

    accounts.bind_sso(&user.uuid, "github:42").unwrap();
    assert_eq!(
        accounts.get_by_sso("github:42").unwrap().unwrap().uuid,
        user.uuid
    );
    accounts.unbind_sso(&user.uuid).unwrap();
    assert!(accounts.get_by_sso("github:42").unwrap().is_none());

    accounts.set_password(&user.uuid, "new-secret").unwrap();
    assert_eq!(
        accounts.check_password("admin", "new-secret").unwrap(),
        Some(user.uuid.clone())
    );
    accounts.set_password_by_username("admin", "third").unwrap();
    assert!(accounts.check_password("admin", "third").unwrap().is_some());

    accounts.enable_2fa(&user.uuid, "SECRET").unwrap();
    assert_eq!(
        accounts
            .get_by_uuid(&user.uuid)
            .unwrap()
            .unwrap()
            .two_factor,
        "SECRET"
    );
    accounts.disable_2fa(&user.uuid).unwrap();
    assert!(
        accounts
            .get_by_uuid(&user.uuid)
            .unwrap()
            .unwrap()
            .two_factor
            .is_empty()
    );

    accounts
        .update_profile(&user.uuid, Some("root"), Some("github"))
        .unwrap();
    let loaded = accounts.require_by_uuid(&user.uuid).unwrap();
    assert_eq!(loaded.username, "root");
    assert_eq!(loaded.sso_type, "github");

    let inserted = User::new("second", "pw");
    accounts.insert(&inserted).unwrap();
    assert_eq!(accounts.list().unwrap().len(), 2);
    assert!(accounts.delete_by_username("second").unwrap());
    assert!(accounts.delete(&user.uuid).unwrap());
}

#[test]
fn sessions_create_validate_expire_and_cleanup() {
    let db = memory_db();
    let user = db.accounts().create("admin", "pw").unwrap();
    let sessions = db.sessions();

    let session = sessions
        .create(&user.uuid, 3600, "agent", "127.0.0.1", "password")
        .unwrap();
    assert_eq!(session.session.len(), 32);
    assert_eq!(
        sessions.validate(&session.session).unwrap().as_deref(),
        Some(user.uuid.as_str())
    );
    assert_eq!(
        sessions
            .user_by_session(&session.session)
            .unwrap()
            .unwrap()
            .uuid,
        user.uuid
    );

    sessions
        .update_latest(&session.session, "agent/2", "10.0.0.1")
        .unwrap();
    let loaded = sessions.get(&session.session).unwrap().unwrap();
    assert_eq!(loaded.latest_user_agent, "agent/2");
    assert_eq!(loaded.latest_ip, "10.0.0.1");

    let expired = sessions
        .create(&user.uuid, -10, "agent", "127.0.0.1", "password")
        .unwrap();
    assert!(sessions.validate(&expired.session).unwrap().is_none());
    assert!(sessions.get(&expired.session).unwrap().is_none());

    assert_eq!(sessions.list().unwrap().len(), 1);
    assert_eq!(sessions.list_for_user(&user.uuid).unwrap().len(), 1);
    assert!(sessions.delete(&session.session).unwrap());
    assert_eq!(sessions.count().unwrap(), 0);

    sessions.create(&user.uuid, 3600, "a", "b", "c").unwrap();
    assert_eq!(sessions.delete_all().unwrap(), 1);
}

#[test]
fn sessions_delete_expired_is_scoped() {
    let db = memory_db();
    let user = db.accounts().create("admin", "pw").unwrap();
    let sessions = db.sessions();
    let live = sessions.create(&user.uuid, 3600, "a", "b", "c").unwrap();
    let dead = sessions.create(&user.uuid, -10, "a", "b", "c").unwrap();

    assert_eq!(sessions.delete_expired().unwrap(), 1);
    assert!(sessions.get(&dead.session).unwrap().is_none());
    assert!(sessions.get(&live.session).unwrap().is_some());
    assert_eq!(sessions.delete_for_user(&user.uuid).unwrap(), 1);
}

#[test]
fn records_insert_query_latest_and_aggregate() {
    let db = memory_db();
    let records = db.records();
    let client = db.clients().create_named("node").unwrap();

    let mut first = Record::new(&client.uuid);
    first.time = recent(120);
    first.cpu = 10.0;
    first.ram = 100;
    let mut second = Record::new(&client.uuid);
    second.time = recent(60);
    second.cpu = 20.0;
    second.ram = 200;
    let mut third = Record::new(&client.uuid);
    third.time = recent(0);
    third.cpu = 30.0;
    third.ram = 300;

    records
        .insert_batch(&[first.clone(), second, third.clone()])
        .unwrap();
    assert_eq!(records.count().unwrap(), 3);

    let range = records
        .by_client_and_time(&client.uuid, recent(180), recent(-10))
        .unwrap();
    assert_eq!(range.len(), 3);
    assert_eq!(range[0].time, first.time);

    let narrow = records
        .by_client_and_time(&client.uuid, recent(90), recent(-10))
        .unwrap();
    assert_eq!(narrow.len(), 2);

    let latest = records.latest(&client.uuid).unwrap().unwrap();
    assert_eq!(latest.cpu, 30.0);
    assert!(records.latest("missing").unwrap().is_none());

    let start = recent(180);
    let end = recent(-10);
    let avg = records
        .aggregate(
            &client.uuid,
            start,
            end,
            RecordMetric::Cpu,
            AggregateFn::Avg,
        )
        .unwrap()
        .unwrap();
    assert!((avg - 20.0).abs() < 0.001);
    let max = records
        .aggregate(
            &client.uuid,
            start,
            end,
            RecordMetric::Ram,
            AggregateFn::Max,
        )
        .unwrap()
        .unwrap();
    assert_eq!(max, 300.0);
    let count = records
        .aggregate(
            &client.uuid,
            start,
            end,
            RecordMetric::Cpu,
            AggregateFn::Count,
        )
        .unwrap()
        .unwrap();
    assert_eq!(count, 3.0);
    assert_eq!(records.count_in_range(&client.uuid, start, end).unwrap(), 3);

    // Long-term table is independent and queryable through the same API.
    records.insert_into(RecordTable::LongTerm, &third).unwrap();
    assert_eq!(
        records
            .by_client_and_time_in(RecordTable::LongTerm, &client.uuid, start, end)
            .unwrap()
            .len(),
        1
    );
    assert!(
        records
            .latest_in(RecordTable::LongTerm, &client.uuid)
            .unwrap()
            .is_some()
    );

    assert!(records.delete_before(recent(90)).unwrap() >= 1);
    assert_eq!(records.count_in_range(&client.uuid, start, end).unwrap(), 2);
    assert!(records.delete_for_client(&client.uuid).unwrap() >= 2);
    records.insert(&third).unwrap();
    records.delete_all().unwrap();
    assert_eq!(records.count().unwrap(), 0);
}

#[test]
fn records_combined_merges_long_term_and_downsamples_recent() {
    let db = memory_db();
    let records = db.records();
    let client = db.clients().create_named("combined").unwrap();

    // Anchor three live samples in a completed 15-minute bucket so the
    // grouping is deterministic regardless of when the test runs.
    let now = LocalTime::now().timestamp().unwrap();
    let quarter = ((now - 900) / 900) * 900;
    let at = |offset: i64| LocalTime::from_unix_timestamp(quarter + offset).unwrap();

    let mut first = Record::new(&client.uuid);
    first.time = at(60);
    first.cpu = 10.0;
    let mut second = Record::new(&client.uuid);
    second.time = at(120);
    second.cpu = 20.0;
    let mut third = Record::new(&client.uuid);
    third.time = at(180);
    third.cpu = 30.0;
    records
        .insert_batch(&[first, second, third.clone()])
        .unwrap();

    let mut long_term = Record::new(&client.uuid);
    long_term.time = at(-1800);
    long_term.cpu = 99.0;
    records
        .insert_into(RecordTable::LongTerm, &long_term)
        .unwrap();

    let start = at(-7200);
    let end = LocalTime::now();
    let combined = records
        .combined_by_client_and_time(&client.uuid, start, end)
        .unwrap();
    assert_eq!(combined.len(), 2, "recent bucket + one long-term row");
    // The newest sample in the bucket survives; long-term rows are appended.
    assert_eq!(combined[0].cpu, 30.0);
    assert_eq!(combined[0].time, third.time);
    assert_eq!(combined[1].cpu, 99.0);
    assert_eq!(combined[1].time, long_term.time);

    // The explicit live-only API still returns all raw rows.
    assert_eq!(
        records
            .live_by_client_and_time(&client.uuid, start, end)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn gpu_combined_merges_live_and_long_term_without_downsampling() {
    let db = memory_db();
    let records = db.records();
    let client = db.clients().create_named("gpu-combined").unwrap();

    let now = LocalTime::now().timestamp().unwrap();
    let quarter = ((now - 900) / 900) * 900;
    let at = |offset: i64| LocalTime::from_unix_timestamp(quarter + offset).unwrap();

    let live_one = GPURecord {
        client: client.uuid.clone(),
        time: at(60),
        device_index: 1,
        device_name: "GPU A".to_owned(),
        utilization: 10.0,
        ..GPURecord::default()
    };
    let live_two = GPURecord {
        client: client.uuid.clone(),
        time: at(120),
        device_index: 0,
        device_name: "GPU B".to_owned(),
        utilization: 20.0,
        ..GPURecord::default()
    };
    let long_term = GPURecord {
        client: client.uuid.clone(),
        time: at(-1800),
        device_index: 2,
        device_name: "GPU C".to_owned(),
        utilization: 30.0,
        ..GPURecord::default()
    };
    records
        .insert_gpu_batch(&[live_one.clone(), live_two.clone()])
        .unwrap();
    records
        .insert_gpu_into(RecordTable::LongTerm, &long_term)
        .unwrap();

    let start = at(-7200);
    let end = LocalTime::now();
    let combined = records
        .combined_gpu_by_client_and_time(&client.uuid, start, end)
        .unwrap();
    assert_eq!(combined.len(), 3);
    assert_eq!(combined[0].time, live_one.time);
    assert_eq!(combined[1].time, live_two.time);
    assert_eq!(combined[2].time, long_term.time);
    assert_eq!(
        records
            .live_gpu_by_client_and_time(&client.uuid, start, end)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn gpu_records_round_trip() {
    let db = memory_db();
    let records = db.records();
    let client = db.clients().create_named("gpu-node").unwrap();

    let mut record = GPURecord {
        client: client.uuid.clone(),
        time: recent(10),
        device_index: 1,
        device_name: "RTX 4090".to_owned(),
        mem_total: 24 * 1024 * 1024 * 1024,
        mem_used: 8 * 1024 * 1024 * 1024,
        utilization: 42.5,
        temperature: 61,
    };
    records.insert_gpu(&record).unwrap();
    record.device_index = 0;
    record.time = recent(5);
    record.utilization = 10.0;
    records.insert_gpu(&record).unwrap();

    let list = records
        .gpu_by_client_and_time(&client.uuid, recent(60), recent(-1))
        .unwrap();
    assert_eq!(list.len(), 2);
    // Ordered by time ASC, then device index.
    assert_eq!(list[0].device_index, 1);
    assert_eq!(list[0].device_name, "RTX 4090");

    let latest = records.latest_gpu(&client.uuid, 0).unwrap().unwrap();
    assert_eq!(latest.utilization, 10.0);
}

#[test]
fn config_get_set_typed_and_defaults() {
    let db = memory_db();
    let config = db.config();

    assert!(config.get("sitename").unwrap().is_none());
    config.set("sitename", "Komari").unwrap();
    assert_eq!(
        config.get_raw("sitename").unwrap().as_deref(),
        Some("\"Komari\"")
    );
    assert_eq!(
        config.get_as::<String>("sitename").unwrap().as_deref(),
        Some("Komari")
    );
    assert_eq!(
        config.get("sitename").unwrap().unwrap(),
        serde_json::json!("Komari")
    );
    assert!(config.contains("sitename").unwrap());

    assert_eq!(
        config.get_as_or("record_preserve_time", 720_i64).unwrap(),
        720
    );
    // The default must have been persisted.
    assert_eq!(
        config.get_as::<i64>("record_preserve_time").unwrap(),
        Some(720)
    );

    assert_eq!(
        config
            .get_or("theme", serde_json::json!("default"))
            .unwrap(),
        serde_json::json!("default")
    );

    let mut many = HashMap::new();
    many.insert("a".to_owned(), serde_json::json!(1));
    many.insert("b".to_owned(), serde_json::json!({"x": true}));
    config.set_many(&many).unwrap();
    assert_eq!(config.get_many(&["a", "b", "c"]).unwrap().len(), 2);
    let all = config.get_all().unwrap();
    assert_eq!(all.get("a"), Some(&serde_json::json!(1)));

    assert_eq!(config.count().unwrap(), 5);
    assert!(config.remove("a").unwrap());
    assert!(!config.remove("a").unwrap());
}

#[test]
fn config_get_as_uses_go_style_loose_conversions() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("float_as_int", "80.0").unwrap();
    assert_eq!(config.get_as::<i64>("float_as_int").unwrap(), Some(80));
    config.set_raw("fraction_as_int", "80.9").unwrap();
    assert_eq!(config.get_as::<i64>("fraction_as_int").unwrap(), Some(80));

    config.set_raw("int_as_float", "80").unwrap();
    assert_eq!(
        config.get_as::<f64>("int_as_float").unwrap(),
        Some(80.0_f64)
    );

    config.set_raw("null_value", "null").unwrap();
    assert_eq!(config.get_as::<i64>("null_value").unwrap(), Some(0));
    assert_eq!(
        config.get_as::<String>("null_value").unwrap(),
        Some(String::new())
    );
    assert_eq!(config.get_as::<bool>("null_value").unwrap(), Some(false));

    // `get_as_or` also uses the loose conversion for existing values.
    assert_eq!(config.get_as_or("float_as_int", 0_i64).unwrap(), 80_i64);

    // Go's JSON decoder turns a base64 string into []byte.
    config.set("bytes", "aGk=").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("bytes").unwrap(),
        Some(b"hi".to_vec())
    );
}

#[test]
fn config_get_as_wraps_narrow_integers_like_go_reflect() {
    let db = memory_db();
    let config = db.config();

    // Go decodes the generic value as float64, truncates to int64, then
    // `reflect.SetInt` narrows modulo 2^bits.
    config.set_raw("i8_float", "300.0").unwrap();
    assert_eq!(config.get_as::<i8>("i8_float").unwrap(), Some(44));
    config.set_raw("i8_integer", "300").unwrap();
    assert_eq!(config.get_as::<i8>("i8_integer").unwrap(), Some(44));
    config.set_raw("i16", "65535.0").unwrap();
    assert_eq!(config.get_as::<i16>("i16").unwrap(), Some(-1));
    config.set_raw("i32", "4294967296.0").unwrap();
    assert_eq!(config.get_as::<i32>("i32").unwrap(), Some(0));

    config.set_raw("u8", "256.0").unwrap();
    assert_eq!(config.get_as::<u8>("u8").unwrap(), Some(0));
    config.set_raw("u16", "65537.0").unwrap();
    assert_eq!(config.get_as::<u16>("u16").unwrap(), Some(1));
    config.set_raw("u32", "4294967296.0").unwrap();
    assert_eq!(config.get_as::<u32>("u32").unwrap(), Some(0));
}

#[test]
fn config_get_as_wraps_negative_values_to_unsigned_like_go() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("u8", "-1.0").unwrap();
    assert_eq!(config.get_as::<u8>("u8").unwrap(), Some(u8::MAX));
    config.set_raw("u16", "-2.0").unwrap();
    assert_eq!(config.get_as::<u16>("u16").unwrap(), Some(u16::MAX - 1));
    config.set_raw("u32", "-1.0").unwrap();
    assert_eq!(config.get_as::<u32>("u32").unwrap(), Some(u32::MAX));
    config.set_raw("u64", "-1.0").unwrap();
    assert_eq!(config.get_as::<u64>("u64").unwrap(), Some(u64::MAX));
}

#[test]
fn config_get_as_matches_go_for_out_of_range_floats() {
    let db = memory_db();
    let config = db.config();

    // Go's amd64 float->int conversion yields i64::MIN for out-of-range
    // values, and uint64 conversion yields 2^63.
    config.set_raw("i64", "1e20").unwrap();
    assert_eq!(config.get_as::<i64>("i64").unwrap(), Some(i64::MIN));
    config.set_raw("i8", "1e20").unwrap();
    assert_eq!(config.get_as::<i8>("i8").unwrap(), Some(0));
    config.set_raw("u64", "1e20").unwrap();
    assert_eq!(config.get_as::<u64>("u64").unwrap(), Some(1_u64 << 63));
    config.set_raw("u8", "1e20").unwrap();
    assert_eq!(config.get_as::<u8>("u8").unwrap(), Some(0));
}

#[test]
fn config_get_as_keeps_in_range_integers_exact() {
    let db = memory_db();
    let config = db.config();

    // 2^53 + 1 is not representable as f64, but Go's initial
    // `json.Unmarshal` into int64 keeps it exact.
    config.set_raw("i64", "9007199254740993").unwrap();
    assert_eq!(
        config.get_as::<i64>("i64").unwrap(),
        Some(9_007_199_254_740_993)
    );
    config.set_raw("u64", "18446744073709551615").unwrap();
    assert_eq!(config.get_as::<u64>("u64").unwrap(), Some(u64::MAX));
}

#[test]
fn config_get_as_truncates_negative_floats_toward_zero() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("i64", "-1.9").unwrap();
    assert_eq!(config.get_as::<i64>("i64").unwrap(), Some(-1));
    config.set_raw("i8", "-1.9").unwrap();
    assert_eq!(config.get_as::<i8>("i8").unwrap(), Some(-1));
    config.set_raw("u64", "-0.5").unwrap();
    assert_eq!(config.get_as::<u64>("u64").unwrap(), Some(0));
}

#[test]
fn config_get_as_converts_numbers_to_f32_and_f64() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("f32", "1.5").unwrap();
    assert_eq!(config.get_as::<f32>("f32").unwrap(), Some(1.5_f32));
    config.set_raw("f64", "18446744073709551615").unwrap();
    assert_eq!(
        config.get_as::<f64>("f64").unwrap(),
        Some(18_446_744_073_709_551_616.0)
    );
}

#[test]
fn config_get_as_decodes_base64_for_byte_targets() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("bytes", "\"aGVsbG8=\"").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("bytes").unwrap(),
        Some(b"hello".to_vec())
    );
    config.set_raw("null_bytes", "null").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("null_bytes").unwrap(),
        Some(Vec::new())
    );
    // Go falls back to reflect's string->[]byte conversion when base64
    // decoding fails, so invalid input is kept verbatim.
    config.set_raw("raw_bytes", "\"not base64!\"").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("raw_bytes").unwrap(),
        Some(b"not base64!".to_vec())
    );
    // Only []byte targets reinterpret strings as bytes.
    assert!(config.get_as::<Vec<i64>>("raw_bytes").is_err());
}

#[derive(Debug, Default, Deserialize, PartialEq)]
struct LooseStruct {
    count: i64,
    ratio: f64,
    small: i8,
}

#[test]
fn config_get_as_remarshals_compound_integral_floats_like_go() {
    let db = memory_db();
    let config = db.config();

    // Go's first strict unmarshal fails on 80.0 into an integer field; the
    // generic remarshal turns it into `80`, so the second unmarshal succeeds.
    config
        .set_raw("struct", r#"{"count":80.0,"ratio":80,"small":80.0}"#)
        .unwrap();
    assert_eq!(
        config.get_as::<LooseStruct>("struct").unwrap(),
        Some(LooseStruct {
            count: 80,
            ratio: 80.0,
            small: 80,
        })
    );

    config.set_raw("slice", "[80.0,81.0]").unwrap();
    assert_eq!(
        config.get_as::<Vec<i64>>("slice").unwrap(),
        Some(vec![80, 81])
    );

    config.set_raw("map", r#"{"a":80.0,"b":81.0}"#).unwrap();
    let mut expected = HashMap::new();
    expected.insert("a".to_owned(), 80_i64);
    expected.insert("b".to_owned(), 81_i64);
    assert_eq!(
        config.get_as::<HashMap<String, i64>>("map").unwrap(),
        Some(expected)
    );

    // Fractions and out-of-range narrow integers still fail after remarshal.
    config
        .set_raw("fraction", r#"{"count":80.5,"ratio":80,"small":80}"#)
        .unwrap();
    assert!(config.get_as::<LooseStruct>("fraction").is_err());
    config
        .set_raw("narrow", r#"{"count":80,"ratio":80,"small":300.0}"#)
        .unwrap();
    assert!(config.get_as::<LooseStruct>("narrow").is_err());
    config.set_raw("slice_fraction", "[80.5]").unwrap();
    assert!(config.get_as::<Vec<i64>>("slice_fraction").is_err());
}

#[test]
fn config_get_as_base64_matches_go_std_encoding() {
    let db = memory_db();
    let config = db.config();

    // Go's base64.StdEncoding ignores CR/LF.
    config.set_raw("crlf", "\"aG\\r\\nk=\"").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("crlf").unwrap(),
        Some(b"hi".to_vec())
    );

    // StdEncoding is non-strict: non-zero trailing bits are accepted.
    config.set_raw("trailing_bits", "\"aGl=\"").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("trailing_bits").unwrap(),
        Some(b"hi".to_vec())
    );

    // Genuinely invalid base64 still falls back to the raw string bytes.
    config.set_raw("invalid", "\"aGk\"").unwrap();
    assert_eq!(
        config.get_as::<Vec<u8>>("invalid").unwrap(),
        Some(b"aGk".to_vec())
    );
}

#[test]
fn config_get_as_still_rejects_incompatible_scalar_types() {
    let db = memory_db();
    let config = db.config();

    config.set_raw("string", "\"not-a-number\"").unwrap();
    assert!(config.get_as::<i64>("string").is_err());
    config.set_raw("bool", "true").unwrap();
    assert!(config.get_as::<i64>("bool").is_err());
    config.set_raw("number", "42").unwrap();
    assert!(config.get_as::<String>("number").is_err());
}

#[test]
fn notifications_offline_and_load() {
    let db = memory_db();
    let client = db.clients().create_named("node").unwrap();
    let notifications = db.notifications();

    let mut offline = OfflineNotification::new(&client.uuid);
    offline.enable = true;
    notifications.offline_save(&offline).unwrap();
    let loaded = notifications.offline_get(&client.uuid).unwrap().unwrap();
    assert!(loaded.enable);
    assert_eq!(loaded.grace_period, 180);

    offline.enable = false;
    offline.grace_period = 60;
    notifications.offline_save(&offline).unwrap();
    assert_eq!(notifications.offline_list().unwrap().len(), 1);
    assert!(
        !notifications
            .offline_get(&client.uuid)
            .unwrap()
            .unwrap()
            .enable
    );
    assert_eq!(notifications.offline_count().unwrap(), 1);
    notifications
        .offline_set_enabled(&client.uuid, true)
        .unwrap();
    notifications
        .offline_set_last_notified(&client.uuid, recent(5))
        .unwrap();
    assert!(notifications.offline_delete(&client.uuid).unwrap());

    let mut rule = LoadNotification::new("high cpu", "cpu");
    rule.clients = StringArray::new([client.uuid.clone()]);
    let id = notifications.load_insert(&rule).unwrap();
    assert!(id > 0);
    let mut loaded = notifications.load_get(id).unwrap().unwrap();
    assert_eq!(loaded.metric, "cpu");
    assert_eq!(
        loaded.clients.as_slice(),
        std::slice::from_ref(&client.uuid)
    );

    loaded.threshold = 95.0;
    assert_eq!(notifications.load_update(&loaded).unwrap(), 1);
    assert_eq!(notifications.load_count().unwrap(), 1);
    assert_eq!(notifications.load_list().unwrap()[0].threshold, 95.0);

    assert_eq!(notifications.load_delete(&[id]).unwrap(), 1);
    assert!(notifications.load_get(id).unwrap().is_none());
}

#[test]
fn provider_tables_are_independent() {
    let db = memory_db();

    db.message_senders()
        .save(&Provider::new("telegram", r#"{"bot_token":""}"#))
        .unwrap();
    db.oauth()
        .save(&Provider::new("github", r#"{"client_id":""}"#))
        .unwrap();
    db.ddns_providers()
        .save(&Provider::new("cloudflare", r#"{"api_token":""}"#))
        .unwrap();

    assert_eq!(db.message_senders().count().unwrap(), 1);
    assert_eq!(db.oauth().count().unwrap(), 1);
    assert_eq!(db.ddns_providers().count().unwrap(), 1);

    let updated = Provider::new("telegram", r#"{"bot_token":"x"}"#);
    db.message_senders().upsert(&updated).unwrap();
    assert_eq!(
        db.message_senders().get("telegram").unwrap().unwrap(),
        updated
    );
    assert_eq!(db.message_senders().list().unwrap()[0].name, "telegram");
    assert!(db.message_senders().delete("telegram").unwrap());
    assert!(db.message_senders().get("telegram").unwrap().is_none());
}

#[test]
fn themes_store_json_configuration() {
    let db = memory_db();
    let themes = db.themes();

    assert_eq!(themes.get_data("default").unwrap(), serde_json::json!({}));
    themes
        .set("default", &serde_json::json!({"primary": "#fff"}))
        .unwrap();
    assert_eq!(
        themes.get_data("default").unwrap()["primary"],
        serde_json::json!("#fff")
    );
    assert_eq!(themes.list().unwrap().len(), 1);
    assert_eq!(themes.count().unwrap(), 1);

    themes.set_raw("broken", "not-json").unwrap();
    assert_eq!(themes.get_data("broken").unwrap(), serde_json::json!({}));

    assert!(themes.delete("default").unwrap());
    assert!(!themes.delete("default").unwrap());
}

#[test]
fn clipboard_crud_and_batch_delete() {
    let db = memory_db();
    let clipboard = db.clipboard();

    let entry = clipboard.create("hello", "greeting").unwrap();
    assert!(entry.id > 0);
    assert_eq!(clipboard.get(entry.id).unwrap().unwrap().text, "hello");

    let mut updated = entry.clone();
    updated.text = "world".to_owned();
    updated.weight = 5;
    assert_eq!(clipboard.update(&updated).unwrap(), 1);

    assert_eq!(
        clipboard
            .patch(
                entry.id,
                &ClipboardPatch {
                    remark: Some("note".to_owned()),
                    ..ClipboardPatch::default()
                },
            )
            .unwrap(),
        1
    );
    let loaded = clipboard.get(entry.id).unwrap().unwrap();
    assert_eq!(loaded.text, "world");
    assert_eq!(loaded.weight, 5);
    assert_eq!(loaded.remark, "note");

    let second = clipboard.insert(&Clipboard::default()).unwrap();
    assert_eq!(clipboard.list().unwrap().len(), 2);
    assert_eq!(clipboard.delete_batch(&[entry.id, second]).unwrap(), 2);
    assert_eq!(clipboard.count().unwrap(), 0);
}

#[test]
fn ddns_history_orders_and_cleans_up() {
    let db = memory_db();
    let ddns = db.ddns();

    let mut old = DdnsSyncHistory::new("client-1", "success");
    old.synced_at = recent(3600);
    old.ipv4 = "1.2.3.4".to_owned();
    let old_id = ddns.save_history(&old).unwrap();

    let mut new = DdnsSyncHistory::new("client-1", "failed");
    new.synced_at = recent(10);
    new.error = "timeout".to_owned();
    let new_id = ddns.save_history(&new).unwrap();
    assert!(new_id > old_id);

    let history = ddns.history("client-1", 10).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, new_id);
    assert_eq!(history[0].status, "failed");
    assert_eq!(history[1].ipv4, "1.2.3.4");

    assert_eq!(ddns.history("", 1).unwrap().len(), 1);
    assert_eq!(ddns.history_count().unwrap(), 2);

    ddns.delete_history_before(recent(60)).unwrap();
    assert_eq!(ddns.history_count().unwrap(), 1);
    assert_eq!(ddns.delete_history_for_client("client-1").unwrap(), 1);
}

#[test]
fn audit_logs_filter_and_retention() {
    let db = memory_db();
    let logs = db.logs();

    logs.log("127.0.0.1", "user", "logged in", "login").unwrap();
    logs.log("", "", "error", "error").unwrap();

    assert_eq!(logs.count().unwrap(), 2);
    assert_eq!(logs.list(10).unwrap().len(), 2);
    assert_eq!(logs.list_by_type("login", 10).unwrap().len(), 1);
    assert_eq!(
        logs.list_by_type("login", 10).unwrap()[0].message,
        "logged in"
    );

    let mut old = Log::new("old", "info");
    old.time = recent(40 * 24 * 3600);
    logs.insert(&old).unwrap();

    assert_eq!(logs.remove_old(30).unwrap(), 1);
    assert_eq!(logs.count().unwrap(), 2);
    assert_eq!(logs.delete_before(recent(-1)).unwrap(), 2);
}

#[test]
fn all_merges_live_and_long_term_records() {
    let db = memory_db();
    let records = db.records();
    let client = db.clients().create_named("all").unwrap();

    let mut live = Record::new(&client.uuid);
    live.time = recent(60);
    live.cpu = 11.0;
    records.insert(&live).unwrap();

    let mut long_term = Record::new(&client.uuid);
    long_term.time = recent(600);
    long_term.cpu = 22.0;
    records
        .insert_into(RecordTable::LongTerm, &long_term)
        .unwrap();

    let all = records.all().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].cpu, 11.0);
    assert_eq!(all[1].cpu, 22.0);

    // The single-table helper remains available.
    assert_eq!(records.all_in(RecordTable::LongTerm).unwrap().len(), 1);
}

#[test]
fn audit_logs_order_and_paginate() {
    let db = memory_db();
    let logs = db.logs();

    for (seconds, message) in [(300, "oldest"), (200, "middle"), (100, "newest")] {
        let mut entry = Log::new(message, "info");
        entry.time = recent(seconds);
        logs.insert(&entry).unwrap();
    }

    let all = logs.list(10).unwrap();
    assert_eq!(
        all.iter()
            .map(|log| log.message.as_str())
            .collect::<Vec<_>>(),
        vec!["newest", "middle", "oldest"]
    );

    let page = logs.list_page(1, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].message, "middle");

    let page = logs.list_page(2, 0).unwrap();
    assert_eq!(
        page.iter()
            .map(|log| log.message.as_str())
            .collect::<Vec<_>>(),
        vec!["newest", "middle"]
    );

    let page = logs.list_by_type_page("info", 1, 2).unwrap();
    assert_eq!(page[0].message, "oldest");
}

#[test]
fn json_field_names_match_the_go_models() {
    let client = Client::new("node");
    let value = serde_json::to_value(&client).unwrap();
    let object = value.as_object().unwrap();
    for key in [
        "uuid",
        "token",
        "name",
        "cpu_name",
        "cpu_cores",
        "mem_total",
        "swap_total",
        "billing_cycle",
        "auto_renewal",
        "expired_at",
        "group",
        "ddns_enabled",
        "ddns_record_type",
        "hidden",
        "traffic_limit",
        "traffic_limit_type",
        "created_at",
        "updated_at",
    ] {
        assert!(object.contains_key(key), "missing JSON key {key}");
    }
    // Go marks these `omitempty`, and a fresh client has empty values.
    assert!(object.get("ipv4").is_none());
    assert!(object.get("ipv6").is_none());
    assert!(object.get("public_remark").is_none());

    let user = User::new("admin", "pw");
    let user = serde_json::to_value(&user).unwrap();
    assert!(user.get("sso_type").is_some());
    assert!(user.get("sso_id").is_some());
    // `two_factor` has `omitempty` in Go and is empty for a new user.
    assert!(user.get("two_factor").is_none());
    assert!(user.get("sessions").is_none());

    let empty_client = serde_json::to_value(Client::default()).unwrap();
    assert!(empty_client.get("uuid").is_none());
    assert!(empty_client.get("ipv4").is_none());
    assert!(empty_client.get("created_at").is_some());
    let partial: Client = serde_json::from_str(r#"{"uuid":"u","token":"t","name":"n"}"#).unwrap();
    assert_eq!(partial.uuid, "u");
    assert_eq!(partial.cpu_cores, 0);

    let legacy = komari_db::models::Config::default();
    let legacy = serde_json::to_value(&legacy).unwrap();
    assert!(legacy.get("o_auth_enabled").is_some());
    assert!(legacy.get("o_auth_provider").is_some());
    assert!(legacy.get("geo_ip_enabled").is_some());

    // Go's OfflineNotification.ClientInfo is a value type, so the field is
    // always serialized (even as a zero-valued object).
    let offline = OfflineNotification::default();
    let offline = serde_json::to_value(&offline).unwrap();
    assert!(offline.get("client_info").is_some());
    assert!(offline["client_info"].is_object());

    // `client_info: null` is accepted and becomes the zero-value client.
    let decoded: OfflineNotification =
        serde_json::from_str(r#"{"client":"c1","client_info":null}"#).unwrap();
    assert_eq!(decoded.client, "c1");
    assert!(decoded.client_info.uuid.is_empty());
    assert!(decoded.client_info.token.is_empty());
}
