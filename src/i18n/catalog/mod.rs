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

    /// The six infrastructure error codes `AppError::code()` already emits
    /// (`src/error.rs:128-144`) but that had no catalog entry until now (plan
    /// 05, Module 6). `configuration_error` in particular is a *new* emitter:
    /// plan 05's OTel fail-fast path (`otel_enabled=true` with no endpoint)
    /// returns `AppError::Config`.
    #[test]
    fn infrastructure_error_keys_are_present_in_the_catalog() {
        for key in [
            "moira.error.configuration_error",
            "moira.error.database_error",
            "moira.error.database_unavailable",
            "moira.error.http_client_error",
            "moira.error.redis_error",
            "moira.error.upstream_error",
        ] {
            assert!(is_known_key(key), "{key} must be a known catalog key");
        }
    }

    #[test]
    fn every_infrastructure_catalog_entry_has_a_non_empty_default_message_and_description() {
        for key in [
            "moira.error.configuration_error",
            "moira.error.database_error",
            "moira.error.database_unavailable",
            "moira.error.http_client_error",
            "moira.error.redis_error",
            "moira.error.upstream_error",
        ] {
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

    /// `moira.error.upstream_error` is the code for `AppError::Upstream` and
    /// must stay distinct from the three `AppError::coded` upstream
    /// *condition* keys (`upstream_bad_response` / `upstream_timeout` /
    /// `upstream_unavailable`) used elsewhere — none may be merged, renamed,
    /// or removed while adding the new key.
    #[test]
    fn upstream_error_is_distinct_from_the_three_existing_upstream_condition_keys() {
        let upstream_keys = [
            "moira.error.upstream_error",
            "moira.error.upstream_bad_response",
            "moira.error.upstream_timeout",
            "moira.error.upstream_unavailable",
        ];
        for key in upstream_keys {
            assert!(is_known_key(key), "{key} must be a known catalog key");
        }
        let unique: std::collections::BTreeSet<_> = upstream_keys.iter().collect();
        assert_eq!(
            unique.len(),
            upstream_keys.len(),
            "the four upstream_* keys must all be distinct"
        );
        // `upstream_error`'s entry must not merely alias one of the three
        // condition entries' wording.
        let upstream_error_message = default_message_for_key("moira.error.upstream_error")
            .expect("moira.error.upstream_error must be catalogued");
        for condition_key in [
            "moira.error.upstream_bad_response",
            "moira.error.upstream_timeout",
            "moira.error.upstream_unavailable",
        ] {
            let condition_message = default_message_for_key(condition_key)
                .unwrap_or_else(|| panic!("{condition_key} must be catalogued"));
            assert_ne!(
                upstream_error_message, condition_message,
                "moira.error.upstream_error must not share default_message with {condition_key}"
            );
        }
    }

    /// Connects to a closed loopback port to obtain a real `reqwest::Error`
    /// without any external network access (reqwest has no public
    /// constructor for `Error`, so this is the only way to produce one from
    /// outside the crate that owns it).
    async fn unreachable_reqwest_error() -> reqwest::Error {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let port = listener.local_addr().expect("read local addr").port();
        // Free the port immediately so nothing is listening on it; connecting
        // to it below must fail fast with "connection refused".
        drop(listener);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("client builds without making a connection");

        client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("connecting to a closed loopback port must fail")
    }

    /// The mechanical guard for `CONVENTIONS.md` §4.1: every `AppError`
    /// variant whose `code()` is fixed by the enum itself (as opposed to a
    /// caller-supplied `AppError::coded`/`AppError::Api` code) is constructed
    /// here and its `message_key` — derived by the real
    /// `format!("moira.error.{}", code())` path in `error_response()`, never
    /// retyped by hand — is checked against the catalog. This is the test
    /// that would have caught the six-code gap this module closes, and it
    /// keeps `§4` honest for this axis until plan 06's systematic drift test
    /// lands.
    #[tokio::test]
    async fn every_error_message_key_resolves_to_a_catalog_entry() {
        use crate::error::AppError;

        let reqwest_error = unreachable_reqwest_error().await;

        let errors: Vec<AppError> = vec![
            AppError::BadRequest("test".to_string()),
            AppError::Unauthorized("test".to_string()),
            AppError::Forbidden("test".to_string()),
            AppError::NotFound("test".to_string()),
            AppError::DatabaseUnavailable,
            AppError::Upstream("test".to_string()),
            AppError::Config("test".to_string()),
            AppError::Internal("test".to_string()),
            AppError::Sqlx(sqlx::Error::RowNotFound),
            AppError::Redis(redis::RedisError::from((redis::ErrorKind::IoError, "test"))),
            AppError::Reqwest(reqwest_error),
        ];

        assert_eq!(
            errors.len(),
            11,
            "this list must cover every fixed-code AppError variant; update it \
             (and this count) whenever a variant is added or removed in src/error.rs"
        );

        for error in errors {
            let response = error.error_response(None);
            assert!(
                is_known_key(&response.error.message_key),
                "message_key {:?} (code {:?}) has no catalog entry",
                response.error.message_key,
                response.error.code
            );
            assert!(
                !response.error.message.is_empty(),
                "message for code {:?} must be non-empty",
                response.error.code
            );
        }
    }
}
