//! Keyed, versioned hashing for idempotency ledger values (plan 03, finding P1-1).
//!
//! # Why this exists
//!
//! [`crate::security::masking::request_hash`] is a plain, unkeyed SHA-256. The values it
//! produced were persisted into `idempotency_records.request_hash` and
//! `.idempotency_key_hash` over request bodies that can contain provider API keys and
//! other credential material (`POST /api/v1/admin/provider-credentials`,
//! `.../system-keys`, `.../consumer-keys`). After a database-only compromise — a leaked
//! backup, a read-replica breach, an SQL-injection read — an attacker holding those
//! digests can offline-verify guesses of the original secret-bearing body, because
//! SHA-256 is fast and the mechanism carries no key. `IdempotencyHasher` closes that by
//! keying every digest with a deployment-held pepper the database never contains.
//!
//! # Output format
//!
//! `"{pepper_version}:{base64url_no_pad(hmac_sha256(pepper, bytes))}"` — e.g.
//! `"v1:9CqQ…"`. With the default `"v1"` version that is `3 + 43 = 46` characters, well
//! inside the `varchar(128)` columns the ledger already uses, which is why this change
//! needs no migration. The base64 alphabet matches
//! [`crate::security::masking::secret_fingerprint`] so stored values stay visually
//! consistent with the fingerprints elsewhere in the schema.
//!
//! # Pepper-rotation contract (deliberately narrower than [`crate::security::ApiKeyHasher`])
//!
//! `ApiKeyHasher` must verify against *previous* peppers, because an API key row lives
//! indefinitely and its `pepper_version` is stored per row. `IdempotencyHasher`
//! deliberately verifies **only against the currently active pepper**: every idempotency
//! record expires within 24 hours (`IDEMPOTENCY_RETENTION_HOURS`, and the matching
//! `Duration::hours(24)` in the public and runtime-admin ledgers), so old-pepper rows age
//! out on their own.
//!
//! The operational consequence, stated plainly: **rotating the idempotency pepper means
//! in-flight (unexpired) claims made under the old pepper stop replay-matching and fall
//! through to normal, non-idempotent processing.** That is a bounded duplicate-processing
//! window of at most the retention period — never a security failure and never a
//! fail-open. [`IdempotencyHasher::verify`] therefore returns `false` for any stored value
//! carrying a *different* version prefix rather than silently accepting it. Operators who
//! care about `/v1/responses` and admin-command replay should avoid rotating this pepper
//! during active traffic, exactly as recommended for the API-key pepper today.
//!
//! ## The retention argument does not transfer to the `content_hash` columns (finding F14)
//!
//! The 24-hour expiry above is what makes "verify only the active pepper" safe, and it is a
//! property of `idempotency_records` alone. This hasher is also the writer for
//! `conversation_messages.content_hash`, `rag_documents.content_hash` and
//! `rag_document_versions.content_hash`, and those rows have no retention at all — nothing
//! ages them out, so a rotation orphans their stored digests permanently rather than for a
//! bounded window.
//!
//! `memory_records.content_hash` used to be written here too and no longer is: plan 11
//! Sub-Phase F compares it (exact-match memory dedupe), and a comparison over a long-lived row
//! is exactly what this contract cannot serve. It moved to
//! [`crate::security::ContentSealer::memory_content_hash`], which wrote the unkeyed
//! [`crate::security::request_hash`] for rows stored in the clear until issue #168 — and since
//! then writes a digest keyed by the keyring's `memory_dedupe` data key under **every** storage
//! policy, because a memory body is guessable enough that an unkeyed digest of it is a
//! dictionary-attack oracle even on a row that stores no body.
//!
//! **That keyed form does not reopen the problem this section describes**,
//! because the key is a wrapped keyring row rather than a deployment pepper: a master-key
//! rotation re-wraps the envelope and the 32 bytes inside it never change, so every stored
//! digest stays byte-identical. That is the property this hasher cannot offer and the reason
//! the dedupe key lives in the keyring instead of beside this pepper.
//!
//! The remaining three columns written here are **write-only fingerprints**: no code path
//! recomputes and compares them, so an orphaned value costs nothing. Before adding a read that
//! compares any `content_hash` written by this hasher, move that column to a content address
//! first.
//!
//! ## Rotation changes a value the API returns (finding F14)
//!
//! `conversation_messages.content_hash` is **both** written by this hasher **and** returned to
//! callers — it is a field on `ConversationMessageRecord` and appears in `docs/openapi.json`.
//! It stays peppered on purpose: an unkeyed digest of message content, handed out, is an
//! offline verifier against content the schema expects to be able to hold encrypted
//! (`conversation_messages.content_encrypted`). The consequence is worth stating plainly
//! because it is not a leak and is therefore easy to miss:
//!
//! **rotating the idempotency pepper changes the `content_hash` the API returns for a message
//! whose content never changed.** The stored row is not rewritten; the same bytes simply hash
//! to a different value under the new pepper, so messages written before and after a rotation
//! report different digests for identical content. A client treating `content_hash` as a stable
//! content identity — caching on it, diffing on it, deduplicating on it — is relying on
//! something rotation is allowed to break. It is a version-prefixed integrity fingerprint, not
//! a content address, and the `"{pepper_version}:"` prefix on every value is the visible signal
//! that it is scoped to a pepper. `ConversationMessageRecord::content_hash` says so in the
//! schema description; keep the two in step.
//!
//! # Legacy compatibility
//!
//! Values written before this change carry no `:` prefix. [`IdempotencyHasher::verify`]
//! falls back to the legacy unkeyed [`secret_fingerprint`] for those, and
//! [`IdempotencyHasher::legacy_hash`] exposes the same value so callers can *look up* a
//! legacy row: `idempotency_key_hash` is an index key under the unique index on
//! `(idempotency_key_hash, actor_fingerprint, operation)`, not merely a compared value, so
//! dual-verify alone would not preserve replay for pre-deploy rows.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::masking::secret_fingerprint;

