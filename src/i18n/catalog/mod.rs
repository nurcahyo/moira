//! Moira i18n catalog index.
//!
//! Directory guide:
//! - `README.md` explains the layout for agents and contributors.
//! - `errors.rs` owns `moira.error.*` entries.
//! - `notices.rs` owns `moira.notice.*` entries.
//! - `mod.rs` provides lookups and catalog-level validation.
//!
//! Keep the docs mirror in `docs/i18n-response-catalog.json` synchronized with this code.
//! That synchronization is no longer a convention: `docs_mirror_matches_rust_catalog`
//! below reads the JSON at test time and fails on any divergence, so the mirror cannot
//! drift silently the way it did before plan 06.
//!
//! **The `moira.notice.*` half of this catalog has no production consumers.** All four
//! entries in `notices.rs` are referenced only by this module's tests and by
//! `src/domain/i18n.rs`; nothing in `src/http`, `src/application` or `src/orchestration`
//! emits a notice key today. The next plan that returns a user-visible success string
//! (plan 07 is the first candidate) will be the catalog's first real notice consumer and
//! should expect to add the emission path, not just the entry.

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

// ---------------------------------------------------------------------------
// Compile-time gate: every execution failure class has a catalog entry.
//
// `AppError::coded(status, ExecutionFailureClass::code(class), …)` on the public
// execution path means every variant of that enum ships to clients as
// `moira.error.<code>` (`AppError::message_key`). The source walker in this
// module's tests can only see *literal* code arguments, so it never saw any of
// them — which is how 23 of the 28 shipped as bare keys with no English
// fallback.
//
// The check below closes that for good, and does it at compile time rather than
// test time: `ExecutionFailureClass::ALL` is generated from the same list as the
// enum itself (`src/domain/runtime.rs`), so a new variant is automatically in
// scope here, and if its code has no entry in `errors.rs` the crate does not
// build. The `const fn`s exist because `is_known_key` cannot run in a `const`
// block and `format!` is not available there either — hence the prefix-aware
// byte comparison instead of building `"moira.error." + code`.
// ---------------------------------------------------------------------------

const ERROR_KEY_PREFIX: &[u8] = b"moira.error.";

/// True when `key` is exactly `moira.error.` followed by `code`.
const fn error_key_matches_code(key: &str, code: &str) -> bool {
    let key = key.as_bytes();
    let code = code.as_bytes();
    if key.len() != ERROR_KEY_PREFIX.len() + code.len() {
        return false;
    }
    let mut index = 0;
    while index < ERROR_KEY_PREFIX.len() {
        if key[index] != ERROR_KEY_PREFIX[index] {
            return false;
        }
        index += 1;
    }
    let mut offset = 0;
    while offset < code.len() {
        if key[ERROR_KEY_PREFIX.len() + offset] != code[offset] {
            return false;
        }
        offset += 1;
    }
    true
}

/// `const`-evaluable equivalent of `is_known_key(&format!("moira.error.{code}"))`.
const fn error_catalog_contains_code(code: &str) -> bool {
    let mut index = 0;
    while index < RESPONSE_ERROR_CATALOG.len() {
        if error_key_matches_code(RESPONSE_ERROR_CATALOG[index].key, code) {
            return true;
        }
        index += 1;
    }
    false
}

