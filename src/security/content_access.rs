//! The seam between a repository and the content keyring: seal on write, open on read.
//!
//! [`content_envelope`](crate::security::content_envelope) knows the byte format and nothing
//! else; [`data_keys`](crate::security::data_keys) knows how keys are loaded and nothing about
//! rows. This module is the thin thing in between — it picks the right cipher out of a snapshot,
//! calls seal/open, and turns every refusal into the public error contract.
//!
//! # Two properties this module exists to make structural rather than hoped-for
//!
//! **No I/O.** [`ContentSealer::seal_content`] and [`ContentOpener::open_content`] are synchronous
//! and take an already-unwrapped DEK out of a [`KeyringSnapshot`]. Nothing here awaits, nothing
//! here queries, and nothing here calls [`MasterKeyCustody`](super::MasterKeyCustody). That is
//! what makes it safe to call the cipher *inside* a write transaction, and it is what stops a
//! future KMS custody from turning a 24-message history read into 24 network round trips. The
//! `CountingCustody` test in `tests/conversation_content_encryption.rs` asserts the read side by
//! counting `unwrap` calls, because a comment saying "no I/O" is exactly the kind of guard this
//! project has been bitten by.
//!
//! The cost of that choice, stated rather than hidden: a row sealed by another replica under a
//! key minted *after* this process took its snapshot is unreadable here until the keyring's next
//! refresh (default 300 s), and surfaces as `content_key_unavailable` — a `503`, which is the
//! honest answer for "wait and retry". Resolving it inline would mean a database query and a
//! custody unwrap on the request path, which is the property above.
//!
//! **Precise logs, generic wire.** A header failure names its discriminant in the log: the header
//! is not secret — anyone holding the ciphertext can already read it — and telling an operator
//! "this blob was written by a newer build" apart from "your key is wrong" is the difference
//! between a five-minute fix and an outage spent guessing. An AEAD failure gets **one** opaque
//! code and **one** opaque log line, matching the `credential decryption failed` posture: saying
//! why a tag did not verify is an oracle. Both reach the caller as a coded error with a constant
//! message and no fragment of the row in it.
//!
//! # And the metrics follow the wire, not the log (#171)
//!
//! The three `moira_content_envelope_*` families are incremented here, because this is the only
//! seam both seals and opens pass through. A `/metrics` body is scraped, retained and forwarded,
//! which puts it closer to the wire than to the log — so `{reason}` carries the header
//! discriminants, which are decided from bytes the ciphertext-holder can already read, and folds
//! **every** AEAD refusal into the single `aead_open_failed`. The precise log line stays precise;
//! the exported label set does not inherit that precision.

use std::sync::Arc;

use uuid::Uuid;

use crate::{
    error::AppError,
    infra::metrics::MetricsRegistry,
    security::{
        ContentCipher, ContentEnvelopeError, ContentIdentity, ContentKeyring, KeyringError,
        KeyringSnapshot,
    },
};
use tracing::warn;

/// Opens a sealed content column. Synchronous and I/O-free — see the module header.
pub trait ContentOpener: Send + Sync {
    /// Decrypt `envelope` under `identity` and return the plaintext.
    ///
    /// `identity` must be built from the **same** row columns that were bound at seal time; the
    /// AAD authenticates them, so a mismatch is a tag failure rather than a silent wrong answer.
    fn open_content(
        &self,
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
    ) -> Result<String, AppError>;
}

/// Seals content for a column. Synchronous and I/O-free — see the module header.
pub trait ContentSealer: Send + Sync {
    /// Encrypt `plaintext` for `identity` under the keyring's **active** data key.
    ///
    /// Refuses with `content_key_unavailable` (503) rather than falling back to anything. There
    /// is no arm of this function that writes plaintext.
    fn seal_content(
        &self,
        identity: &ContentIdentity<'_>,
        plaintext: &str,
    ) -> Result<Vec<u8>, AppError>;

