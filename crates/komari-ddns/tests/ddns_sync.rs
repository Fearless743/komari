use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use komari_config::keys::{DDNS_ENABLED_KEY, DDNS_PROVIDER_KEY};
use komari_db::Db;
use komari_db::models::{Client, DdnsProvider as ProviderModel};
use komari_ddns::error::DdnsError;
use komari_ddns::factory::DdnsFactory;
use komari_ddns::provider::{DdnsProvider, SyncContext, SyncResult};
use komari_ddns::service::{DdnsService, merge_client_provider_config};
use serde_json::{Value, json};

/// Shared between the factory constructor and the test body.
#[derive(Default)]
struct ProviderState {
    calls: Mutex<Vec<SyncContext>>,
    fail: AtomicBool,
    result_id: String,
}

struct TestProvider {
    state: Arc<ProviderState>,
}

#[async_trait]
impl DdnsProvider for TestProvider {
    fn name(&self) -> &str {
        "test"
    }

    fn configuration(&self) -> Value {
        json!({})
    }

    fn set_configuration(&mut self, configuration: Value) -> Result<(), DdnsError> {
        if configuration.is_null() || configuration.is_object() {
            Ok(())
        } else {
            Err(DdnsError::InvalidConfig("expected object".to_owned()))
        }
    }

    async fn sync(&self, context: &SyncContext) -> Result<SyncResult, DdnsError> {
        self.state
            .calls
            .lock()
            .expect("call log poisoned")
            .push(context.clone());
        if self.state.fail.load(Ordering::SeqCst) {
            return Err(DdnsError::Provider("boom".to_owned()));
        }
        if self.state.result_id.is_empty() {
            Ok(SyncResult::default())
        } else {
            Ok(SyncResult::resolved(&self.state.result_id))
        }
    }
}

fn database() -> Arc<Db> {
    let db = Arc::new(Db::open_in_memory().expect("in-memory database"));
    db.migrate().expect("migrate");
    db
}

fn service(db: &Arc<Db>, result_id: &str, max_retries: u32) -> (DdnsService, Arc<ProviderState>) {
    let state = Arc::new(ProviderState {
        result_id: result_id.to_owned(),
        ..ProviderState::default()
    });
    let factory = DdnsFactory::new();
    let constructor_state = Arc::clone(&state);
    factory.register_provider(Arc::new(move || {
        Box::new(TestProvider {
            state: Arc::clone(&constructor_state),
        })
    }));
    let service = DdnsService::with_factory(Arc::clone(db), factory)
        .with_retry_policy(max_retries, Duration::ZERO);
    service
        .load_provider("test", "{}")
        .expect("load test provider");
    (service, state)
}

fn enable_ddns(db: &Db) {
    db.config().set(DDNS_ENABLED_KEY, &true).expect("enable");
    db.config()
        .set(DDNS_PROVIDER_KEY, "test")
        .expect("provider key");
}

fn create_client(db: &Db, uuid: &str, ipv4: &str) -> Client {
    let mut client = Client::new("node");
    client.uuid = uuid.to_owned();
    client.ipv4 = ipv4.to_owned();
    client.ddns_enabled = true;
    client.ddns_hostname = "node.example.com".to_owned();
    db.clients().create(&client).expect("create client");
    client
}

#[tokio::test]
async fn unchanged_address_does_not_sync_again() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    let client = create_client(&db, "client-1", "1.1.1.1");

    service
        .sync_client(&client, "test", false)
        .await
        .expect("first");
    service
        .sync_client(&client, "test", false)
        .await
        .expect("second");

    assert_eq!(state.calls.lock().unwrap().len(), 1);
    assert_eq!(db.ddns().history_count().unwrap(), 1);
    let history = db.ddns().history("", 50).unwrap();
    assert_eq!(history[0].status, "success");
    assert_eq!(history[0].ipv4, "1.1.1.1");
    assert_eq!(history[0].hostname, "node.example.com");
    assert_eq!(
        service.last_state("client-1"),
        Some(("1.1.1.1".to_owned(), String::new()))
    );
}