const _: () = {
    let classes = crate::domain::ExecutionFailureClass::ALL;
    let mut index = 0;
    while index < classes.len() {
        assert!(
            error_catalog_contains_code(classes[index].code()),
            "an ExecutionFailureClass variant has no `moira.error.<code>` entry in \
             src/i18n/catalog/errors.rs — every class reaches clients as a message key, \
             so it needs an English default_message and a description (CONVENTIONS.md §4.1). \
             Run `cargo test --lib i18n::catalog::tests::every_execution_failure_class_code_has_a_catalog_entry` \
             to be told which one."
        );
        index += 1;
    }
};

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

    /// Plan 07's error keys, named exactly (CONVENTIONS §4.5). D1 cuts the
    /// setup-token credential path, so the four token-lifecycle codes the plan's body
    /// once named (`setup_token_invalid`/`_expired`/`_consumed`/`_target_mismatch`) and
    /// the unemittable `auth_provider_method_unsupported` (§0.7 Wave 2) are deliberately
    /// **absent** here — listing them would pin codes nothing in this tree ever raises.
    /// `setup_token_not_supported` replaces them as D1's one refusal code.
    #[test]
    fn identity_error_keys_exist_in_the_catalog() {
        for key in [
            "moira.error.unregistered_trusted_issuer",
            "moira.error.admin_claim_email_required",
            "moira.error.admin_claim_email_not_verified",
            "moira.error.admin_claim_domain_not_allowed",
            "moira.error.admin_identity_already_claimed",
            "moira.error.setup_claim_credential_required",
            "moira.error.setup_token_not_supported",
            "moira.error.auth_provider_not_found",
            "moira.error.duplicate_auth_provider",
            "moira.error.auth_provider_method_config_incomplete",
            "moira.error.auth_provider_url_not_allowed",
            "moira.error.console_issuer_must_not_assert_scopes",
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

    #[test]
    fn identity_notice_keys_exist_in_the_catalog() {
        for key in [
            "moira.notice.admin_identity_claimed",
            // `setup_token_issued` is named by the plan's i18n table alongside the
            // claimed notice, but D1 cuts `mint_setup_token` entirely — there is no
            // emitter, and a catalog entry with no emitter is worse than the gap
            // (§0.7 Wave 2's own rule for `auth_provider_method_unsupported`). It is
            // deliberately not asserted here.
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

    /// **D7 regression guard.** The client-secret error codes an earlier draft of plan
    /// 07 specified must never reappear in the catalog: their existence would imply a
    /// secret-handling code path that decision D7 deleted. If one of these keys is ever
    /// added back, it means someone reintroduced the secret envelope this plan removed.
    #[test]
    fn no_auth_provider_client_secret_keys_exist_in_the_catalog() {
        for key in [
            "moira.error.auth_provider_secret_required",
            "moira.error.auth_provider_secret_not_supported",
            "moira.error.auth_provider_secret_rebind_required",
        ] {
            assert!(
                !is_known_key(key),
                "{key} must not exist: decision D7 removed the OAuth client secret \
                 from Moira entirely, so no code describing a secret condition on \
                 auth_provider_settings may be catalogued"
            );
        }

        let mirror = read_docs_mirror();
        for entry in &mirror.entries {
            for key in [
                "moira.error.auth_provider_secret_required",
                "moira.error.auth_provider_secret_not_supported",
                "moira.error.auth_provider_secret_rebind_required",
            ] {
                assert_ne!(
                    entry.key, key,
                    "{key} must not exist in docs/i18n-response-catalog.json either"
                );
            }
        }
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

    /// Plan 05's binding name for the property over the entries plan 05 itself
    /// added. It is deliberately a separate test from
    /// `new_catalog_entries_have_non_empty_default_messages_and_descriptions`
    /// (which pins plan 04's two pagination/precondition keys) and asserts more
    /// than `every_infrastructure_catalog_entry_has_a_non_empty_default_message_and_description`:
    /// the text must survive trimming, must resolve through the public
    /// `default_message_for_key` lookup, and the description must not be a copy
    /// of the message — a whitespace-only or duplicated string is non-empty but
    /// is not a usable catalog entry.
    #[test]
    fn every_new_catalog_entry_has_a_non_empty_default_message_and_description() {
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
                !entry.default_message.trim().is_empty(),
                "{key} default_message must be non-empty"
            );
            assert!(
                !entry.description.trim().is_empty(),
                "{key} description must be non-empty"
            );
            assert_ne!(
                entry.default_message, entry.description,
                "{key} description must explain when the key is used, not repeat the message"
            );
            assert_eq!(
                default_message_for_key(key),
                Some(entry.default_message),
                "{key} must resolve through the public lookup"
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

    // -----------------------------------------------------------------------
    // Docs-mirror gates (plan 06, module 10c)
    //
    // Until these landed, *nothing* in `src/` or `tests/` read
    // `docs/i18n-response-catalog.json`. The mirror is the artifact translators
    // and the console consume, and it could diverge from the Rust catalog
    // arbitrarily without a single test noticing — the two duplicate objects
    // plan 02b removed were found by hand, not by CI. These three tests are the
    // gate that closes that hole.
    // -----------------------------------------------------------------------

    const DOCS_MIRROR_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/i18n-response-catalog.json"
    );

    #[derive(serde::Deserialize)]
    struct MirrorEntry {
        key: String,
        default_message: String,
        description: String,
    }

    #[derive(serde::Deserialize)]
    struct DocsMirror {
        entries: Vec<MirrorEntry>,
    }

    fn read_docs_mirror() -> DocsMirror {
        let raw = std::fs::read_to_string(DOCS_MIRROR_PATH)
            .unwrap_or_else(|error| panic!("read {DOCS_MIRROR_PATH}: {error}"));
        serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("parse {DOCS_MIRROR_PATH} as JSON: {error}"))
    }

    /// The mirror in `docs/` must be byte-equivalent to the Rust catalog on all
    /// three fields, not merely key-compatible: a translator working from a
    /// stale `default_message` ships a wrong string with a correct key, which is
    /// the failure mode a key-only comparison cannot see.
    #[test]
    fn docs_mirror_matches_rust_catalog() {
        use std::collections::BTreeMap;

        let mirror = read_docs_mirror();
        let mirror_by_key: BTreeMap<&str, (&str, &str)> = mirror
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.key.as_str(),
                    (entry.default_message.as_str(), entry.description.as_str()),
                )
            })
            .collect();
        let rust_by_key: BTreeMap<&str, (&str, &str)> = all_entries()
            .map(|entry| (entry.key, (entry.default_message, entry.description)))
            .collect();

        let mut problems: Vec<String> = Vec::new();
        for key in rust_by_key.keys() {
            if !mirror_by_key.contains_key(key) {
                problems.push(format!(
                    "{key}: present in the Rust catalog, missing from docs/i18n-response-catalog.json"
                ));
            }
        }
        for key in mirror_by_key.keys() {
            if !rust_by_key.contains_key(key) {
                problems.push(format!(
                    "{key}: present in docs/i18n-response-catalog.json, missing from the Rust catalog"
                ));
            }
        }
        for (key, (rust_message, rust_description)) in &rust_by_key {
            let Some((mirror_message, mirror_description)) = mirror_by_key.get(key) else {
                continue;
            };
            if rust_message != mirror_message {
                problems.push(format!(
                    "{key}: default_message differs\n  rust:   {rust_message:?}\n  mirror: {mirror_message:?}"
                ));
            }
            if rust_description != mirror_description {
                problems.push(format!(
                    "{key}: description differs\n  rust:   {rust_description:?}\n  mirror: {mirror_description:?}"
                ));
            }
        }

        assert!(
            problems.is_empty(),
            "docs/i18n-response-catalog.json has drifted from the Rust catalog \
             ({} problem(s)):\n{}",
            problems.len(),
            problems.join("\n")
        );
    }

    /// JSON objects with a repeated `key` parse happily and are invisible to a
    /// set comparison that keeps only the last one. This is the assertion that
    /// would have caught the duplicated `idempotency_conflict` / `rate_limited`
    /// objects plan 02b removed by hand.
    #[test]
    fn docs_mirror_has_no_duplicate_keys() {
        use std::collections::BTreeMap;

        let mirror = read_docs_mirror();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for entry in &mirror.entries {
            *counts.entry(entry.key.as_str()).or_insert(0) += 1;
        }
        let duplicates: Vec<String> = counts
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(key, count)| format!("{key} ({count}x)"))
            .collect();

        assert!(
            duplicates.is_empty(),
            "docs/i18n-response-catalog.json contains duplicate keys: {}",
            duplicates.join(", ")
        );
        assert_eq!(
            mirror.entries.len(),
            all_entries().count(),
            "docs/i18n-response-catalog.json entry count must equal the Rust catalog's"
        );
    }

    /// The three `AppError` constructors that take a caller-supplied error code,
    /// paired with the zero-based argument position that code occupies.
    /// `coded` also takes a `StatusCode` first; `conflict` and `unprocessable`
    /// fix the status themselves.
    ///
    /// The needles are assembled with `concat!` on purpose: this file is itself
    /// walked by the scanner below, and a literal `AppError`-plus-`::coded(`
    /// spelled out here would be found in its own source and parsed as a call.
    const CODED_CONSTRUCTORS: &[(&str, usize)] = &[
        (concat!("AppError", "::coded("), 1),
        (concat!("AppError", "::conflict("), 0),
        (concat!("AppError", "::unprocessable("), 0),
    ];

    /// Every `.rs` file under `src/`, in a deterministic (sorted, depth-first)
    /// order so a failure names the same file on every machine.
    fn rust_sources_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                rust_sources_under(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// Splits the comma-separated top-level arguments of a call, given the slice
    /// of the source that begins at the call's opening `(`. String literals are
    /// tracked so a comma or a bracket inside a message never splits an
    /// argument. Returns whatever it has if the call is never closed, which
    /// cannot happen in a file that compiles.
    fn top_level_args(from_open_paren: &str) -> Vec<String> {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut current = String::new();
        let mut args = Vec::new();

        for character in from_open_paren.chars() {
            if in_string {
                current.push(character);
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => {
                    in_string = true;
                    current.push(character);
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    if depth > 1 {
                        current.push(character);
                    }
                }
                ')' | ']' | '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        args.push(current.trim().to_string());
                        return args;
                    }
                    current.push(character);
                }
                ',' if depth == 1 => {
                    args.push(current.trim().to_string());
                    current.clear();
                }
                _ => current.push(character),
            }
        }
        args
    }

    /// Returns the contents of `argument` when it is a plain `"snake_case"`
    /// string literal, and `None` when the code is computed at runtime.
    fn string_literal_code(argument: &str) -> Option<&str> {
        let inner = argument.strip_prefix('"')?.strip_suffix('"')?;
        if inner.is_empty()
            || !inner.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
            })
        {
            return None;
        }
        Some(inner)
    }

    /// `AppError::coded`/`conflict`/`unprocessable` codes reach the wire as
    /// `moira.error.<code>` (`AppError::message_key`, `src/error.rs`), so every
    /// one of them needs a catalog entry — but no test looked at them until now,
    /// which is exactly how `routing_policy_provider_model_mismatch` shipped
    /// uncatalogued from `src/application/runtime_admin.rs`.
    ///
    /// `validate_override` forwards a `code: &'static str` parameter into `AppError::coded`, so the
    /// literal scanner cannot see the codes — they live at the *call sites*, not at the constructor.
    ///
    /// That blind spot is not hypothetical: four of the five (`route_`, `model_`, `provider_` and
    /// `credential_override_forbidden`) shipped to clients as `moira.error.<code>` message keys with
    /// no catalog entry, and every existing gate was green the whole time. This walks the call sites
    /// directly so a sixth caller with a new code cannot repeat it.
    #[test]
    fn every_validate_override_code_has_a_catalog_entry() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/application/public.rs"),
        )
        .expect("read src/application/public.rs");

        // The last string literal before each call's closing paren is the `code` argument.
        let mut codes: Vec<String> = Vec::new();
        for (index, _) in source.match_indices("validate_override(") {
            // Skip the definition — its body contains `AppError::coded(…, code, "<message>")`, and
            // treating it as a call site harvests that human message as though it were a code.
            if source[..index].trim_end().ends_with("fn") {
                continue;
            }
            let call = &source[index + "validate_override(".len()..];
            // Terminate on the statement's `;`, not on `);` — these calls end `)?;`, and matching
            // the wrong terminator silently swallows the next statement's literals.
            let Some(end) = call.find(';') else { continue };
            let args = &call[..end];
            if let Some(last) = args.rmatch_indices('"').nth(1) {
                let start = last.0 + 1;
                if let Some(close) = args[start..].find('"') {
                    codes.push(args[start..start + close].to_string());
                }
            }
        }

        assert!(
            codes.len() >= 5,
            "expected to find the `validate_override` call sites and their literal codes; found \
             {} — the parser above has probably drifted from the call shape, which would make this \
             gate silently vacuous",
            codes.len()
        );

        let missing: Vec<&String> = codes
            .iter()
            .filter(|code| !error_catalog_contains_code(code))
            .collect();
        assert!(
            missing.is_empty(),
            "these codes are passed to `validate_override` and reach clients as \
             `moira.error.<code>` message keys, but have no catalog entry (CONVENTIONS §4.1): {:?}",
            missing
        );
    }

    /// Codes that are *not* string literals cannot be resolved by reading the
    /// source, so they are collected separately and pinned: a new dynamic code
    /// site must be a deliberate, reviewed decision rather than a silent way out
    /// of this gate. See the note on the final assertion.
    #[test]
    fn every_coded_error_literal_in_src_has_a_catalog_entry() {
        use std::collections::{BTreeMap, BTreeSet};

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_sources_under(&manifest.join("src"), &mut files);
        assert!(
            files.len() > 20,
            "the source walker found only {} files under src/ — it is broken, \
             and a broken walker asserts nothing",
            files.len()
        );

        let mut literal_codes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut dynamic_sites: Vec<String> = Vec::new();

        for file in &files {
            let source = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            let relative = file
                .strip_prefix(manifest)
                .unwrap_or(file)
                .display()
                .to_string();

            for (needle, code_position) in CODED_CONSTRUCTORS {
                let mut cursor = 0usize;
                while let Some(offset) = source[cursor..].find(needle) {
                    let start = cursor + offset;
                    let open = start + needle.len() - 1;
                    cursor = start + needle.len();

                    // A mention inside a `//` comment is prose, not a call site.
                    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
                    if source[line_start..start].contains("//") {
                        continue;
                    }

                    let args = top_level_args(&source[open..]);
                    let Some(argument) = args.get(*code_position) else {
                        dynamic_sites.push(format!("{relative}: {needle} (unparsed arguments)"));
                        continue;
                    };
                    match string_literal_code(argument) {
                        Some(code) => {
                            literal_codes
                                .entry(code.to_string())
                                .or_default()
                                .insert(relative.clone());
                        }
                        None => {
                            dynamic_sites.push(format!("{relative}: {needle}…, {argument}, …)"))
                        }
                    }
                }
            }
        }

        assert!(
            literal_codes.len() > 40,
            "expected the whole `AppError::coded`/`conflict`/`unprocessable` surface \
             (~53 distinct codes), found {} — the scanner is not matching",
            literal_codes.len()
        );

        let missing: Vec<String> = literal_codes
            .iter()
            .filter(|(code, _)| !is_known_key(&format!("moira.error.{code}")))
            .map(|(code, files)| {
                format!(
                    "moira.error.{code} (emitted in {})",
                    files.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            })
            .collect();

        assert!(
            missing.is_empty(),
            "these error codes are emitted from src/ but have no catalog entry \
             in src/i18n/catalog/errors.rs (CONVENTIONS §4.1):\n  {}",
            missing.join("\n  ")
        );

        // Runtime-computed codes cannot be resolved by reading the source, so
        // they are only safe if some *other* gate enumerates their possible
        // values. Plan 06 pinned the count of such sites at 2, which proved
        // nothing: the 23 uncatalogued execution failure codes sat behind
        // exactly those two sites and the pin was green throughout.
        //
        // The pin is replaced by an allowlist of *expressions*. Each entry names
        // a computed code together with the gate that covers its value set. A
        // new dynamic site spelled any other way fails here and has to either
        // become a literal or bring its own exhaustive gate.
        const COVERED_DYNAMIC_CODE_EXPRESSIONS: &[&str] = &[
            // Covered by the `const` block above, which walks
            // `ExecutionFailureClass::ALL` and refuses to compile when a variant's
            // code has no catalog entry.
            //
            // Matched on the function name, not the whole call expression: the argument is spelled
            // `failure.class` at two sites and `value.class` at a third, and renaming a local
            // should not silently drop a site out of this gate's coverage — or, worse, break the
            // build for a reason that has nothing to do with i18n. `failure_code` is a thin
            // delegate to `ExecutionFailureClass::code`, so *every* call is covered by the
            // exhaustive walk regardless of how its argument is written.
            "failure_code(",
            // `validate_override` takes `code: &'static str` and passes it straight to
            // `AppError::coded`. Its five call sites all pass literals, but those literals sit
            // outside any `AppError::coded(` call, so the literal scanner above never sees them —
            // which is exactly how four of the five (`route_`, `model_`, `provider_`,
            // `credential_override_forbidden`) reached clients with no catalog entry until this
            // gate was written. Covered by
            // `every_validate_override_code_has_a_catalog_entry` below, which walks the call sites
            // directly. Adding a sixth caller with a new code fails that test.
            //
            // Matched on the rendered argument, not the callee: the site text is rendered without
            // its enclosing function, and spelling the constructor literally here would be found
            // by this very scanner when it walks this file.
            "…, code, …",
        ];

        let uncovered: Vec<&String> = dynamic_sites
            .iter()
            .filter(|site| {
                !COVERED_DYNAMIC_CODE_EXPRESSIONS
                    .iter()
                    .any(|expression| site.contains(expression))
            })
            .collect();

        assert!(
            uncovered.is_empty(),
            "these error codes are computed at runtime and no exhaustive gate covers \
             their possible values, so nothing proves they have catalog entries \
             (CONVENTIONS §4.1). Either use a literal code, or enumerate the source \
             type the way `ExecutionFailureClass::ALL` is enumerated above:\n  {}",
            uncovered
                .iter()
                .map(|site| site.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );

        // The allowlist must stay honest in the other direction too: an entry
        // that matches nothing is a stale carve-out hiding behind a passing test.
        for expression in COVERED_DYNAMIC_CODE_EXPRESSIONS {
            assert!(
                dynamic_sites.iter().any(|site| site.contains(expression)),
                "{expression:?} is allowlisted as a covered runtime-computed code site \
                 but no such site exists in src/ any more — remove the carve-out"
            );
        }
    }

    /// The runtime companion to the `const` gate above.
    ///
    /// The `const` version is the one that matters — it fails the *build* — but
    /// a const panic cannot interpolate, so it cannot say which variant is
    /// missing. This one can, and it is what a developer should run when the
    /// build breaks.
    #[test]
    fn every_execution_failure_class_code_has_a_catalog_entry() {
        use crate::domain::ExecutionFailureClass;

        let missing: Vec<&str> = ExecutionFailureClass::ALL
            .iter()
            .map(|class| class.code())
            .filter(|code| !is_known_key(&format!("moira.error.{code}")))
            .collect();

        assert!(
            missing.is_empty(),
            "these execution failure classes reach clients as moira.error.<code> but \
             have no entry in src/i18n/catalog/errors.rs:\n  {}",
            missing.join("\n  ")
        );

        for class in ExecutionFailureClass::ALL {
            let key = format!("moira.error.{}", class.code());
            let entry = all_entries()
                .find(|entry| entry.key == key)
                .unwrap_or_else(|| panic!("{key} must be catalogued"));
            assert!(
                !entry.default_message.trim().is_empty(),
                "{key} default_message must be non-empty"
            );
            assert!(
                !entry.description.trim().is_empty(),
                "{key} description must be non-empty"
            );
            assert_ne!(
                entry.default_message, entry.description,
                "{key} description must explain when the key is used, not repeat the message"
            );
        }
    }

    /// `ExecutionFailureClass::code` and the enum's `serde` representation must
    /// agree, because callers see both: `code` becomes the `error.code` and the
    /// `moira.error.*` key, while `serde` produces the `failure.class` string in
    /// the execution summary. If they diverged, the catalog gate would be
    /// guarding a vocabulary clients never see.
    #[test]
    fn execution_failure_class_codes_match_their_serde_representation() {
        use crate::domain::ExecutionFailureClass;
        use std::collections::BTreeSet;

        let mut seen = BTreeSet::new();
        for class in ExecutionFailureClass::ALL {
            let serialized =
                serde_json::to_value(class).expect("ExecutionFailureClass serializes to JSON");
            assert_eq!(
                serialized,
                serde_json::Value::String(class.code().to_string()),
                "{class:?}: code() and the serde representation disagree"
            );
            assert!(
                seen.insert(class.code()),
                "{:?} duplicates the code {:?}",
                class,
                class.code()
            );
        }
    }
}
