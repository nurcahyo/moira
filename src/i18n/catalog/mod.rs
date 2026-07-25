//! Moira i18n catalog index.
//!
//! Directory guide:
//! - `README.md` explains the layout for agents and contributors.
//! - `errors.rs` owns `moira.error.*` entries.
//! - `notices.rs` owns `moira.notice.*` entries.
//! - `mod.rs` provides lookups and catalog-level validation.
//!
//! Keep the docs mirror in `docs/i18n-response-catalog.json` synchronized with this code.

mod errors;
mod notices;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I18nEntry {
    pub key: &'static str,
    pub default_message: &'static str,
    pub description: &'static str,
}

pub use errors::RESPONSE_ERROR_CATALOG;
pub use notices::RESPONSE_NOTICE_CATALOG;

pub fn all_entries() -> impl Iterator<Item = &'static I18nEntry> {
    RESPONSE_ERROR_CATALOG
        .iter()
        .chain(RESPONSE_NOTICE_CATALOG.iter())
}

pub fn is_known_key(key: &str) -> bool {
    all_entries().any(|entry| entry.key == key)
}

pub fn default_message_for_key(key: &str) -> Option<&'static str> {
    all_entries()
        .find(|entry| entry.key == key)
        .map(|entry| entry.default_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_catalog_keys_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in all_entries() {
            assert!(seen.insert(entry.key), "duplicate key: {}", entry.key);
        }
    }

    #[test]
    fn default_messages_can_be_resolved() {
        assert_eq!(
            default_message_for_key("moira.error.bad_request"),
            Some("The request could not be processed.")
        );
        assert_eq!(
            default_message_for_key("moira.notice.response_completed"),
            Some("The response completed successfully.")
        );
        assert_eq!(default_message_for_key("moira.error.unknown"), None);
    }

    #[test]
    fn idempotency_in_progress_key_is_catalogued() {
        assert!(is_known_key("moira.error.idempotency_in_progress"));
        let entry = all_entries()
            .find(|entry| entry.key == "moira.error.idempotency_in_progress")
            .expect("moira.error.idempotency_in_progress must be catalogued");
        assert!(
            !entry.default_message.is_empty(),
            "default_message must be non-empty"
        );
        assert!(
            !entry.description.is_empty(),
            "description must be non-empty"
        );
    }

    #[test]
    fn idempotency_keys_are_catalogued_exactly_once() {
        for key in [
            "moira.error.idempotency_conflict",
            "moira.error.idempotency_in_progress",
        ] {
            let count = RESPONSE_ERROR_CATALOG
                .iter()
                .filter(|entry| entry.key == key)
                .count();
            assert_eq!(
                count, 1,
                "{key} must appear exactly once in RESPONSE_ERROR_CATALOG"
            );
        }
    }

    #[test]
    fn every_catalog_key_appears_exactly_once() {
        // Drift guard: with the docs mirror's duplicate `idempotency_conflict` /
        // `rate_limited` objects removed (plan 04), every key in the combined
        // error + notice catalog must now be unique with no per-key carve-out.
        let mut seen = std::collections::HashMap::new();
        for entry in all_entries() {
            *seen.entry(entry.key).or_insert(0) += 1;
        }
        let duplicates: Vec<_> = seen
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(key, count)| format!("{key} ({count}x)"))
            .collect();
        assert!(
            duplicates.is_empty(),
            "catalog keys must be unique, found duplicates: {duplicates:?}"
        );
    }

    #[test]
    fn middleware_error_keys_are_catalogued() {
        for key in [
            "moira.error.request_timeout",
            "moira.error.payload_too_large",
            "moira.error.jwks_url_rejected",
            "moira.error.internal_error",
            "moira.error.unauthorized",
        ] {
            assert!(is_known_key(key), "{key} must be a known catalog key");
            let entry = all_entries()
                .find(|entry| entry.key == key)
                .unwrap_or_else(|| panic!("{key} must be catalogued"));
            assert!(
                !entry.default_message.is_empty(),
                "{key} default_message must be non-empty"
            );
            assert!(
                !entry.description.is_empty(),
                "{key} description must be non-empty"
            );
        }
    }

    #[test]
    fn pagination_and_precondition_error_keys_are_present_in_the_catalog() {
        assert!(is_known_key("moira.error.invalid_cursor"));
        assert!(is_known_key("moira.error.if_match_required"));
    }

    #[test]
    fn new_catalog_entries_have_non_empty_default_messages_and_descriptions() {
        for key in [
            "moira.error.invalid_cursor",
            "moira.error.if_match_required",
        ] {
            assert_eq!(
                default_message_for_key(key),
                Some(match key {
                    "moira.error.invalid_cursor" => "The pagination cursor is invalid.",
                    "moira.error.if_match_required" =>
                        "The If-Match header is required for this request.",
                    _ => unreachable!(),
                })
            );
            let entry = all_entries()
                .find(|entry| entry.key == key)
                .unwrap_or_else(|| panic!("{key} must be catalogued"));
            assert!(
                !entry.default_message.is_empty(),
                "{key} default_message must be non-empty"
            );
            assert!(
                !entry.description.is_empty(),
                "{key} description must be non-empty"
            );
        }
    }
}
