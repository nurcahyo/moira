//! Success and notice translation keys.
//!
//! Add new `moira.notice.*` entries here when API payloads expose readable notices.

use super::I18nEntry;

pub const RESPONSE_NOTICE_CATALOG: &[I18nEntry] = &[
    I18nEntry {
        key: "moira.notice.response_accepted",
        default_message: "The request was accepted for processing.",
        description: "Used for success acknowledgements that return before execution completes.",
    },
    I18nEntry {
        key: "moira.notice.response_completed",
        default_message: "The response completed successfully.",
        description: "Used when a response finishes successfully.",
    },
    I18nEntry {
        key: "moira.notice.response_persisted",
        default_message: "The response was saved successfully.",
        description: "Used when a response is written to durable storage.",
    },
    I18nEntry {
        key: "moira.notice.stream_started",
        default_message: "Streaming has started.",
        description: "Used when a streaming response is opened successfully.",
    },
];