#[tokio::test]
async fn changed_address_syncs_and_writes_history() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    let client = create_client(&db, "client-1", "1.1.1.1");

    service
        .sync_client(&client, "test", false)
        .await
        .expect("first");
    let mut changed = client.clone();
    changed.ipv4 = "9.9.9.9".to_owned();
    service
        .sync_client(&changed, "test", false)
        .await
        .expect("second");

    assert_eq!(state.calls.lock().unwrap().len(), 2);
    let history = db.ddns().history("", 50).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].ipv4, "9.9.9.9");
    assert!(history.iter().all(|entry| entry.status == "success"));
}

#[tokio::test]
async fn failures_retry_and_record_the_error() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    state.fail.store(true, Ordering::SeqCst);
    let client = create_client(&db, "client-1", "1.1.1.1");

    let error = service
        .sync_client(&client, "test", false)
        .await
        .expect_err("provider always fails");
    assert!(matches!(
        error,
        DdnsError::RetryExhausted { retries: 2, .. }
    ));
    assert_eq!(state.calls.lock().unwrap().len(), 3);
    assert_eq!(service.retry_count("client-1"), 1);

    let history = db.ddns().history("", 50).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, "failed");
    assert!(history[0].error.contains("boom"));

    state.fail.store(false, Ordering::SeqCst);
    service
        .sync_client(&client, "test", false)
        .await
        .expect("eventual success");
    assert_eq!(state.calls.lock().unwrap().len(), 4);
    assert_eq!(service.retry_count("client-1"), 0);
    assert_eq!(db.ddns().history_count().unwrap(), 2);
}

