//! Moira's own message shape for `ExecutionCommand` (plan 06, finding P2-2).
//!
//! # Why this exists
//!
//! `ExecutionCommand.messages` used to be `Vec<Message>` borrowed straight from Rig, which made a
//! `domain` DTO generic over an upstream crate's execution primitive. That violates two rules the
//! repository already states: `docs/project-structure.md` ("domain must stay dependency-light")
//! and `CLAUDE.md` ("Rig owns AI execution primitives"). `ExecutionCommand` also derives
//! `Serialize`/`Deserialize`, so the leak meant Moira's internal command shape was pinned to Rig's
//! wire format for messages.
//!
//! `DomainMessage` is deliberately **not** a general-purpose re-expression of Rig's message type
//! (that would be the parallel-abstraction anti-pattern). It is exactly the caller-supplied input
//! Moira accepts, and nothing more: a role plus ordered content parts. Assistant tool calls,
//! reasoning blocks, audio, video, and documents are provider-protocol concerns and have no
//! representation here.
//!
//! # Conversion
//!
//! There is exactly one conversion site, and it lives at the Rig boundary:
//! `impl TryFrom<&DomainMessage>` in `src/orchestration/runtime_factory.rs`. Nothing outside
//! `orchestration` may construct a Rig message. The reverse direction does not exist — provider
//! output is narrowed to text by `text_from_choice`, and re-hydrating a `DomainMessage` from a
//! completion would be a second response-narrowing path.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainMessage {
    pub role: DomainMessageRole,
    pub content: Vec<DomainMessageContent>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainMessageContent {
    Text { text: String },
    ImageUrl { url: String },
}

impl DomainMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self::single_text(DomainMessageRole::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::single_text(DomainMessageRole::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::single_text(DomainMessageRole::Assistant, text)
    }

    pub fn new(role: DomainMessageRole, content: Vec<DomainMessageContent>) -> Self {
        Self { role, content }
    }

    /// First text part of this message, ignoring non-text content.
    pub fn first_text(&self) -> Option<&str> {
        self.content.iter().find_map(|part| match part {
            DomainMessageContent::Text { text } => Some(text.as_str()),
            DomainMessageContent::ImageUrl { .. } => None,
        })
    }

    fn single_text(role: DomainMessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![DomainMessageContent::Text { text: text.into() }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_text_skips_image_parts() {
        let message = DomainMessage::new(
            DomainMessageRole::User,
            vec![
                DomainMessageContent::ImageUrl {
                    url: "https://example.test/a.png".to_string(),
                },
                DomainMessageContent::Text {
                    text: "describe it".to_string(),
                },
            ],
        );
        assert_eq!(message.first_text(), Some("describe it"));
    }

    #[test]
    fn first_text_is_none_without_any_text_part() {
        let message = DomainMessage::new(
            DomainMessageRole::User,
            vec![DomainMessageContent::ImageUrl {
                url: "https://example.test/a.png".to_string(),
            }],
        );
        assert_eq!(message.first_text(), None);
    }

    #[test]
    fn domain_message_serde_uses_snake_case_roles() {
        let json = serde_json::to_value(DomainMessage::user("hello")).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "role": "user",
                "content": [{ "type": "text", "text": "hello" }],
            })
        );

        for (role, encoded) in [
            (DomainMessageRole::System, "system"),
            (DomainMessageRole::User, "user"),
            (DomainMessageRole::Assistant, "assistant"),
            (DomainMessageRole::Tool, "tool"),
        ] {
            assert_eq!(serde_json::to_value(role).expect("serialize"), encoded);
        }

        assert_eq!(
            serde_json::to_value(DomainMessageContent::ImageUrl {
                url: "https://example.test/a.png".to_string(),
            })
            .expect("serialize"),
            serde_json::json!({ "type": "image_url", "url": "https://example.test/a.png" })
        );
    }

    #[test]
    fn a_message_round_trips_through_serde() {
        let message = DomainMessage::new(
            DomainMessageRole::Assistant,
            vec![DomainMessageContent::Text {
                text: "done".to_string(),
            }],
        );
        let encoded = serde_json::to_string(&message).expect("serialize");
        let decoded: DomainMessage = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, message);
    }
}