type HmacSha256 = Hmac<Sha256>;

/// Separator between the version prefix and the digest. Also the character the
/// configured `pepper_version` must never contain.
const VERSION_SEPARATOR: char = ':';

/// # `Debug` is hand-written, and must stay hand-written
///
/// `#[derive(Debug)]` here prints `pepper` as a byte array, and the whole security value of
/// this type is that the pepper lives *outside* the database. A derived `Debug` puts it back
/// inside the blast radius of anything that formats a value transitively containing this
/// hasher — `AdminCommandRunner`, `AppState`'s constructor arguments, a `tracing` field, a
/// panic message. `tests::debug_redacts_the_pepper_everywhere_it_is_reachable` pins the
/// wiring rather than merely the helper.
#[derive(Clone)]
pub struct IdempotencyHasher {
    pepper: Vec<u8>,
    pepper_version: String,
}

impl std::fmt::Debug for IdempotencyHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The version is safe and genuinely useful in a log line: it says which pepper a
        // stored digest belongs to. The pepper itself is never rendered, not even its length,
        // which would narrow a brute force.
        f.debug_struct("IdempotencyHasher")
            .field("pepper", &"[redacted]")
            .field("pepper_version", &self.pepper_version)
            .finish()
    }
}

impl IdempotencyHasher {
    pub fn new(pepper: impl Into<Vec<u8>>, pepper_version: impl Into<String>) -> Self {
        Self {
            pepper: pepper.into(),
            pepper_version: pepper_version.into(),
        }
    }

    /// The version prefix this hasher writes. Stored values carrying any other prefix are
    /// rejected by [`Self::verify`].
    pub fn pepper_version(&self) -> &str {
        &self.pepper_version
    }

    /// Produces `"{pepper_version}:{base64url(hmac_sha256(pepper, bytes))}"`.
    pub fn hash(&self, bytes: &[u8]) -> String {
        format!(
            "{}{VERSION_SEPARATOR}{}",
            self.pepper_version,
            URL_SAFE_NO_PAD.encode(self.mac(bytes))
        )
    }

