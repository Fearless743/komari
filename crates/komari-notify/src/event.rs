//! Event text formatting and the send-with-retry service.

use komari_db::models::EventMessage;

/// Default template from `config.Legacy.NotificationTemplate`.
pub const DEFAULT_TEMPLATE: &str = "{{emoji}}{{emoji}}{{emoji}}\nEvent: {{event}}\nClients: {{client}}\nMessage: {{message}}\nTime: {{time}}";

/// QQ returns this error even when the message was actually delivered.
///
/// Go treats it as success in `SendEvent`; the constant is exposed so the
/// retry loop can apply the same rule.
pub const QQ_SHORT_RESPONSE_ERROR: &str = "short response: \u{0}\u{0}\u{0}\u{1a}\u{0}\u{0}\u{0}";

/// Render the notification template for an event.
///
/// Mirrors Go `parseTemplate`: client names fall back to their UUID, clients
/// are joined with `", "`, and time is formatted as RFC 3339.
pub fn format_event(template: &str, event: &EventMessage) -> String {
    let clients = event
        .clients
        .iter()
        .map(|client| {
            if client.name.trim().is_empty() {
                client.uuid.as_str()
            } else {
                client.name.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    let time = event
        .time
        .to_rfc3339()
        .unwrap_or_else(|| "0001-01-01T00:00:00Z".to_owned());

    template
        .replace("{{event}}", &event.event)
        .replace("{{client}}", &clients)
        .replace("{{time}}", &time)
        .replace("{{message}}", &event.message)
        .replace("{{emoji}}", &event.emoji)
}

#[cfg(test)]
mod tests {
    use komari_db::LocalTime;
    use komari_db::models::{Client, EventMessage};

    use super::*;

    fn client(name: &str, uuid: &str) -> Client {
        Client {
            name: name.to_owned(),
            uuid: uuid.to_owned(),
            ..Client::default()
        }
    }

    #[test]
    fn template_replaces_every_placeholder() {
        let event = EventMessage {
            event: "Offline".to_owned(),
            clients: vec![client("alpha", "u-1"), client("", "u-2")],
            time: LocalTime::parse("2026-01-02T03:04:05Z").unwrap(),
            message: "hello".to_owned(),
            emoji: "🔴".to_owned(),
        };

        let rendered = format_event(DEFAULT_TEMPLATE, &event);
        assert!(rendered.starts_with("🔴🔴🔴\n"));
        assert!(rendered.contains("Event: Offline"));
        assert!(rendered.contains("Clients: alpha, u-2"));
        assert!(rendered.contains("Message: hello"));
        let expected_time = event.time.to_rfc3339().unwrap();
        assert!(rendered.contains(&format!("Time: {expected_time}")));
    }

    #[test]
    fn zero_time_falls_back_to_go_zero_format() {
        let event = EventMessage::default();
        assert!(format_event("{{time}}", &event).starts_with("0001-01-01T00:00:00"));
    }
}
