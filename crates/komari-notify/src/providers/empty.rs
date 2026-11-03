//! The `empty` provider (Go `utils/messageSender/empty`).
//!
//! It is the fallback when notifications are disabled or no provider is
//! configured, and deliberately fails every send.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{NotifyError, Result};
use crate::provider::{ConfigField, MessageSender};

/// The empty provider has no configuration fields.
#[derive(Clone, Debug, Default)]
pub struct EmptySender;

impl EmptySender {
    pub fn from_value(_value: Value) -> Result<Self> {
        Ok(Self)
    }
}

pub fn fields() -> Vec<ConfigField> {
    Vec::new()
}

pub fn default_config() -> Value {
    Value::Object(serde_json::Map::new())
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(EmptySender::from_value(value)?))
}

#[async_trait]
impl MessageSender for EmptySender {
    fn name(&self) -> &'static str {
        "empty"
    }

    fn config(&self) -> Value {
        Value::Object(serde_json::Map::new())
    }

    async fn send_text(&self, _message: &str, _title: &str) -> Result<()> {
        Err(NotifyError::Send(
            "you are using an empty message sender, please check your configuration".to_owned(),
        ))
    }
}
