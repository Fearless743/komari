//! Notification service: provider selection, retries and event templating
//! (Go `utils/messageSender/sender.go`).

use std::sync::Arc;

use komari_db::Db;
use komari_db::models::EventMessage;

use crate::error::{NotifyError, Result};
use crate::event::{DEFAULT_TEMPLATE, QQ_SHORT_RESPONSE_ERROR, format_event};
use crate::provider::MessageSender;
use crate::registry::load_provider_from_db;

/// Configuration values that control whether and how events are sent.
#[derive(Clone, Debug)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub template: String,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            template: DEFAULT_TEMPLATE.to_owned(),
        }
    }
}

impl NotificationSettings {
    /// Read `notification_enabled` / `notification_template`.
    ///
    /// Go's defaults are `false` and [`DEFAULT_TEMPLATE`]; missing keys do not
    /// get written back here.
    pub fn from_db(db: &Db) -> Result<Self> {
        let enabled = db
            .config()
            .get_as::<bool>("notification_enabled")?
            .unwrap_or(false);
        let template = db
            .config()
            .get_as::<String>("notification_template")?
            .unwrap_or_else(|| DEFAULT_TEMPLATE.to_owned());
        Ok(Self { enabled, template })
    }
}

/// Sends notifications through one provider, retrying three times like Go.
pub struct NotificationService<'a> {
    db: Option<&'a Db>,
    sender: Option<Arc<dyn MessageSender>>,
    settings: NotificationSettings,
}

impl<'a> NotificationService<'a> {
    /// Build the service selected by the database (`notification_method`).
    pub async fn from_db(db: &'a Db) -> Result<Self> {
        let settings = NotificationSettings::from_db(db)?;
        let sender = load_provider_from_db(db).await?;
        let sender: Arc<dyn MessageSender> = Arc::from(sender);
        Ok(Self {
            db: Some(db),
            sender: Some(sender),
            settings,
        })
    }

    /// Construct a service around an explicit sender (useful for tests).
    pub fn with_sender(sender: Arc<dyn MessageSender>, settings: NotificationSettings) -> Self {
        Self {
            db: None,
            sender: Some(sender),
            settings,
        }
    }
    pub fn settings(&self) -> &NotificationSettings {
        &self.settings
    }

    pub fn sender(&self) -> Option<&Arc<dyn MessageSender>> {
        self.sender.as_ref()
    }

