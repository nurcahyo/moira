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

#[derive(Debug, Clone)]
pub struct IdempotencyHasher {
    pepper: Vec<u8>,
    pepper_version: String,
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
}
