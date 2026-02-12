/// CLI adapter — stdin/stdout for testing (spec 6.9).
///
/// Always assigns `Principal::Owner` since CLI access implies
/// direct machine access. Used for integration testing and
/// local development.
use chrono::Utc;
use uuid::Uuid;

use crate::types::{EventKind, EventPayload, EventSource, InboundEvent, Principal};

/// Create an inbound event from a CLI text message.
///
/// The CLI adapter always produces events with `Principal::Owner`
/// since physical access to the terminal implies owner trust (spec 6.9).
pub fn create_cli_event(text: &str) -> InboundEvent {
    InboundEvent {
        event_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        source: EventSource {
            adapter: "cli".to_owned(),
            principal: Principal::Owner,
        },
        kind: EventKind::Message,
        payload: EventPayload {
            text: Some(text.to_owned()),
            attachments: vec![],
            reply_to: None,
            metadata: serde_json::json!({}),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_cli_event() {
        let event = create_cli_event("check my email");
        assert_eq!(event.source.adapter, "cli");
        assert_eq!(event.source.principal, Principal::Owner);
        assert_eq!(event.payload.text.as_deref(), Some("check my email"));
        assert!(matches!(event.kind, EventKind::Message));
    }
}