    /// The `memory_records.content_hash` a row must carry. **Keyed, under every policy value.**
    ///
    /// # Why this takes no storage form — issue #168
    ///
    /// It used to take a [`ContentWrite`](crate::domain::ContentWrite) and branch on it: keyed
    /// under `encrypted_content`, the unkeyed content address under everything else. Issue #140
    /// closed the oracle for sealed rows and **named the residual it left**; issue #168 is the
    /// decision that closed the rest of it.
    ///
    /// The residual was this. A memory body is short, low-entropy and highly guessable — "user
    /// prefers dark mode", "user's timezone is Asia/Jakarta". An unkeyed digest of such a body is
    /// an offline verifier: a database dump plus a wordlist recovers the content for free. Under
    /// `none` and `metadata_only` the row stores **no body at all** and still stored that digest,
    /// so an operator who selected the strongest privacy value got a *weaker* outcome than one who
    /// selected `encrypted_content`, and nothing told them.
    ///
    /// So there is no longer a parameter that could select an unkeyed digest, because there is no
    /// longer a branch. That is deliberate and is the strongest available form of the property:
    /// the cheapest edit that reopens the oracle is now replacing this call with
    /// [`request_hash`](crate::security::request_hash) outright, which no reader mistakes for a
    /// refactor.
    ///
    /// `plain_content` is keyed too, one step past what #168 strictly asked. A row whose body sits
    /// in the clear in the adjacent column is not *protected* by keying its digest — but a
    /// three-of-four rule leaves a branch, a branch is where the next unkeyed arm gets added back,
    /// and "the memory digest is keyed, always" is a sentence an operator can hold. The cost is
    /// paid in full below rather than netted off.
    ///
    /// # What this costs, stated rather than implied
    ///
    /// **Every deployment now depends on the `memory_dedupe` key for dedupe to keep working** —
    /// including `none` and `metadata_only` deployments, on rows whose bodies they deliberately do
    /// not keep. If that key is lost (the keyring's `abandon` path), exact-match dedupe breaks for
    /// those rows too, even though no plaintext was ever at risk because none was stored. Memories
    /// then accumulate one duplicate per distinct body. `docs/security.md` states this at the same
    /// volume; it is a real trade, taken knowingly.
    ///
    /// It costs less than F14 feared, and the reason is the key's custody rather than an argument
    /// about likelihood. F14 rejected a *peppered* hash here because `memory_records` has **no
    /// retention** — a nullable `valid_until`, a `status` that stays `'active'` indefinitely — so a
    /// pepper rotation permanently orphans every stored digest with no error and no log line. A
    /// `content_data_keys` row is different in kind: it is wrapped by the master key, so a
    /// master-key rotation re-wraps the envelope and the 32 bytes inside are unchanged. Every
    /// stored `content_hash` stays byte-identical.
    /// `memory_dedupe_hashes_survive_a_master_key_rotation` proves that against a real database,
    /// across more than one policy value, rather than trusting that #140's single-policy version
    /// generalises.
    ///
    /// # What did not change
    ///
    /// The `d1:` prefix, its collision-safety argument, and the two-era behaviour. A
    /// `request_hash` value is unpadded base64url over a fixed 32-byte digest and can never
    /// contain `:` — migration `0021`'s own rule — so a row written before this change and a row
    /// written after it **miss, never falsely match**. No rewrite of existing rows is required;
    /// what is required is announcing that unsealed applications get one duplicate per re-stated
    /// memory across the boundary, exactly as sealed ones did at #140.
    ///
    /// Also unchanged: `content_size_bytes`, `token_count`, and whether a body is stored at all.
    /// This is a decision about the digest and nothing else.
    ///
    /// # Reversal condition
    ///
    /// Drop the digest entirely under `none` and `metadata_only` — the issue's option 2 — if the
    /// key-loss dependency ever proves worse than losing dedupe for applications that store no
    /// body. That is a coherent position: a stable digest of a body you refused to store is close
    /// to a contradiction. It was not taken because dedupe under those policies is a working
    /// feature today and removing it is the larger behaviour change of the two.
    ///
    /// Refuses with `content_key_unavailable` when there is no active `memory_dedupe` key.
    /// **There is no arm that falls back to the unkeyed address**; that fallback is the hole this
    /// function exists to close.
    fn memory_content_hash(&self, content: &str) -> Result<String, AppError>;
}

/// A [`ContentOpener`] and [`ContentSealer`] backed by a live [`ContentKeyring`].
///
/// Holds the keyring rather than a snapshot, and takes a fresh snapshot per call: a snapshot
/// captured once at construction would keep a rotation invisible to a long-lived repository for
/// the life of the process. Taking one is an `RwLock` read and an `Arc` clone — nanoseconds, and
/// still no I/O.
///
/// `keyring: None` is the process that has **no database** ([`crate::app::AppState::pool`] is
/// `None` in health-and-metrics-only mode, and there is no keyring to load without one). Every
/// method then refuses with `content_key_unavailable`; none of them degrades to plaintext.
#[derive(Clone, Debug)]
pub struct KeyringContentAccess {
    keyring: Option<Arc<ContentKeyring>>,
}

impl KeyringContentAccess {
    pub fn new(keyring: Option<Arc<ContentKeyring>>) -> Self {
        Self { keyring }
    }

    /// Whether this access can seal, **asked the same way a write asks it**.
    ///
    /// Read by the admin API's narrowed `conversation_content_persistence_unsupported` refusal: an
    /// operator selecting `encrypted_content` on a process that cannot seal deserves the refusal
    /// at policy-write time rather than on their users' next message.
    ///
    /// It delegates to [`Self::resolve_active_cipher`] rather than testing `keyring.is_some()`,
    /// and that is the whole point. A cheaper predicate here would be a *second, narrower*
    /// definition of "can seal": the 422 would accept a policy the very next `add_message` refuses
    /// with a `503`. Two answers about one condition is exactly the drift this subsystem keeps
    /// being bitten by, so there is one function and both callers ask it.
    ///
    /// The `Err` is dropped rather than logged: this is a question, not an attempt, and a WARN per
    /// policy read would be noise. The write path logs when it actually refuses a write.
    pub fn can_seal(&self) -> bool {
        self.resolve_active_cipher().is_ok()
    }

