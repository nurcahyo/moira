//! Opaque, tamper-evident list cursors (plan 04, finding P1-4).
//!
//! # What a cursor is here
//!
//! Every `ListResponse<T>`-returning endpoint advertises a `cursor` query parameter and a
//! `pagination.next_cursor` field. This module owns the single codec both sides use, so
//! `domain` can express the pagination key without any dependency back into `infra`.
//!
//! Two shapes cover every list in the codebase, because the *sort column* varies per
//! endpoint but the *key shape* does not:
//!
//! - [`ListCursor`] — `(timestamptz, uuid)`, used by every timestamp-ordered list. The
//!   timestamp is whichever column that endpoint already orders by (`created_at`,
//!   `updated_at`, or `occurred_at` for audit logs); the codec neither knows nor cares
//!   which, and callers must **not** change an endpoint's existing sort key to make it
//!   uniform.
//! - [`SeqCursor`] — a bare `i64`, used by the conversation message list, which orders by
//!   `sequence_number asc` (monotonic and unique per conversation, so it needs no
//!   tiebreaker).
//!
//! # What the integrity tag is, and what it is not
//!
//! The encoded form is `base64url_no_pad(version || kind || body || truncated_hmac)`,
//! matching the `URL_SAFE_NO_PAD` alphabet already used by
//! [`crate::security::masking::secret_fingerprint`].
//!
//! **A cursor is not a secret and not a security boundary.** It is a positional pointer
//! into a result set the caller is already authorised to read, and its contents (`id`,
//! plus a timestamp) are fields the caller just received in the previous page's response
//! body. The tag exists for exactly one reason: to make a mutated cursor **fail closed**
//! with a clean `400 invalid_cursor` instead of producing a confusing SQL error, a
//! silently wrong page, or a cursor from one endpoint quietly "working" on another. It is
//! tamper-*evidence*, not confidentiality, and it must not be treated as either.
//!
//! # Key choice: a compiled-in constant, and what that costs
//!
//! The tag is keyed by [`CURSOR_TAG_KEY`], a fixed constant compiled into the binary.
//! Three richer options were considered and rejected:
//!
//! - **Reuse the idempotency pepper** ([`crate::security::IdempotencyHasher`], plan 03).
//!   Rejected. It would couple a deployment secret with a documented, deliberately narrow
//!   rotation contract to a non-secret positional pointer — rotating the pepper would
//!   silently invalidate every outstanding cursor, a failure mode with no upside. It also
//!   forces the codec to become a stateful value threaded through ~17 call sites instead
//!   of the associated functions the rest of this plan codes against.
//! - **A dedicated pagination secret in `Settings`.** Rejected. Same statefulness cost,
//!   and it buys unforgeability, which is worthless here (see below).
//! - **A process-local random key.** Rejected, and this is the important one: it would be
//!   *strictly worse in production*. Cursors would not survive a restart, and under more
//!   than one replica a cursor minted by replica A would be rejected by replica B —
//!   pagination behind a load balancer would fail intermittently with a `400` that looks
//!   like a client bug. Plan 10 makes Moira multi-replica; buying unforgeability we do not
//!   need at the price of a latent multi-replica outage is a bad trade.
//!
//! **The cost of the constant key, stated plainly: cursors are forgeable.** Anyone reading
//! this source can mint a cursor for any scope. That is deliberate and buys an attacker
//! nothing. A forged cursor only injects a `(ts, id)` (or sequence-number) bound into a
//! query whose `WHERE` clause already carries that endpoint's tenant/actor scoping, so the
//! worst it achieves is seeking to an arbitrary offset *within the caller's own authorised
//! result set* — identical to paging forward normally, at identical cost. Both fields are
//! parsed into typed values (`DateTime<Utc>`, `Uuid`, `i64`) and bound as SQL parameters,
//! never interpolated, so a forged cursor cannot reach the query text. In exchange we get
//! restart-stable and replica-stable cursors with no configuration and no state.
//!
//! If a future requirement genuinely needs unforgeable cursors, the change is local: swap
//! [`CURSOR_TAG_KEY`] for a configured secret and thread a codec value through the call
//! sites. Do not do it speculatively.
//!
//! # Scopes prevent cross-endpoint reuse
//!
//! Encoding and decoding both require a [`CursorScope`] — a per-endpoint label mixed into
//! the tag but never stored in the payload. A cursor minted for `admin.applications`
//! therefore fails to verify against `admin.providers`, even though both are `(timestamp,
//! uuid)` lists. Each call site should declare one `const` and use it for **both** encode
//! and decode; a mismatched pair fails closed and loudly rather than paging wrongly.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use axum::http::StatusCode;

