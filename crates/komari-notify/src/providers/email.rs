//! SMTP email provider (Go `utils/messageSender/email`).
//!
//! Request construction is separated from delivery so tests can inspect the
//! generated MIME envelope. Delivery uses `lettre`; the configuration fields,
//! validation rules and MIME headers follow the Go implementation.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use lettre::message::header::{ContentTransferEncoding, ContentType, MIME_VERSION_1_0};
use lettre::message::{Mailbox, Mailboxes};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{NotifyError, Result};
use crate::provider::{ConfigField, MessageSender};

/// Provider configuration (`email.Addition`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    pub host: String,
    pub port: i64,
    pub username: String,
    pub password: String,
    pub sender: String,
    pub receiver: String,
    pub use_ssl: bool,
    pub use_login_auth: bool,
}

/// A fully-built SMTP message.
#[derive(Clone, Debug)]
pub struct EmailEnvelope {
    pub host: String,
    pub port: u16,
    pub use_ssl: bool,
    pub use_login_auth: bool,
    pub username: String,
    pub password: String,
    pub from: String,
    pub recipients: Vec<String>,
    /// The fully built MIME message (lettre keeps the encoded headers/body).
    pub message: Message,
}

impl EmailEnvelope {
    /// Formatted RFC 5322 bytes, as they are written to the SMTP DATA phase.
    pub fn raw(&self) -> Vec<u8> {
        self.message.formatted()
    }

    pub fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw()).into_owned()
    }
}

/// Delivers a prepared envelope.
#[async_trait]
pub trait EmailTransport: Send + Sync {
    async fn send(&self, envelope: EmailEnvelope) -> Result<()>;
}

/// Production transport backed by lettre's blocking SMTP client.
#[derive(Clone, Debug, Default)]
pub struct LettreEmailTransport;

#[async_trait]
impl EmailTransport for LettreEmailTransport {
    async fn send(&self, envelope: EmailEnvelope) -> Result<()> {
        tokio::task::spawn_blocking(move || send_blocking(envelope))
            .await
            .map_err(|error| NotifyError::Email(format!("smtp task failed: {error}")))?
    }
}

/// TLS policy matching Go's SMTP clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// `use_ssl=true` with an implicit-TLS port.
    Wrapper,
    /// `use_ssl=true` with STARTTLS required.
    Required,
    /// `use_ssl=false`: Go's `smtp.SendMail` still upgrades whenever the
    /// server advertises STARTTLS.
    Opportunistic,
}

/// Resolve the TLS policy from the provider configuration.
pub fn tls_mode(use_ssl: bool, port: u16) -> TlsMode {
    if !use_ssl {
        TlsMode::Opportunistic
    } else if port == 465 {
        TlsMode::Wrapper
    } else {
        TlsMode::Required
    }
}

fn send_blocking(envelope: EmailEnvelope) -> Result<()> {
    let mut builder = SmtpTransport::builder_dangerous(&envelope.host)
        .port(envelope.port)
        .credentials(Credentials::new(
            envelope.username.clone(),
            envelope.password.clone(),
        ))
        .authentication(vec![if envelope.use_login_auth {
            Mechanism::Login
        } else {
            Mechanism::Plain
        }]);

    let parameters = TlsParameters::new(envelope.host.clone())
        .map_err(|error| NotifyError::Email(format!("failed to initialise TLS: {error}")))?;
    let tls = match tls_mode(envelope.use_ssl, envelope.port) {
        TlsMode::Wrapper => Tls::Wrapper(parameters),
        TlsMode::Required => Tls::Required(parameters),
        TlsMode::Opportunistic => Tls::Opportunistic(parameters),
    };
    builder = builder.tls(tls);

    builder
        .build()
        .send(&envelope.message)
        .map(|_| ())
        .map_err(|error| NotifyError::Email(error.to_string()))
}

/// Email text sender.
pub struct EmailSender {
    config: EmailConfig,
    transport: Arc<dyn EmailTransport>,
}

impl EmailSender {
    pub fn new(config: EmailConfig) -> Self {
        Self::with_transport(config, Arc::new(LettreEmailTransport))
    }

    pub fn with_transport(config: EmailConfig, transport: Arc<dyn EmailTransport>) -> Self {
        Self { config, transport }
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let config: EmailConfig = serde_json::from_value(value)?;
        Ok(Self::new(config))
    }

    pub fn with_transport_from_value(
        value: Value,
        transport: Arc<dyn EmailTransport>,
    ) -> Result<Self> {
        let config: EmailConfig = serde_json::from_value(value)?;
        Ok(Self::with_transport(config, transport))
    }