    /// The registry the three `moira_content_envelope_*` families are written into.
    ///
    /// `None` only for the keyring-less process — no database, therefore no sealed row to seal or
    /// open, therefore nothing to count. Every refusal that arm produces is `keyring_not_loaded`,
    /// which is why that string is deliberately absent from
    /// [`CONTENT_ENVELOPE_OPEN_FAILURE_REASONS`](crate::infra::metrics::CONTENT_ENVELOPE_OPEN_FAILURE_REASONS):
    /// seeding a reason nothing can ever increment would be a permanent zero pretending to be a
    /// measurement.
    fn metrics(&self) -> Option<&MetricsRegistry> {
        self.keyring.as_ref().map(|keyring| keyring.metrics())
    }

    /// The cipher new content is sealed under, or the reason there is none.
    ///
    /// Returns the discriminant rather than an `AppError` so [`Self::can_seal`] can ask without
    /// logging and [`Self::active_cipher`] can log a specific reason. The state re-check is not
    /// redundant with [`ContentKeyring::load`], which selects `active_content` from the row whose
    /// state is `active`: it is here because [`KeyringSnapshot::active_content`] is infallible by
    /// type, and "the write path cannot express a refusal" is precisely how a fallback gets added
    /// later by someone who needs one.
    fn resolve_active_cipher(&self) -> Result<Arc<ContentCipher>, UnsealableReason> {
        let Some(keyring) = self.keyring.as_ref() else {
            return Err(UnsealableReason::KeyringNotLoaded);
        };
        let snapshot = keyring.snapshot();
        let cipher = snapshot.active_content();
        let data_key_id = cipher.data_key_id();
        match snapshot.entry(data_key_id) {
            Some(entry) if entry.state.is_writable() => Ok(cipher),
            Some(entry) => Err(UnsealableReason::ActiveKeyNotWritable {
                data_key_id,
                state: entry.state.as_str(),
            }),
            None => Err(UnsealableReason::ActiveKeyAbsentFromSnapshot { data_key_id }),
        }
    }

    /// [`Self::resolve_active_cipher`], with the refusal logged and mapped to the wire.
    fn active_cipher(&self) -> Result<Arc<ContentCipher>, AppError> {
        self.resolve_active_cipher().map_err(|reason| {
            match reason {
                UnsealableReason::KeyringNotLoaded => warn!(
                    reason = "keyring_not_loaded",
                    "refusing to store content under an encrypted persistence policy: this \
                     process has no content keyring"
                ),
                UnsealableReason::ActiveKeyNotWritable { data_key_id, state } => warn!(
                    reason = "active_key_not_writable",
                    %data_key_id,
                    state,
                    "refusing to store content under an encrypted persistence policy"
                ),
                UnsealableReason::ActiveKeyAbsentFromSnapshot { data_key_id } => warn!(
                    reason = "active_key_absent_from_snapshot",
                    %data_key_id,
                    "refusing to store content under an encrypted persistence policy"
                ),
            }
            content_key_unavailable()
        })
    }
}

/// Why this process cannot seal. Log-only; every variant reaches the wire as the one
/// `content_key_unavailable` code.
#[derive(Debug, Clone, Copy)]
enum UnsealableReason {
    /// No keyring at all — a process with no database.
    KeyringNotLoaded,
    /// The snapshot's active key is not in a state new content may be written under.
    ActiveKeyNotWritable {
        data_key_id: Uuid,
        state: &'static str,
    },
    /// The snapshot names an active cipher whose entry it does not carry. A wiring bug in
    /// keyring assembly rather than an operator condition, which is why it has its own name.
    ActiveKeyAbsentFromSnapshot { data_key_id: Uuid },
}

impl ContentSealer for KeyringContentAccess {
    fn seal_content(
        &self,
        identity: &ContentIdentity<'_>,
        plaintext: &str,
    ) -> Result<Vec<u8>, AppError> {
        let cipher = self.active_cipher()?;
        let sealed = cipher
            .seal(identity, plaintext.as_bytes())
            .map_err(|error| seal_failed(error, identity))?;
        // After the bytes exist, never before: `moira_content_envelope_seal_total` counts
        // envelopes, not attempts. `active_cipher()` above already returned, so a keyring is
        // present and its registry is the one every other content family lands in.
        if let Some(metrics) = self.metrics() {
            metrics.record_content_envelope_seal(identity.profile());
        }
        Ok(sealed)
    }