    /// Port of `SendTextMessage`; returns `false` when notifications are off.
    pub async fn send_text(&self, message: &str, title: &str) -> Result<bool> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(NotifyError::NotInitialized);
        };
        if !self.settings.enabled {
            return Ok(false);
        }

        let mut last_error = None;
        for _ in 0..3 {
            match sender.send_text(message, title).await {
                Ok(()) => {
                    self.log(&format!("Message sent: {title}"), "info");
                    return Ok(true);
                }
                Err(error) => last_error = Some(error),
            }
        }

        let error = last_error.unwrap_or(NotifyError::Send("unknown send failure".to_owned()));
        self.log(
            &format!("Failed to send message after 3 attempts: {error},{title}"),
            "error",
        );
        Err(error)
    }

    /// Port of `SendEvent`: prefer the provider's structured event handler,
    /// then retry three times.
    pub async fn send_event(&self, event: &EventMessage) -> Result<bool> {
        let Some(sender) = self.sender.as_ref() else {
            return Err(NotifyError::NotInitialized);
        };
        if !self.settings.enabled {
            return Ok(false);
        }

        // Go type-asserts the provider to `IEventMessageSender` and only falls
        // back to the templated text when that assertion fails.
        if let Some(event_sender) = sender.as_event_sender() {
            let mut last_error = None;
            for _ in 0..3 {
                match event_sender.send_event(event).await {
                    Ok(()) => {
                        self.log(&format!("Event message sent: {}", event.event), "info");
                        return Ok(true);
                    }
                    Err(NotifyError::Send(message)) if message == QQ_SHORT_RESPONSE_ERROR => {
                        self.log(&format!("Event message sent: {}", event.event), "info");
                        return Ok(true);
                    }
                    Err(error) => last_error = Some(error),
                }
            }

            let error = last_error.unwrap_or(NotifyError::Send("unknown send failure".to_owned()));
            self.log(
                &format!(
                    "Failed to send event message after 3 attempts: {error},{}",
                    event.event
                ),
                "error",
            );
            return Err(error);
        }

        let message = format_event(&self.settings.template, event);
        let mut last_error = None;
        for _ in 0..3 {
            match sender.send_text(&message, &event.event).await {
                Ok(()) => {
                    self.log(&format!("Event message sent: {}", event.event), "info");
                    return Ok(true);
                }
                Err(NotifyError::Send(message)) if message == QQ_SHORT_RESPONSE_ERROR => {
                    // Go treats this QQ error as a successful delivery.
                    self.log(&format!("Event message sent: {}", event.event), "info");
                    return Ok(true);
                }
                Err(error) => last_error = Some(error),
            }
        }

        let error = last_error.unwrap_or(NotifyError::Send("unknown send failure".to_owned()));
        self.log(
            &format!(
                "Failed to send event message after 3 attempts: {error},{}",
                event.event
            ),
            "error",
        );
        Err(error)
    }

    fn log(&self, message: &str, level: &str) {
        if let Some(db) = self.db {
            // Audit logging must never turn a delivered notification into a
            // failure; Go also ignores the log write result here.
            let _ = db.logs().log("", "", message, level);
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use komari_db::models::EventMessage;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct AlwaysFail {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl MessageSender for AlwaysFail {
        fn name(&self) -> &'static str {
            "always-fail"
        }
        fn config(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        async fn send_text(&self, _message: &str, _title: &str) -> Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(NotifyError::Send("nope".to_owned()))
        }
    }

    struct QqShortResponse {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl MessageSender for QqShortResponse {
        fn name(&self) -> &'static str {
            "qq-short-response"
        }
        fn config(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        async fn send_text(&self, _message: &str, _title: &str) -> Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(NotifyError::Send(QQ_SHORT_RESPONSE_ERROR.to_owned()))
        }
    }

    struct Recorder {
        messages: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl MessageSender for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn config(&self) -> serde_json::Value {
            serde_json::Value::Null
        }
        async fn send_text(&self, message: &str, _title: &str) -> Result<()> {
            self.messages.lock().push(message.to_owned());
            Ok(())
        }
    }

    #[tokio::test]
    async fn retries_exactly_three_times() {
        let sender = Arc::new(AlwaysFail {
            attempts: AtomicUsize::new(0),
        });
        let service = NotificationService::with_sender(
            sender.clone(),
            NotificationSettings {
                enabled: true,
                ..NotificationSettings::default()
            },
        );
        assert!(service.send_text("hello", "title").await.is_err());
        assert_eq!(sender.attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn disabled_notifications_do_not_call_the_provider() {
        let sender = Arc::new(Recorder {
            messages: Mutex::new(Vec::new()),
        });
        let service =
            NotificationService::with_sender(sender.clone(), NotificationSettings::default());
        assert!(!service.send_text("hello", "title").await.unwrap());
        assert!(sender.messages.lock().is_empty());
    }

    #[tokio::test]
    async fn qq_short_response_counts_as_success() {
        let sender = Arc::new(QqShortResponse {
            attempts: AtomicUsize::new(0),
        });
        let service = NotificationService::with_sender(
            sender.clone(),
            NotificationSettings {
                enabled: true,
                ..NotificationSettings::default()
            },
        );
        let event = EventMessage {
            event: "Offline".to_owned(),
            ..EventMessage::default()
        };
        assert!(service.send_event(&event).await.unwrap());
        assert_eq!(sender.attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn event_is_rendered_with_the_template() {
        let sender = Arc::new(Recorder {
            messages: Mutex::new(Vec::new()),
        });
        let settings = NotificationSettings {
            enabled: true,
            template: "{{emoji}} {{event}} {{message}}".to_owned(),
        };
        let service = NotificationService::with_sender(sender.clone(), settings);
        let event = EventMessage {
            event: "Offline".to_owned(),
            message: "gone".to_owned(),
            emoji: "🔴".to_owned(),
            ..EventMessage::default()
        };
        assert!(service.send_event(&event).await.unwrap());
        assert_eq!(sender.messages.lock().as_slice(), ["🔴 Offline gone"]);
    }
}
