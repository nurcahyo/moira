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
    // Plan 07 — the catalog's first production notice consumer. A notice entry
    // exists only where the response actually carries prose a console shows a
    // human: `GET …/setup/claim-status` (a bare boolean), the auth-provider
    // records and `GET …/setup/auth-methods` (pure configuration data) carry
    // none and get none.
    I18nEntry {
        key: "moira.notice.admin_identity_claimed",
        default_message: "Admin access has been granted to this identity.",
        description: "Used on both the 201 of a fresh admin-identity claim and the 200 of an idempotent replay, which returns the stored body verbatim. The status code, not the notice, distinguishes the two.",
    },
];