    fn memory_content_hash(&self, content: &str) -> Result<String, AppError> {
        let hasher = self
            .keyring
            .as_ref()
            .and_then(|keyring| keyring.active_memory_dedupe())
            .ok_or_else(|| {
                warn!(
                    reason = "no_active_memory_dedupe_key",
                    "refusing to store a memory: this keyring carries no active `memory_dedupe` \
                     key, and the unkeyed content address of a short, guessable memory body is a \
                     dictionary-attack oracle against content this row may not even hold"
                );
                content_key_unavailable()
            })?;
        Ok(hasher.hash(content.as_bytes()))
    }
}

impl ContentOpener for KeyringContentAccess {
    fn open_content(
        &self,
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
    ) -> Result<String, AppError> {
        let Some(keyring) = self.keyring.as_ref() else {
            warn!(
                reason = "keyring_not_loaded",
                profile = identity.profile().column(),
                "cannot open sealed content: this process has no content keyring"
            );
            return Err(content_key_unavailable());
        };
        open_with_snapshot(&keyring.snapshot(), keyring.metrics(), envelope, identity)
    }
}

/// A [`ContentOpener`] over one frozen [`KeyringSnapshot`].
///
/// Exists for the paths that must prove they touched the keyring exactly once — and for tests
/// that need to hold a snapshot still while asserting that a read never went back to custody.
#[derive(Clone, Debug)]
pub struct SnapshotContentOpener {
    snapshot: Arc<KeyringSnapshot>,
    /// Required rather than optional, unlike [`KeyringContentAccess::metrics`]. This opener is
    /// constructed from an already-loaded snapshot, so the keyring-less arm that justifies the
    /// `Option` there cannot occur here, and an `Option` would only offer a way to build an
    /// opener that silently counts nothing.
    metrics: MetricsRegistry,
}

impl SnapshotContentOpener {
    pub fn new(snapshot: Arc<KeyringSnapshot>, metrics: MetricsRegistry) -> Self {
        Self { snapshot, metrics }
    }
}

impl ContentOpener for SnapshotContentOpener {
    fn open_content(
        &self,
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
    ) -> Result<String, AppError> {
        open_with_snapshot(&self.snapshot, &self.metrics, envelope, identity)
    }
}