use crate::error::AppError;

type HmacSha256 = Hmac<Sha256>;

/// Domain-separation key for the cursor integrity tag.
///
/// Deliberately a public constant, not a secret — see the module docs. Changing it
/// invalidates every outstanding cursor, so treat it as a format version.
const CURSOR_TAG_KEY: &[u8] = b"moira.pagination.cursor.v1";

/// Bumped only if the wire layout below changes incompatibly. Old cursors then fail closed
/// with `invalid_cursor` instead of decoding into a different meaning.
const CURSOR_FORMAT_VERSION: u8 = 1;

/// Discriminates the two key shapes. Covered by the tag, so it cannot be swapped silently.
const KIND_LIST: u8 = 1;
const KIND_SEQ: u8 = 2;

/// `version || kind`.
const HEADER_LEN: usize = 2;
/// `i64 micros (8) || uuid (16)`.
const LIST_BODY_LEN: usize = 24;
/// `i64 sequence number (8)`.
const SEQ_BODY_LEN: usize = 8;

/// Truncated HMAC-SHA-256. 128 bits is far more than tamper-evidence needs and keeps the
/// encoded cursor short enough to sit comfortably in a query string.
const TAG_LEN: usize = 16;

/// Separates the signed payload from the scope label in the MAC input.
const SCOPE_SEPARATOR: u8 = 0xff;

/// Longest cursor this module can ever produce, plus slack. Checked *before* base64
/// decoding so an oversized value is rejected without allocating for it.
const MAX_ENCODED_LEN: usize = 128;

/// The error every decode failure produces.
///
/// The code suffix must stay exactly `invalid_cursor`: [`AppError`] derives
/// `message_key` as `format!("moira.error.{}", code())`, and the catalog entry is
/// `moira.error.invalid_cursor`.
///
/// The message is deliberately generic and carries no `message_args` — echoing the
/// offending cursor back would be reflected input and would tell the caller nothing
/// actionable.
fn invalid_cursor() -> AppError {
    AppError::coded(
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
        "The pagination cursor is invalid.",
    )
}

/// A per-endpoint label bound into the cursor's integrity tag.
///
/// Declare one `const` per list endpoint and pass the same value to `encode` and `decode`:
///
/// ```
/// use moira::domain::{CursorScope, ListCursor};
///
/// const APPLICATIONS: CursorScope = CursorScope::new("admin.applications");
/// const PROVIDERS: CursorScope = CursorScope::new("admin.providers");
///
/// let cursor = ListCursor::new(chrono::Utc::now(), uuid::Uuid::now_v7());
/// let encoded = cursor.encode(APPLICATIONS);
///
/// assert!(ListCursor::decode(&encoded, APPLICATIONS).is_ok());
/// assert!(ListCursor::decode(&encoded, PROVIDERS).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorScope(&'static str);

impl CursorScope {
    pub const fn new(label: &'static str) -> Self {
        Self(label)
    }

    pub const fn label(self) -> &'static str {
        self.0
    }
}

/// Cursor for a list ordered by `(timestamp, id)`.
///
/// `ts` is whichever timestamp column the endpoint already orders by. Encoded as
/// microseconds since the Unix epoch — Postgres `timestamptz` resolution exactly — so the
/// keyset predicate compares the same value the row carries, with no textual-format or
/// precision drift that could re-return or skip a row at a page boundary.
///
/// Intentionally **not** `Serialize`/`Deserialize`: a cursor reaches the wire only through
/// [`Self::encode`], never as a structured field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListCursor {
    pub ts: DateTime<Utc>,
    pub id: Uuid,
}