    /// Validate the same fields Go requires before sending.
    pub fn validate(&self) -> Result<()> {
        if self.config.host.is_empty()
            || self.config.sender.is_empty()
            || self.config.username.is_empty()
            || self.config.password.is_empty()
            || self.config.receiver.is_empty()
        {
            return Err(NotifyError::InvalidConfig(
                "email sending is not fully configured".to_owned(),
            ));
        }
        Ok(())
    }

    /// Build the MIME message and SMTP envelope without connecting.
    pub fn build_envelope(&self, message: &str, title: &str) -> Result<EmailEnvelope> {
        self.validate()?;

        let sender = parse_sender(&self.config.sender)?;
        let recipients = parse_recipients(&self.config.receiver)?;
        if recipients.is_empty() {
            return Err(NotifyError::Email(
                "no valid recipient address parsed".to_owned(),
            ));
        }

        let is_html = is_html_message(message);
        let content_type = if is_html {
            ContentType::TEXT_HTML
        } else {
            ContentType::TEXT_PLAIN
        };

        let mut builder = Message::builder()
            .from(sender.clone())
            .subject(title.to_owned())
            .date_now()
            .message_id(Some(format!(
                "<{}@{}>",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                self.config.host
            )))
            .header(MIME_VERSION_1_0)
            .header(ContentTransferEncoding::QuotedPrintable)
            .header(content_type);
        for recipient in &recipients {
            builder = builder.to(recipient.clone());
        }

        let message = builder
            .body(message.to_owned())
            .map_err(|error| NotifyError::Email(error.to_string()))?;

        Ok(EmailEnvelope {
            host: self.config.host.clone(),
            port: u16::try_from(self.config.port).map_err(|_| {
                NotifyError::InvalidConfig(format!("invalid SMTP port: {}", self.config.port))
            })?,
            use_ssl: self.config.use_ssl,
            use_login_auth: self.config.use_login_auth,
            username: self.config.username.clone(),
            password: self.config.password.clone(),
            from: sender.email.to_string(),
            recipients: recipients
                .iter()
                .map(|mailbox| mailbox.email.to_string())
                .collect(),
            message,
        })
    }
}

fn parse_sender(value: &str) -> Result<Mailbox> {
    Mailbox::from_str(value).or_else(|_| {
        lettre::Address::from_str(value)
            .map(|address| Mailbox::new(None, address))
            .map_err(|error| NotifyError::Email(format!("invalid sender address: {error}")))
    })
}

fn parse_recipients(value: &str) -> Result<Vec<Mailbox>> {
    if let Ok(mailboxes) = Mailboxes::from_str(value) {
        let parsed: Vec<Mailbox> = mailboxes.into_iter().collect();
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }

    let mut recipients = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        recipients.push(parse_sender(part)?);
    }
    Ok(recipients)
}

fn is_html_message(message: &str) -> bool {
    let trimmed = message.trim();
    let lower = trimmed.to_lowercase();
    lower.contains("<html")
        || lower.contains("<!doctype")
        || (trimmed.contains("<div") && trimmed.contains("</div>"))
}

pub fn fields() -> Vec<ConfigField> {
    vec![
        ConfigField::new("host", "string").required(true),
        ConfigField::new("port", "int")
            .required(true)
            .default_value("587"),
        ConfigField::new("username", "string"),
        ConfigField::new("password", "string"),
        ConfigField::new("sender", "string"),
        ConfigField::new("receiver", "string"),
        ConfigField::new("use_ssl", "bool").default_value("false"),
        ConfigField::new("use_login_auth", "bool").default_value("false").help(
            "Use LOGIN authentication method instead of PLAIN. Enable this if you encounter authentication errors with Microsoft (Outlook/Office365), NetEase (163.com), or other email providers",
        ),
    ]
}

pub fn default_config() -> Value {
    serde_json::to_value(EmailConfig::default()).unwrap_or(Value::Null)
}

pub fn constructor(value: Value) -> Result<Box<dyn MessageSender>> {
    Ok(Box::new(EmailSender::from_value(value)?))
}

#[async_trait]
impl MessageSender for EmailSender {
    fn name(&self) -> &'static str {
        "email"
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.config).unwrap_or(Value::Null)
    }

    async fn send_text(&self, message: &str, title: &str) -> Result<()> {
        let envelope = self.build_envelope(message, title)?;
        self.transport.send(envelope).await
    }
}