/// The one implementation of the read path, shared by both openers so they cannot drift.
///
/// # The metric contract this function is the sole enforcer of
///
/// Exactly one of `moira_content_envelope_open_total` and
/// `moira_content_envelope_open_failed_total` moves per call, and every `return Err` below is
/// paired with the failure counter while the single `Ok` at the end is paired with the success
/// counter. The success counter is written *after* the last fallible step rather than after the
/// AEAD, so the two are disjoint: a counter that ticked on both paths would look meaningful and
/// would not be, which is the shape #171 exists to avoid rather than to add.
///
/// Every `reason` is the same `&'static str` the neighbouring WARN already logs — the log and the
/// metric cannot disagree about what happened — and `data_key_id` is a label on the success
/// counter only. See `CONTENT_ENVELOPE_OPEN_FAILURE_REASONS` for why.
fn open_with_snapshot(
    snapshot: &KeyringSnapshot,
    metrics: &MetricsRegistry,
    envelope: &[u8],
    identity: &ContentIdentity<'_>,
) -> Result<String, AppError> {
    // Framing first, and *before* any key is looked at. A v2 blob on a v1 binary must be refused
    // as an unreadable format, never as a missing key — the remedies are opposite.
    let header = crate::security::EnvelopeHeader::parse(envelope).map_err(|error| {
        metrics.record_content_envelope_open_failed(
            identity.profile(),
            envelope_error_discriminant(&error),
        );
        envelope_refused(error, identity)
    })?;

    let Some(entry) = snapshot.entry(header.data_key_id) else {
        warn!(
            reason = "unknown_data_key",
            data_key_id = %header.data_key_id,
            table = identity.profile().table(),
            column = identity.profile().column(),
            "cannot open sealed content: the keyring snapshot has no such data key. Either it \
             is retired, or it was minted after this replica's last keyring refresh"
        );
        // No `data_key_id` label here, deliberately. This is the one arm where the header names a
        // key the keyring has never heard of, so labelling it would let anyone with database write
        // access mint an unbounded series set on the scrape path — the exact leak the success
        // counter's bound depends on this arm not opening.
        metrics.record_content_envelope_open_failed(identity.profile(), "unknown_data_key");
        return Err(content_key_unavailable());
    };
    // Two conditions, two answers. Folding them would send an operator chasing a lost master key
    // for an envelope that names a key which was never a content key in the first place.
    let cipher = entry.cipher().map_err(|error| match error {
        KeyringError::WrongPurposeDataKey { purpose, .. } => {
            warn!(
                reason = "wrong_purpose_data_key",
                data_key_id = %header.data_key_id,
                key_purpose = purpose,
                table = identity.profile().table(),
                column = identity.profile().column(),
                "cannot open sealed content: the envelope names a data key whose purpose is not \
                 content encryption. Nothing in this build seals under such a key, so the row \
                 was written by something else"
            );
            metrics
                .record_content_envelope_open_failed(identity.profile(), "wrong_purpose_data_key");
            content_decryption_failed()
        }
        _ => {
            warn!(
                reason = "abandoned_data_key",
                data_key_id = %header.data_key_id,
                table = identity.profile().table(),
                column = identity.profile().column(),
                "cannot open sealed content: this data key was abandoned, so rows sealed under \
                 it are permanently unreadable"
            );
            metrics.record_content_envelope_open_failed(identity.profile(), "abandoned_data_key");
            AppError::coded(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "content_key_abandoned",
                "the key that protects this content was abandoned and the content cannot be read",
            )
        }
    })?;

    let plaintext = cipher.open(envelope, identity).map_err(|error| {
        // `envelope_error_discriminant` renders every AEAD failure as the single
        // `aead_open_failed`, so the label set stops exactly where the wire format stops. Nothing
        // downstream of the failing `decrypt` — not the key, not a byte of the body — is reachable
        // from here.
        metrics.record_content_envelope_open_failed(
            identity.profile(),
            envelope_error_discriminant(&error),
        );
        envelope_refused(error, identity)
    })?;
    // Post-AEAD, so these bytes are authenticated: this is not an oracle, and it can only mean
    // that something other than this build's writers produced the row.
    let text = String::from_utf8(plaintext.to_vec()).map_err(|_| {
        warn!(
            reason = "plaintext_not_utf8",
            data_key_id = %header.data_key_id,
            table = identity.profile().table(),
            column = identity.profile().column(),
            "sealed content authenticated but its plaintext is not UTF-8"
        );
        metrics.record_content_envelope_open_failed(identity.profile(), "plaintext_not_utf8");
        content_decryption_failed()
    })?;
    // The only success arm, and the only place `data_key_id` becomes a label.
    //
    // The id is read off `cipher`, never off `header`, and that is the family's whole cardinality
    // bound. `cipher` exists only because `snapshot.entry(..)` returned `Some` and `entry.cipher()`
    // succeeded, so its id is by construction one the loaded keyring carries — a caller cannot
    // label with an id it did not resolve, because it would have to produce a `ContentCipher` to
    // get one. Reading `header.data_key_id` here would compile and would be equal (`open` refuses
    // `DataKeyMismatch` above), but it would make the bound a fact about which line precedes which
    // rather than about where the value came from, and moving one line would silently let anyone
    // with database write access mint an unbounded series set on the scrape path.
    metrics.record_content_envelope_open(identity.profile(), cipher.data_key_id());
    Ok(text)
}

/// Translate a [`ContentEnvelopeError`] into the public contract.
///
/// **The asymmetry is the point.** Every framing variant is decided from bytes that are already
/// visible to whoever holds the ciphertext, so its discriminant goes in the log. `Decrypt` is the
/// tag failing, and it gets one word — `aead_open_failed` — because anything more is a way to
/// learn whether a guessed key or a doctored blob got closer.
fn envelope_refused(error: ContentEnvelopeError, identity: &ContentIdentity<'_>) -> AppError {
    let profile = identity.profile();
    if matches!(error, ContentEnvelopeError::Decrypt) {
        warn!(
            reason = "aead_open_failed",
            table = profile.table(),
            column = profile.column(),
            "sealed content did not authenticate"
        );
        return content_decryption_failed();
    }
    warn!(
        reason = envelope_error_discriminant(&error),
        table = profile.table(),
        column = profile.column(),
        detail = %error,
        "stored content envelope was refused before decryption"
    );
    AppError::coded(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "content_envelope_unsupported",
        "the stored content is not in a format this build can read",
    )
}

/// A seal failure. Not reachable with a valid key and an in-range plaintext, so it is reported
/// as the encoding refusal it is rather than being folded into the key conditions.
fn seal_failed(error: ContentEnvelopeError, identity: &ContentIdentity<'_>) -> AppError {
    let profile = identity.profile();
    warn!(
        reason = envelope_error_discriminant(&error),
        table = profile.table(),
        column = profile.column(),
        detail = %error,
        "refusing to store content: sealing failed"
    );
    AppError::coded(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "content_envelope_unsupported",
        "the content could not be sealed for storage",
    )
}