impl ListCursor {
    pub fn new(ts: DateTime<Utc>, id: Uuid) -> Self {
        Self { ts, id }
    }

    /// Encodes this cursor for `scope`.
    pub fn encode(&self, scope: CursorScope) -> String {
        let mut signed = Vec::with_capacity(HEADER_LEN + LIST_BODY_LEN);
        signed.push(CURSOR_FORMAT_VERSION);
        signed.push(KIND_LIST);
        signed.extend_from_slice(&self.ts.timestamp_micros().to_be_bytes());
        signed.extend_from_slice(self.id.as_bytes());
        finish(signed, scope)
    }

    /// Decodes a client-supplied cursor issued for `scope`.
    ///
    /// Returns `invalid_cursor` for anything that is not a byte-exact, tag-verified
    /// `ListCursor` for this scope — including a [`SeqCursor`], a cursor for another
    /// endpoint, a truncated or oversized value, and non-base64 input. Never panics.
    pub fn decode(raw: &str, scope: CursorScope) -> Result<Self, AppError> {
        let body = verified_body(raw, KIND_LIST, LIST_BODY_LEN, scope)?;

        let micros = i64::from_be_bytes(
            body[..8]
                .try_into()
                .expect("verified_body guarantees the body length"),
        );
        let ts = DateTime::from_timestamp_micros(micros).ok_or_else(invalid_cursor)?;

        let id_bytes: [u8; 16] = body[8..24]
            .try_into()
            .expect("verified_body guarantees the body length");

        Ok(Self {
            ts,
            id: Uuid::from_bytes(id_bytes),
        })
    }

    /// Convenience for `PageQuery`-style `Option<String>` cursors.
    ///
    /// `None` and an empty/whitespace-only value both mean "first page" — several HTTP
    /// clients emit a bare `?cursor=` for an unset parameter, and treating that as a
    /// malformed cursor would be a gratuitous `400`. Any other value must decode.
    pub fn decode_optional(
        raw: Option<&str>,
        scope: CursorScope,
    ) -> Result<Option<Self>, AppError> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => Self::decode(value, scope).map(Some),
            None => Ok(None),
        }
    }
}

/// Cursor for a list ordered by an ascending, unique `sequence_number`.
///
/// Intentionally **not** `Serialize`/`Deserialize`, for the same reason as [`ListCursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqCursor(pub i64);

impl SeqCursor {
    pub fn new(sequence_number: i64) -> Self {
        Self(sequence_number)
    }

    pub fn sequence_number(self) -> i64 {
        self.0
    }

    /// Encodes this cursor for `scope`.
    pub fn encode(&self, scope: CursorScope) -> String {
        let mut signed = Vec::with_capacity(HEADER_LEN + SEQ_BODY_LEN);
        signed.push(CURSOR_FORMAT_VERSION);
        signed.push(KIND_SEQ);
        signed.extend_from_slice(&self.0.to_be_bytes());
        finish(signed, scope)
    }

    /// Decodes a client-supplied cursor issued for `scope`.
    ///
    /// Returns `invalid_cursor` for anything that is not a byte-exact, tag-verified
    /// `SeqCursor` for this scope — including a [`ListCursor`]. Never panics.
    pub fn decode(raw: &str, scope: CursorScope) -> Result<Self, AppError> {
        let body = verified_body(raw, KIND_SEQ, SEQ_BODY_LEN, scope)?;

        Ok(Self(i64::from_be_bytes(
            body.try_into()
                .expect("verified_body guarantees the body length"),
        )))
    }

    /// See [`ListCursor::decode_optional`].
    pub fn decode_optional(
        raw: Option<&str>,
        scope: CursorScope,
    ) -> Result<Option<Self>, AppError> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => Self::decode(value, scope).map(Some),
            None => Ok(None),
        }
    }
}

/// Appends the integrity tag to an already-built `version || kind || body` prefix and
/// base64url-encodes the result.
fn finish(mut signed: Vec<u8>, scope: CursorScope) -> String {
    let tag: [u8; 32] = mac(&signed, scope).finalize().into_bytes().into();
    signed.extend_from_slice(&tag[..TAG_LEN]);
    URL_SAFE_NO_PAD.encode(signed)
}

