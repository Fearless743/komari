//! No-op DDNS provider used when DDNS is disabled or misconfigured.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DdnsError;
use crate::factory::DdnsFactory;
use crate::provider::{DdnsProvider, SyncContext, SyncResult};

/// A provider that accepts every synchronization and does nothing.
#[derive(Clone, Debug, Default)]
pub struct EmptyProvider;

#[async_trait]
impl DdnsProvider for EmptyProvider {
    fn name(&self) -> &str {
        "empty"
    }

    fn configuration(&self) -> Value {
        json!({})
    }

    fn set_configuration(&mut self, configuration: Value) -> Result<(), DdnsError> {
        match configuration {
            Value::Null | Value::Object(_) => Ok(()),
            _ => Err(DdnsError::InvalidConfig(
                "empty provider configuration must be a JSON object".to_owned(),
            )),
        }
    }

    async fn sync(&self, _context: &SyncContext) -> Result<SyncResult, DdnsError> {
        Ok(SyncResult::default())
    }

    async fn delete_record(&self, _record_id: &str) -> Result<(), DdnsError> {
        Err(DdnsError::Provider(
            "empty DDNS provider cannot delete records".to_owned(),
        ))
    }
}

/// Register the `empty` provider.
pub fn register(factory: &DdnsFactory) {
    factory.register_provider(std::sync::Arc::new(|| Box::new(EmptyProvider)));
}