/// A stable, log-only name per framing variant.
///
/// Exhaustive on purpose: a new [`ContentEnvelopeError`] variant is a compile error here, which
/// is what stops it from silently inheriting whatever the previous catch-all said.
fn envelope_error_discriminant(error: &ContentEnvelopeError) -> &'static str {
    match error {
        ContentEnvelopeError::TooShort { .. } => "too_short",
        ContentEnvelopeError::BadMagic { .. } => "bad_magic",
        ContentEnvelopeError::UnsupportedFormatVersion { .. } => "unsupported_format_version",
        ContentEnvelopeError::UnsupportedAlgorithm { .. } => "unsupported_algorithm",
        ContentEnvelopeError::UnsupportedKeyMode { .. } => "unsupported_key_mode",
        ContentEnvelopeError::ReservedNotZero { .. } => "reserved_not_zero",
        ContentEnvelopeError::UnknownAadProfile { .. } => "unknown_aad_profile",
        ContentEnvelopeError::BodyLengthMismatch { .. } => "body_length_mismatch",
        ContentEnvelopeError::ProfileMismatch { .. } => "profile_mismatch",
        ContentEnvelopeError::DataKeyMismatch { .. } => "data_key_mismatch",
        ContentEnvelopeError::PlaintextTooLarge { .. } => "plaintext_too_large",
        ContentEnvelopeError::Encrypt => "aead_seal_failed",
        // Never reaches a *log* line — `envelope_refused` short-circuits this variant above and
        // writes its own. It does reach `moira_content_envelope_open_failed_total{reason}`, and
        // that is the point: one opaque value for every AEAD refusal, so the metric stops exactly
        // where the wire format stops. Present in this match rather than as a `_` arm so a new
        // variant cannot inherit the opaque name by accident.
        ContentEnvelopeError::Decrypt => "aead_open_failed",
    }
}

/// `503`. Retrying helps once the key is restored, promoted or refreshed.
pub fn content_key_unavailable() -> AppError {
    AppError::coded(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "content_key_unavailable",
        "content encryption is configured but no usable content key is available",
    )
}

/// `500`, with a message that is a constant. It carries no key id, no reason discriminant and no
/// fragment of the row: everything an operator needs went to the log line instead.
fn content_decryption_failed() -> AppError {
    AppError::coded(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "content_decryption_failed",
        "the stored content could not be decrypted",
    )
}

/// The impossible-by-CHECK-constraint case: a row holding **both** a plaintext and a ciphertext.
///
/// `migrations/0027_content_encryption_keyring.sql` adds a `plain is null or encrypted is null`
/// CHECK to **all five** tables — `conversation_messages`, `memory_records` and
/// `rag_document_versions` on `(content_plain, content_encrypted)`, `conversation_summaries` on
/// its `summary_text_*` pair, and `rag_chunks` on its `chunk_text_*` pair — so this is
/// unreachable through the constraint. It is `not valid`, though, so
/// pre-existing rows were never checked — and a row that somehow holds both is ambiguous rather
/// than merely odd. Encrypted wins (it is the stricter of the two intentions) and exactly one
/// WARN naming the row is logged.
///
/// `the_five_content_exclusivity_constraints_exist_and_are_validated` in
/// `tests/rag_content_encryption.rs` asserts all five against `pg_constraint`, including that
/// each is still `convalidated`, because everything above is a promise about the schema that
/// nothing in Rust can enforce.
pub fn warn_content_storage_ambiguous(table: &'static str, row_id: Uuid) {
    warn!(
        code = "content_storage_ambiguous",
        table,
        %row_id,
        "row holds both a plaintext and a sealed body; the sealed body wins. The CHECK \
         constraint added by migration 0027 is NOT VALID, so a row written before it was added \
         can reach this"
    );
}