/// Decodes, length-checks, header-checks and tag-verifies `raw`, returning the body bytes.
///
/// Every rejection path funnels into the same `invalid_cursor` error: a caller can learn
/// only "this cursor is not valid here", which is all that is actionable.
fn verified_body(
    raw: &str,
    kind: u8,
    body_len: usize,
    scope: CursorScope,
) -> Result<Vec<u8>, AppError> {
    if raw.is_empty() || raw.len() > MAX_ENCODED_LEN {
        return Err(invalid_cursor());
    }

    let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| invalid_cursor())?;

    if bytes.len() != HEADER_LEN + body_len + TAG_LEN {
        return Err(invalid_cursor());
    }
    if bytes[0] != CURSOR_FORMAT_VERSION || bytes[1] != kind {
        return Err(invalid_cursor());
    }

    let (signed, tag) = bytes.split_at(HEADER_LEN + body_len);

    // Constant-time, and length-checked against `TAG_LEN` by the digest crate.
    mac(signed, scope)
        .verify_truncated_left(tag)
        .map_err(|_| invalid_cursor())?;

    Ok(signed[HEADER_LEN..].to_vec())
}

/// MAC over `signed || 0xff || scope`. `signed` is fixed-length for a given `kind` (and
/// `kind` is inside it), so the concatenation is unambiguous and neither the shape nor the
/// scope can be swapped without invalidating the tag.
fn mac(signed: &[u8], scope: CursorScope) -> HmacSha256 {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(CURSOR_TAG_KEY)
        .expect("HMAC accepts a key of any length");
    mac.update(signed);
    mac.update(&[SCOPE_SEPARATOR]);
    mac.update(scope.label().as_bytes());
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: CursorScope = CursorScope::new("test.primary");
    const OTHER_SCOPE: CursorScope = CursorScope::new("test.secondary");

    fn sample_ts() -> DateTime<Utc> {
        DateTime::from_timestamp_micros(1_753_401_600_123_456).expect("in-range timestamp")
    }

    fn sample_id() -> Uuid {
        Uuid::parse_str("018f3a7c-1c2d-7e4f-8a9b-0c1d2e3f4a5b").expect("valid uuid")
    }

    fn sample_list_cursor() -> ListCursor {
        ListCursor::new(sample_ts(), sample_id())
    }

    #[test]
    fn list_cursor_round_trips_through_encode_and_decode() {
        let cursor = sample_list_cursor();
        let encoded = cursor.encode(SCOPE);

        let decoded = ListCursor::decode(&encoded, SCOPE).expect("round trip");

        assert_eq!(decoded, cursor);
        // Microsecond fidelity is load-bearing: Postgres `timestamptz` stores microseconds,
        // and a lossy round trip would re-return or skip a row at a page boundary.
        assert_eq!(decoded.ts.timestamp_micros(), cursor.ts.timestamp_micros());
        assert_eq!(decoded.id, cursor.id);
        // Encoding is deterministic, so `next_cursor` is stable for a given row.
        assert_eq!(encoded, cursor.encode(SCOPE));
    }

    #[test]
    fn list_cursor_round_trips_at_the_edges_of_the_timestamp_range() {
        for ts in [
            DateTime::from_timestamp_micros(0).expect("epoch"),
            DateTime::from_timestamp_micros(-62_135_596_800_000_000).expect("year 1"),
            DateTime::from_timestamp_micros(253_402_300_799_999_999).expect("year 9999"),
        ] {
            let cursor = ListCursor::new(ts, sample_id());
            let decoded = ListCursor::decode(&cursor.encode(SCOPE), SCOPE).expect("round trip");
            assert_eq!(decoded, cursor, "failed for {ts}");
        }
    }

    #[test]
    fn seq_cursor_round_trips_through_encode_and_decode() {
        for sequence_number in [0_i64, 1, 42, -1, i64::MIN, i64::MAX] {
            let cursor = SeqCursor::new(sequence_number);
            let encoded = cursor.encode(SCOPE);

            let decoded = SeqCursor::decode(&encoded, SCOPE).expect("round trip");

            assert_eq!(decoded, cursor);
            assert_eq!(decoded.sequence_number(), sequence_number);
            assert_eq!(encoded, cursor.encode(SCOPE));
        }
    }

    #[test]
    fn tampered_list_cursor_is_rejected_as_invalid_cursor() {
        let encoded = sample_list_cursor().encode(SCOPE);
        let raw = URL_SAFE_NO_PAD.decode(&encoded).expect("our own encoding");

        // Every single bit-flip anywhere in the cursor — header, body, or tag — must fail.
        for index in 0..raw.len() {
            for bit in [0_u8, 3, 7] {
                let mut mutated = raw.clone();
                mutated[index] ^= 1 << bit;
                if mutated == raw {
                    continue;
                }
                let candidate = URL_SAFE_NO_PAD.encode(&mutated);

                let error = ListCursor::decode(&candidate, SCOPE)
                    .expect_err(&format!("byte {index} bit {bit} must not decode"));
                assert_eq!(error.status(), StatusCode::BAD_REQUEST);
            }
        }
    }

    #[test]
    fn tampering_with_a_single_base64_character_is_rejected() {
        let encoded = sample_list_cursor().encode(SCOPE);
        let mut chars: Vec<char> = encoded.chars().collect();
        // Pick a middle character so the mutation lands in the signed body, and swap it for
        // a different character from the same alphabet so base64 decoding still succeeds.
        let index = chars.len() / 2;
        chars[index] = if chars[index] == 'A' { 'B' } else { 'A' };
        let mutated: String = chars.into_iter().collect();

        assert_ne!(mutated, encoded);
        assert!(ListCursor::decode(&mutated, SCOPE).is_err());
    }

    #[test]
    fn truncated_or_garbage_cursor_is_rejected_without_panicking() {
        let valid = sample_list_cursor().encode(SCOPE);

        let mut candidates = vec![
            String::new(),
            " ".to_string(),
            "!!!not base64!!!".to_string(),
            "====".to_string(),
            // Valid base64 of nonsense, at the right length and at the wrong length.
            URL_SAFE_NO_PAD.encode([0_u8; HEADER_LEN + LIST_BODY_LEN + TAG_LEN]),
            URL_SAFE_NO_PAD.encode([7_u8; 3]),
            URL_SAFE_NO_PAD.encode(vec![9_u8; 512]),
            // Oversized: rejected before any allocation for the decoded form.
            "A".repeat(MAX_ENCODED_LEN + 1),
            "A".repeat(1_000_000),
            valid.clone() + "AAAA",
        ];
        // Every truncation of a genuinely valid cursor.
        for length in 0..valid.len() {
            candidates.push(valid[..length].to_string());
        }

        for candidate in candidates {
            assert!(
                ListCursor::decode(&candidate, SCOPE).is_err(),
                "ListCursor::decode accepted {:?}",
                &candidate[..candidate.len().min(32)]
            );
            assert!(
                SeqCursor::decode(&candidate, SCOPE).is_err(),
                "SeqCursor::decode accepted {:?}",
                &candidate[..candidate.len().min(32)]
            );
        }
    }

    #[test]
    fn cursor_issued_for_a_different_sort_key_is_rejected() {
        let list = sample_list_cursor().encode(SCOPE);
        let seq = SeqCursor::new(17).encode(SCOPE);

        // The kind byte sits inside the MAC input, so the two shapes cannot be confused
        // even if a future body happened to be the same length.
        assert!(
            SeqCursor::decode(&list, SCOPE).is_err(),
            "a (ts, id) cursor must not decode as a sequence cursor"
        );
        assert!(
            ListCursor::decode(&seq, SCOPE).is_err(),
            "a sequence cursor must not decode as a (ts, id) cursor"
        );
    }

    #[test]
    fn cursor_issued_for_a_different_endpoint_scope_is_rejected() {
        let list = sample_list_cursor().encode(SCOPE);
        let seq = SeqCursor::new(17).encode(SCOPE);

        assert!(ListCursor::decode(&list, SCOPE).is_ok());
        assert!(SeqCursor::decode(&seq, SCOPE).is_ok());

        // Same shape, same length, different endpoint: must fail closed rather than page
        // through an unrelated table's key space.
        assert!(ListCursor::decode(&list, OTHER_SCOPE).is_err());
        assert!(SeqCursor::decode(&seq, OTHER_SCOPE).is_err());
    }

    #[test]
    fn invalid_cursor_error_carries_the_expected_code_and_message_key() {
        let error = ListCursor::decode("not-a-cursor", SCOPE).expect_err("must reject");
        let response = error.error_response(Some("req_test".to_string()));

        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.error.code, "invalid_cursor");
        assert_eq!(response.error.message_key, "moira.error.invalid_cursor");
        assert!(!response.error.message.is_empty());
        // Structured args only, and never the offending input reflected back.
        assert_eq!(response.error.message_args.as_object().unwrap().len(), 0);
        assert!(!response.error.message.contains("not-a-cursor"));
    }

    #[test]
    fn seq_cursor_invalid_error_matches_the_list_cursor_error() {
        let error = SeqCursor::decode("not-a-cursor", SCOPE).expect_err("must reject");
        let response = error.error_response(Some("req_test".to_string()));

        assert_eq!(response.error.code, "invalid_cursor");
        assert_eq!(response.error.message_key, "moira.error.invalid_cursor");
    }

    #[test]
    fn encoded_cursor_contains_no_row_content_beyond_the_sort_key() {
        let cursor = sample_list_cursor();
        let raw = URL_SAFE_NO_PAD
            .decode(cursor.encode(SCOPE))
            .expect("our own encoding");

        // By construction there is no room for anything but the sort key: two header bytes,
        // the 24-byte (micros, uuid) body, and the tag.
        assert_eq!(raw.len(), HEADER_LEN + LIST_BODY_LEN + TAG_LEN);
        assert_eq!(&raw[2..10], &cursor.ts.timestamp_micros().to_be_bytes());
        assert_eq!(&raw[10..26], cursor.id.as_bytes());

        let seq_raw = URL_SAFE_NO_PAD
            .decode(SeqCursor::new(99).encode(SCOPE))
            .expect("our own encoding");
        assert_eq!(seq_raw.len(), HEADER_LEN + SEQ_BODY_LEN + TAG_LEN);
        assert_eq!(&seq_raw[2..10], &99_i64.to_be_bytes());
    }

    #[test]
    fn encoded_cursor_is_url_safe_and_short_enough_for_a_query_string() {
        let encoded = sample_list_cursor().encode(SCOPE);

        assert!(encoded.len() <= MAX_ENCODED_LEN);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "cursor must need no percent-encoding: {encoded}"
        );
    }

    #[test]
    fn decode_optional_treats_absent_and_empty_values_as_the_first_page() {
        assert_eq!(ListCursor::decode_optional(None, SCOPE).unwrap(), None);
        assert_eq!(ListCursor::decode_optional(Some(""), SCOPE).unwrap(), None);
        assert_eq!(
            ListCursor::decode_optional(Some("   "), SCOPE).unwrap(),
            None
        );
        assert_eq!(SeqCursor::decode_optional(None, SCOPE).unwrap(), None);
        assert_eq!(SeqCursor::decode_optional(Some(""), SCOPE).unwrap(), None);

        let cursor = sample_list_cursor();
        assert_eq!(
            ListCursor::decode_optional(Some(&cursor.encode(SCOPE)), SCOPE).unwrap(),
            Some(cursor)
        );
        assert!(ListCursor::decode_optional(Some("garbage"), SCOPE).is_err());
    }

    #[test]
    fn a_cursor_with_a_different_format_version_fails_closed() {
        let mut raw = URL_SAFE_NO_PAD
            .decode(sample_list_cursor().encode(SCOPE))
            .expect("our own encoding");
        raw[0] = CURSOR_FORMAT_VERSION + 1;

        assert!(ListCursor::decode(&URL_SAFE_NO_PAD.encode(&raw), SCOPE).is_err());
    }
}