#[tokio::test]
async fn disabled_ddns_requires_force() {
    let db = database();
    let (service, state) = service(&db, "", 2);
    let client = create_client(&db, "client-1", "1.1.1.1");

    service
        .sync_client(&client, "test", false)
        .await
        .expect("skip");
    assert_eq!(state.calls.lock().unwrap().len(), 0);

    service
        .sync_client(&client, "test", true)
        .await
        .expect("force");
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn client_with_ddns_disabled_requires_force() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    let mut client = create_client(&db, "client-1", "1.1.1.1");
    client.ddns_enabled = false;

    service
        .sync_client(&client, "test", false)
        .await
        .expect("skip");
    assert_eq!(state.calls.lock().unwrap().len(), 0);

    service
        .sync_client(&client, "test", true)
        .await
        .expect("force");
    assert_eq!(state.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn resolved_record_id_is_persisted() {
    let db = database();
    enable_ddns(&db);
    let (service, _) = service(&db, "record-1", 2);
    let client = create_client(&db, "client-1", "1.1.1.1");
    assert!(client.ddns_record_id.is_empty());

    service
        .sync_client(&client, "test", false)
        .await
        .expect("sync");

    let stored = db.clients().get("client-1").unwrap().expect("client");
    assert_eq!(stored.ddns_record_id, "record-1");
}

#[tokio::test]
async fn empty_addresses_are_skipped_even_when_forced() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    let mut client = create_client(&db, "client-1", "");
    client.ipv6.clear();

    service
        .sync_client(&client, "test", true)
        .await
        .expect("skip");
    assert_eq!(state.calls.lock().unwrap().len(), 0);
    assert_eq!(db.ddns().history_count().unwrap(), 0);
}

#[tokio::test]
async fn delete_record_requires_a_loaded_provider_and_delegates() {
    let db = database();
    let unloaded = DdnsService::new(Arc::clone(&db));
    assert!(matches!(
        unloaded.delete_record("record-1").await,
        Err(DdnsError::NotConfigured(message)) if message.contains("no DDNS provider")
    ));

    let (service, _) = service(&db, "", 2);
    let error = service
        .delete_record("record-1")
        .await
        .expect_err("test provider does not support deletion");
    assert!(error.to_string().contains("does not support deleting"));
}

#[tokio::test]
async fn load_provider_validates_name_and_configuration() {
    let db = database();
    let (service, _) = service(&db, "", 2);

    assert!(matches!(
        service.load_provider("missing", "{}"),
        Err(DdnsError::ProviderNotFound(name)) if name == "missing"
    ));
    assert!(matches!(
        service.load_provider("test", "not-json"),
        Err(DdnsError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn initialize_persists_defaults_and_loads_selected_provider() {
    let db = database();
    db.ddns_providers()
        .save(&ProviderModel::new(
            "webhook",
            r#"{"url":"http://example.test/hook"}"#,
        ))
        .expect("save webhook");
    db.config()
        .set(DDNS_PROVIDER_KEY, "webhook")
        .expect("provider key");

    let service = DdnsService::new(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(
        service
            .current_provider()
            .map(|provider| provider.name().to_owned()),
        Some("webhook".to_owned())
    );
    let names: Vec<String> = db
        .ddns_providers()
        .list()
        .unwrap()
        .into_iter()
        .map(|provider| provider.name)
        .collect();
    assert!(names.contains(&"cloudflare".to_owned()));
    assert!(names.contains(&"empty".to_owned()));
    assert!(names.contains(&"webhook".to_owned()));

    let cloudflare = db.ddns_providers().get("cloudflare").unwrap().unwrap();
    assert!(cloudflare.addition.contains("api_token"));
    assert_eq!(
        db.config()
            .get_as::<String>(DDNS_PROVIDER_KEY)
            .unwrap()
            .unwrap(),
        "webhook"
    );
}

#[tokio::test]
async fn initialize_falls_back_when_selected_provider_row_is_missing() {
    let db = database();
    db.config()
        .set(DDNS_PROVIDER_KEY, "missing-provider")
        .expect("provider key");

    let service = DdnsService::new(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(
        service
            .current_provider()
            .map(|provider| provider.name().to_owned()),
        Some("empty".to_owned())
    );
}

#[tokio::test]
async fn initialize_falls_back_when_selected_provider_json_is_invalid() {
    let db = database();
    db.ddns_providers()
        .save(&ProviderModel::new("webhook", "not-json"))
        .expect("save invalid provider");
    db.config()
        .set(DDNS_PROVIDER_KEY, "webhook")
        .expect("provider key");

    let service = DdnsService::new(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(
        service
            .current_provider()
            .map(|provider| provider.name().to_owned()),
        Some("empty".to_owned())
    );
}

#[tokio::test]
async fn initialize_survives_a_config_table_read_error() {
    let db = database();
    db.execute_batch("DROP TABLE configs")
        .expect("drop configs");

    let service = DdnsService::new(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(
        service
            .current_provider()
            .map(|provider| provider.name().to_owned()),
        Some("empty".to_owned())
    );
}

#[tokio::test]
async fn config_read_error_is_treated_as_disabled_during_sync() {
    let db = database();
    enable_ddns(&db);
    let (service, state) = service(&db, "", 2);
    let client = create_client(&db, "client-1", "1.1.1.1");
    db.execute_batch("DROP TABLE configs")
        .expect("drop configs");

    service
        .sync_client(&client, "test", false)
        .await
        .expect("failure degrades to disabled");

    assert_eq!(state.calls.lock().unwrap().len(), 0);
    assert_eq!(db.ddns().history_count().unwrap(), 0);
}

#[tokio::test]
async fn initialize_without_selection_uses_empty_provider() {
    let db = database();
    let service = DdnsService::new(Arc::clone(&db));
    service.initialize().await.expect("initialize");

    assert_eq!(
        service
            .current_provider()
            .map(|provider| provider.name().to_owned()),
        Some("empty".to_owned())
    );
    assert_eq!(
        db.config()
            .get_as::<String>(DDNS_PROVIDER_KEY)
            .unwrap()
            .unwrap(),
        "none"
    );
}

#[test]
fn client_overrides_merge_over_provider_configuration() {
    let mut configuration = serde_json::Map::new();
    configuration.insert("hostname".to_owned(), json!("global.example.com"));
    configuration.insert("api_token".to_owned(), json!("token"));
    let mut client = Client::new("node");
    client.ddns_hostname = "node.example.com".to_owned();
    client.ddns_record_id = "record-1".to_owned();
    client.ddns_record_type = "AAAA".to_owned();

    let merged = merge_client_provider_config(configuration, &client);
    assert_eq!(merged["api_token"], "token");
    assert_eq!(merged["hostname"], "node.example.com");
    assert_eq!(merged["record_id"], "record-1");
    assert_eq!(merged["record_type"], "AAAA");

    let empty_client = Client::new("node");
    let merged = merge_client_provider_config(serde_json::Map::new(), &empty_client);
    assert!(!merged.contains_key("hostname"));
}