/// The failure class each public code above belongs to.
///
/// Exists so `ExecutionFailureClass` and the literal codes cannot drift: the class enumeration is
/// what the compile-time i18n gate walks, and the literals are what this module actually emits.
/// The test below compares them.
#[cfg(test)]
const CONTENT_FAILURE_CLASSES: [crate::domain::ExecutionFailureClass; 4] = [
    crate::domain::ExecutionFailureClass::ContentDecryptionFailed,
    crate::domain::ExecutionFailureClass::ContentEnvelopeUnsupported,
    crate::domain::ExecutionFailureClass::ContentKeyUnavailable,
    crate::domain::ExecutionFailureClass::ContentKeyAbandoned,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::ExecutionFailureClass, security::AadProfile};

    fn message_identity() -> ContentIdentity<'static> {
        ContentIdentity::ConversationMessage {
            message_id: Uuid::nil(),
            conversation_id: Uuid::nil(),
            sequence_number: 1,
        }
    }

    /// `can_seal()` and the write path must answer the same question.
    ///
    /// **This is the assertion behind the narrowed 422.** If `can_seal` were a cheaper predicate
    /// than the one `seal_content` applies, the admin API would accept a policy that the very
    /// next `add_message` refuses with a `503` — an operator told "yes" and then told "no", with
    /// nothing between the two to explain it. One function answers both, and this pins that they
    /// stay pinned to each other.
    ///
    /// A keyring-less access is the arm reachable without a database, which is why it is the one
    /// asserted here; the two key-state arms need a live keyring and are covered from
    /// `tests/conversation_content_encryption.rs`.
    #[test]
    fn can_seal_agrees_with_what_sealing_actually_does() {
        let access = KeyringContentAccess::new(None);
        assert!(
            !access.can_seal(),
            "a process with no keyring must not report that it can seal; the admin API's 422 \
             reads this"
        );
        let error = access
            .seal_content(&message_identity(), "body")
            .expect_err("a keyring-less access must refuse to seal");
        assert_eq!(
            error.error_response(None).error.code,
            "content_key_unavailable"
        );
        assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            access.can_seal(),
            access.seal_content(&message_identity(), "body").is_ok(),
            "can_seal and seal_content disagree, so the 422 and the 503 are answering two \
             different questions"
        );
    }

    /// The wire must not distinguish an AEAD failure from anything, and must not carry any part
    /// of the row. Asserted against the rendered response body, not against the constructor
    /// arguments, because the body is what a caller reads.
    #[test]
    fn an_aead_failure_renders_one_opaque_code_and_message() {
        let error = envelope_refused(ContentEnvelopeError::Decrypt, &message_identity());
        let response = error.error_response(None);
        assert_eq!(response.error.code, "content_decryption_failed");
        assert_eq!(
            response.error.message, "the stored content could not be decrypted",
            "the public message must be a constant; anything derived from the failure is an \
             oracle"
        );
    }

    /// A header failure and an AEAD failure must not collapse into one code. If they did, an
    /// operator could no longer tell "this build predates the format" from "your key is wrong",
    /// which is the split the format was designed around.
    #[test]
    fn a_header_failure_is_a_different_code_from_an_aead_failure() {
        let header = envelope_refused(
            ContentEnvelopeError::UnsupportedFormatVersion { format_version: 2 },
            &message_identity(),
        );
        let aead = envelope_refused(ContentEnvelopeError::Decrypt, &message_identity());
        assert_eq!(
            header.error_response(None).error.code,
            "content_envelope_unsupported"
        );
        assert_ne!(
            header.error_response(None).error.code,
            aead.error_response(None).error.code
        );
    }

    /// The public message of a header refusal must not echo the discriminant either: the log gets
    /// that, the wire does not.
    #[test]
    fn a_header_failure_does_not_put_its_discriminant_on_the_wire() {
        for error in [
            ContentEnvelopeError::BadMagic { found: *b"XXXX" },
            ContentEnvelopeError::UnsupportedFormatVersion { format_version: 9 },
            ContentEnvelopeError::ReservedNotZero { reserved: 7 },
            ContentEnvelopeError::BodyLengthMismatch {
                declared: 10,
                actual: 20,
            },
        ] {
            let discriminant = envelope_error_discriminant(&error);
            let message = envelope_refused(error, &message_identity())
                .error_response(None)
                .error
                .message;
            assert!(
                !message.contains(discriminant),
                "{discriminant:?} reached the public message {message:?}"
            );
        }
    }

    /// Every framing variant gets a distinct log name. A duplicate would silently merge two
    /// conditions in an operator's dashboard.
    #[test]
    fn every_envelope_discriminant_is_distinct() {
        let names = [
            envelope_error_discriminant(&ContentEnvelopeError::TooShort { len: 0 }),
            envelope_error_discriminant(&ContentEnvelopeError::BadMagic { found: [0; 4] }),
            envelope_error_discriminant(&ContentEnvelopeError::UnsupportedFormatVersion {
                format_version: 2,
            }),
            envelope_error_discriminant(&ContentEnvelopeError::UnsupportedAlgorithm {
                algorithm_id: 2,
            }),
            envelope_error_discriminant(&ContentEnvelopeError::UnsupportedKeyMode { key_mode: 2 }),
            envelope_error_discriminant(&ContentEnvelopeError::ReservedNotZero { reserved: 1 }),
            envelope_error_discriminant(&ContentEnvelopeError::UnknownAadProfile {
                aad_profile: 99,
            }),
            envelope_error_discriminant(&ContentEnvelopeError::BodyLengthMismatch {
                declared: 1,
                actual: 2,
            }),
            envelope_error_discriminant(&ContentEnvelopeError::ProfileMismatch {
                header: AadProfile::ConversationMessageContent,
                identity: AadProfile::ConversationSummaryText,
            }),
            envelope_error_discriminant(&ContentEnvelopeError::DataKeyMismatch {
                header: Uuid::nil(),
                cipher: Uuid::nil(),
            }),
            envelope_error_discriminant(&ContentEnvelopeError::PlaintextTooLarge { len: 1 }),
            envelope_error_discriminant(&ContentEnvelopeError::Encrypt),
            envelope_error_discriminant(&ContentEnvelopeError::Decrypt),
        ];
        let mut sorted = names.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "two ContentEnvelopeError variants share a log discriminant: {names:?}"
        );
    }

    /// Every `reason` the read path can hand to the metrics registry must be admitted by the
    /// closed domain there — otherwise it is folded into `other` and the condition disappears
    /// into a bucket that means "unrecognised".
    ///
    /// This is the drift guard between two lists that live in different modules. Adding a
    /// [`ContentEnvelopeError`] variant already fails to compile in
    /// [`envelope_error_discriminant`]; without this test, giving it a name there and forgetting
    /// `CONTENT_ENVELOPE_OPEN_FAILURE_REASONS` would compile, run, and silently mislabel it.
    ///
    /// The four keyring reasons are listed as literals rather than derived, because they *are*
    /// literals at their call sites — the same strings the WARN lines carry. If they were derived
    /// from a shared constant the test would prove only that a constant equals itself.
    #[test]
    fn every_open_refusal_reason_is_admitted_by_the_metric_domain() {
        use crate::infra::metrics::CONTENT_ENVELOPE_OPEN_FAILURE_REASONS as ADMITTED;

        // Every ContentEnvelopeError the *open* path can produce. `PlaintextTooLarge` and
        // `Encrypt` are absent because they are seal-only; nothing on the read path can reach
        // them, and admitting them would seed two reasons that can never move.
        let open_reachable = [
            ContentEnvelopeError::TooShort { len: 0 },
            ContentEnvelopeError::BadMagic { found: [0; 4] },
            ContentEnvelopeError::UnsupportedFormatVersion { format_version: 2 },
            ContentEnvelopeError::UnsupportedAlgorithm { algorithm_id: 2 },
            ContentEnvelopeError::UnsupportedKeyMode { key_mode: 2 },
            ContentEnvelopeError::ReservedNotZero { reserved: 1 },
            ContentEnvelopeError::UnknownAadProfile { aad_profile: 99 },
            ContentEnvelopeError::BodyLengthMismatch {
                declared: 1,
                actual: 2,
            },
            ContentEnvelopeError::ProfileMismatch {
                header: AadProfile::ConversationMessageContent,
                identity: AadProfile::ConversationSummaryText,
            },
            ContentEnvelopeError::DataKeyMismatch {
                header: Uuid::nil(),
                cipher: Uuid::nil(),
            },
            ContentEnvelopeError::Decrypt,
        ];
        for error in &open_reachable {
            let reason = envelope_error_discriminant(error);
            assert!(
                ADMITTED.contains(&reason),
                "{reason:?} can be emitted by the read path but is not in \
                 CONTENT_ENVELOPE_OPEN_FAILURE_REASONS, so it would be folded into `other`"
            );
        }
        for reason in [
            "unknown_data_key",
            "wrong_purpose_data_key",
            "abandoned_data_key",
            "plaintext_not_utf8",
        ] {
            assert!(
                ADMITTED.contains(&reason),
                "{reason:?} is logged and counted by open_with_snapshot but is not admitted"
            );
        }

        // The seal-only discriminants must stay out, and `keyring_not_loaded` with them: it is
        // raised in a process that has no registry to raise it into, so seeding it would render a
        // permanent zero that looks like a measurement and is not.
        for absent in [
            "plaintext_too_large",
            "aead_seal_failed",
            "keyring_not_loaded",
        ] {
            assert!(
                !ADMITTED.contains(&absent),
                "{absent:?} cannot be produced by the read path, so a seeded series for it would \
                 be a permanent zero pretending to be a measurement"
            );
        }

        // The AEAD arm has exactly one value. A second `aead_*` reason would mean somebody split
        // the tag failure into cases, which is the oracle the wire format refuses to be.
        let aead: Vec<&&str> = ADMITTED
            .iter()
            .filter(|reason| reason.starts_with("aead"))
            .collect();
        assert_eq!(
            aead,
            vec![&"aead_open_failed"],
            "every AEAD refusal must collapse into one opaque reason"
        );
    }

    /// The statuses this module raises and the statuses `failure_http_status` maps the matching
    /// `ExecutionFailureClass` to must be the same number. Two answers about one condition would
    /// be worse than either.
    #[test]
    fn content_failure_classes_agree_with_the_statuses_content_access_raises() {
        use axum::http::StatusCode;
        let raised = [
            (
                content_decryption_failed(),
                ExecutionFailureClass::ContentDecryptionFailed,
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                envelope_refused(
                    ContentEnvelopeError::TooShort { len: 0 },
                    &message_identity(),
                ),
                ExecutionFailureClass::ContentEnvelopeUnsupported,
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                content_key_unavailable(),
                ExecutionFailureClass::ContentKeyUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (error, class, expected) in raised {
            assert_eq!(
                error.status(),
                expected,
                "{} raised the wrong status",
                class.code()
            );
            assert_eq!(
                error.error_response(None).error.code,
                class.code(),
                "the literal code and the failure class disagree"
            );
        }
        assert_eq!(
            CONTENT_FAILURE_CLASSES.len(),
            4,
            "a fifth content failure class needs a status and a literal here"
        );
    }
}