    /// The legacy, unkeyed digest for `bytes`, identical to what
    /// [`crate::security::request_hash`] produced before this change.
    ///
    /// Exposed **only** so read paths can perform the dual *lookup* that keeps pre-deploy
    /// rows replayable while they remain unexpired. Never use it to write a new value.
    pub fn legacy_hash(&self, bytes: &[u8]) -> String {
        secret_fingerprint(bytes)
    }

    /// Verifies `bytes` against a stored value written either by this hasher under the
    /// active pepper, or by the legacy unkeyed hash.
    ///
    /// A stored value with a *different* version prefix returns `false`: this hasher holds
    /// only the active pepper and must never accept a digest it cannot actually recompute.
    pub fn verify(&self, bytes: &[u8], stored: &str) -> bool {
        match stored.split_once(VERSION_SEPARATOR) {
            Some((version, digest)) if version == self.pepper_version => {
                let Ok(expected) = URL_SAFE_NO_PAD.decode(digest) else {
                    return false;
                };
                // `verify_slice` is constant-time and length-checked.
                self.mac_instance(bytes).verify_slice(&expected).is_ok()
            }
            Some(_) => false,
            None => secret_fingerprint(bytes) == stored,
        }
    }

    fn mac_instance(&self, bytes: &[u8]) -> HmacSha256 {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.pepper)
            .expect("HMAC accepts a key of any length");
        mac.update(bytes);
        mac
    }

    fn mac(&self, bytes: &[u8]) -> [u8; 32] {
        self.mac_instance(bytes).finalize().into_bytes().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasher() -> IdempotencyHasher {
        IdempotencyHasher::new(b"pepper-one".to_vec(), "v1")
    }

    #[test]
    fn versioned_hash_round_trips_under_the_active_pepper() {
        let hasher = hasher();
        let hashed = hasher.hash(b"{\"api_key\":\"sk-live-secret\"}");

        assert!(hashed.starts_with("v1:"));
        assert!(hasher.verify(b"{\"api_key\":\"sk-live-secret\"}", &hashed));
        assert!(!hasher.verify(b"{\"api_key\":\"sk-live-secre7\"}", &hashed));
    }

    #[test]
    fn hash_output_fits_the_varchar_128_column() {
        // Guards the "no migration needed" claim: `idempotency_records.request_hash`,
        // `.idempotency_key_hash` and every `content_hash` column are varchar(128).
        let hashed = hasher().hash(&vec![7_u8; 4096]);

        assert_eq!(hashed.len(), 46, "\"v1:\" + 43 base64url characters");
        assert!(hashed.len() <= 128);
    }

    #[test]
    fn legacy_unkeyed_hash_still_verifies() {
        let hasher = hasher();
        let legacy = secret_fingerprint(b"pre-deploy body");

        assert!(!legacy.contains(VERSION_SEPARATOR));
        assert!(hasher.verify(b"pre-deploy body", &legacy));
        assert!(!hasher.verify(b"a different body", &legacy));
        assert_eq!(hasher.legacy_hash(b"pre-deploy body"), legacy);
    }

    #[test]
    fn verify_rejects_a_hash_from_a_different_pepper_version() {
        let active = hasher();
        let rotated = IdempotencyHasher::new(b"pepper-one".to_vec(), "v2");
        let stored = rotated.hash(b"body");

        assert!(stored.starts_with("v2:"));
        assert!(
            !active.verify(b"body", &stored),
            "a different version prefix must never be silently accepted"
        );
    }

    #[test]
    fn hash_changes_when_the_pepper_changes() {
        let one = IdempotencyHasher::new(b"pepper-one".to_vec(), "v1");
        let two = IdempotencyHasher::new(b"pepper-two".to_vec(), "v1");

        assert_ne!(one.hash(b"body"), two.hash(b"body"));
        assert!(!two.verify(b"body", &one.hash(b"body")));
        // The whole point of P1-1: the digest is not derivable from the body alone.
        assert_ne!(one.hash(b"body"), secret_fingerprint(b"body"));
    }

    #[test]
    fn hash_never_reveals_plaintext() {
        let hashed = hasher().hash(b"sk-live-super-secret-value");

        assert!(!hashed.contains("sk-live"));
        assert!(!hashed.contains("secret"));
        assert_eq!(hashed, hasher().hash(b"sk-live-super-secret-value"));
    }

    /// Asserts `rendered` discloses `pepper` in **neither** spelling a `Debug` impl can
    /// produce.
    ///
    /// The two-spelling check is the entire point. `#[derive(Debug)]` on a `Vec<u8>` field
    /// prints `[105, 100, 101, ...]`, not the ASCII text, so a test that only searched for
    /// the readable string would stay green against the exact regression it exists to catch —
    /// someone replacing the hand-written impl with a derive. Both spellings are searched, so
    /// neither a `Vec<u8>` nor a `String` pepper field can slip through.
    fn assert_pepper_is_not_disclosed(what: &str, rendered: &str, pepper: &[u8]) {
        let as_text = String::from_utf8_lossy(pepper);
        assert!(
            !rendered.contains(as_text.as_ref()),
            "{what} disclosed its pepper as text: {rendered}"
        );

        let as_bytes = format!("{pepper:?}");
        assert!(
            !rendered.contains(&as_bytes),
            "{what} disclosed its pepper as a byte array — this is what `#[derive(Debug)]` \
             on a `Vec<u8>` actually prints: {rendered}"
        );

        // Whole-value absence is not enough. A `Debug` impl that printed a "safe-looking"
        // prefix — `pepper: "abcd…"`, a fingerprint, the first few bytes for correlation —
        // passes both checks above while still handing out key material: the pepper's whole
        // job is to be unguessable, and every disclosed byte is one an offline attacker no
        // longer has to search. So no non-trivial prefix or suffix may appear either, in
        // text or byte-array spelling.
        //
        // Eight is below any plausible "identifying" excerpt and far above the run length
        // that could collide by chance with ordinary struct text.
        const MAX_DISCLOSED: usize = 8;
        for length in MAX_DISCLOSED..=pepper.len() {
            for window in [&pepper[..length], &pepper[pepper.len() - length..]] {
                let text = String::from_utf8_lossy(window);
                assert!(
                    !rendered.contains(text.as_ref()),
                    "{what} disclosed a {length}-byte run of its pepper as text — a truncated \
                     excerpt is still key material: {rendered}"
                );
                assert!(
                    !rendered.contains(&format!("{window:?}")),
                    "{what} disclosed a {length}-byte run of its pepper as a byte array: \
                     {rendered}"
                );
            }
        }
    }

    /// The **wiring** test for pepper redaction, not the predicate test.
    ///
    /// A predicate test — "`IdempotencyHasher`'s `Debug` redacts its pepper" — is satisfied by
    /// a correct impl on a type nothing logs. What actually leaks a pepper is a `Debug` on
    /// some *container*: `AdminCommandRunner` is formatted by any `tracing` field or panic
    /// message that captures it, `AuthService` lives for the whole process, and `Settings`
    /// holds both peppers as configured, one level above either hasher. Each of those derives
    /// `Debug`, so each is only as safe as the leaf it contains — and that dependency is
    /// invisible at the leaf.
    ///
    /// So this walks the real containers, constructed the way production constructs them, and
    /// requires the pepper to be absent from each. Redacting a leaf and forgetting a container
    /// that reaches the secret by another route fails here; a correct leaf with no container
    /// coverage would not have.
    // `tokio::test` because `PgPool::connect_lazy` and `reqwest::Client` both require a
    // runtime to exist. Neither performs any I/O here: the pool never opens a connection and
    // the JWKS cache never fetches.
    #[tokio::test]
    async fn debug_redacts_the_pepper_everywhere_it_is_reachable() {
        use crate::{
            application::AdminCommandRunner,
            config::{JwksFetchSettings, Settings},
            infra::repositories::PgAdminRepository,
            security::{ApiKeyHasher, AuthService, JwksCache},
        };

        const IDEMPOTENCY_PEPPER: &[u8] = b"idempotency-pepper-that-must-never-be-printed";
        const API_KEY_PEPPER: &[u8] = b"api-key-pepper-that-must-never-be-printed";
        const CONFIGURED_IDEMPOTENCY_PEPPER: &str = "Y29uZmlndXJlZC1pZGVtcG90ZW5jeS1wZXBwZXI";
        const CONFIGURED_API_KEY_PEPPER: &str = "Y29uZmlndXJlZC1hcGkta2V5LXBlcHBlcg";

        // ---- the leaves ----
        let idempotency_hasher = IdempotencyHasher::new(IDEMPOTENCY_PEPPER.to_vec(), "v1");
        let rendered = format!("{idempotency_hasher:?}");
        assert_pepper_is_not_disclosed("IdempotencyHasher", &rendered, IDEMPOTENCY_PEPPER);
        // The version *is* disclosed on purpose: it names which pepper a stored digest
        // belongs to and is not itself secret. Asserting it keeps the redaction from being
        // "fixed" by making `Debug` uselessly opaque.
        assert!(
            rendered.contains("v1"),
            "the pepper version must stay visible"
        );

        let key_hasher = ApiKeyHasher::new(API_KEY_PEPPER.to_vec(), "v1", 20);
        assert_pepper_is_not_disclosed("ApiKeyHasher", &format!("{key_hasher:?}"), API_KEY_PEPPER);

        // ---- the containers that actually get logged ----

        // `connect_lazy` builds the pool object without opening a connection, so the real
        // `AdminCommandRunner` — the one every admin mutation goes through — is constructible
        // here with no database.
        let pool = sqlx::PgPool::connect_lazy("postgres://moira:moira@127.0.0.1:5432/moira")
            .expect("a lazy pool must not require a reachable database");
        let runner = AdminCommandRunner::new(
            PgAdminRepository::new(pool),
            IdempotencyHasher::new(IDEMPOTENCY_PEPPER.to_vec(), "v1"),
        );
        assert_pepper_is_not_disclosed(
            "AdminCommandRunner",
            &format!("{runner:?}"),
            IDEMPOTENCY_PEPPER,
        );

        let auth = AuthService::new(
            Default::default(),
            Default::default(),
            ApiKeyHasher::new(API_KEY_PEPPER.to_vec(), "v1", 20),
            JwksCache::new(JwksFetchSettings::default())
                .expect("the jwks cache must build its client"),
        );
        assert_pepper_is_not_disclosed("AuthService", &format!("{auth:?}"), API_KEY_PEPPER);

        // `Settings` reaches both peppers in their *configured* form, before either hasher
        // exists. Redacting only the hashers would leave this one printing them.
        let mut settings = Settings::default();
        settings.api_keys.pepper_base64 = Some(CONFIGURED_API_KEY_PEPPER.to_string());
        settings.idempotency.pepper_base64 = Some(CONFIGURED_IDEMPOTENCY_PEPPER.to_string());
        let rendered = format!("{settings:?}");
        assert_pepper_is_not_disclosed(
            "Settings.api_keys",
            &rendered,
            CONFIGURED_API_KEY_PEPPER.as_bytes(),
        );
        assert_pepper_is_not_disclosed(
            "Settings.idempotency",
            &rendered,
            CONFIGURED_IDEMPOTENCY_PEPPER.as_bytes(),
        );
        // Whether a pepper is configured at all stays visible — the dev-fallback warnings and
        // `Settings::validate` both turn on it, and it is not the secret.
        assert!(
            rendered.contains("[redacted]"),
            "a configured pepper must render as a redaction marker, not vanish: {rendered}"
        );
    }
}
