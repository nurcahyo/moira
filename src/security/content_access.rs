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

use std::sync::Arc;

use uuid::Uuid;

use crate::{
    domain::ContentWrite,
    error::AppError,
    security::{
        ContentCipher, ContentEnvelopeError, ContentIdentity, ContentKeyring, KeyringError,
        KeyringSnapshot, request_hash,
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

    /// The `memory_records.content_hash` a row must carry, given the form its body is stored in.
    ///
    /// # Why this lives on the sealer and takes a [`ContentWrite`]
    ///
    /// The hash and the body are **one decision**. A row whose body is sealed but whose hash is
    /// the unkeyed content address is not a partially-encrypted row, it is an unencrypted one
    /// with extra steps: memory bodies are short and guessable, so a dump plus a wordlist
    /// recovers them from the digest alone. Putting the choice behind the same object that seals,
    /// keyed off the same value that decided the column, is what makes the two unable to
    /// disagree.
    ///
    /// The `match` on [`ContentWrite`] is exhaustive with no catch-all, so a fourth variant does
    /// not compile until somebody says which digest it gets — the same property
    /// [`ContentWrite::under_policy`] buys at the write sites.
    ///
    /// # `Omitted` and `Plain` keep the unkeyed content address — finding F14, still standing
    ///
    /// `migrations/0021` moved this column off `IdempotencyHasher` for a reason that has not
    /// changed. That hasher verifies only against the *active* pepper, and justifies the
    /// narrowness with a retention argument: every `idempotency_records` row expires within 24
    /// hours. **`memory_records` has no retention** — a nullable `valid_until`, a `status` that
    /// stays `'active'` indefinitely — so a rotation there would not open a bounded window, it
    /// would permanently orphan every stored hash and exact-match dedupe would stop matching
    /// with no error and no log line. Keying the unsealed arms would re-create that, and gain
    /// nothing: a row whose body is already in the clear is not protected by hiding its digest.
    ///
    /// The three-clause admitting rule plan 11 wrote for `rag_chunks.chunk_hash` still holds for
    /// the unkeyed arms, and is applied **per table**, which is `0021`'s own words:
    ///
    /// * **(a) not caller-visible.** [`crate::domain::MemoryRecord`] has no `content_hash` field
    ///   and no schema in `docs/openapi.json` carries one for a memory. `ConversationMessageRecord`
    ///   does, which is why *that* column stays peppered.
    /// * **(b) never a caller-supplied lookup key.** `MemoryCreateRequest`, `MemoryPatchRequest`
    ///   and `MemoryQuery` all carry `deny_unknown_fields` and none has a hash field.
    /// * **(c) never a cross-application comparison.** Every `memory_records` read is bound by
    ///   `application_id` through `MEMORY_SCOPE_PREDICATE`, which
    ///   `every_memory_read_shares_the_isolation_predicate` asserts against the emitted SQL.
    ///
    /// # Why `Encrypt` is the exception
    ///
    /// Clause (a) is about who holds the digest. Encryption changes the question: a **database
    /// dump** now holds the digest and the ciphertext, and a memory body is short and
    /// low-entropy — "user prefers dark mode". The unkeyed address is then a complete bypass of
    /// the encryption, no key required. `0021` anticipated exactly this and refused to recompute
    /// hashes for encrypted rows; this is the reversal it anticipated, restricted to the rows
    /// that need it so the unsealed corpus keeps matching.
    ///
    /// # Reversal condition
    ///
    /// Move the unsealed arms to a keyed digest too — and pair that with a re-hash-on-rotation
    /// procedure, because the lifetime problem does not go away — the moment any one of the
    /// three clauses stops holding: `content_hash` appears on a caller-visible DTO, a lookup
    /// accepts a caller-supplied hash, or a dedupe query drops the `application_id` predicate.
    ///
    /// Refuses with `content_key_unavailable` when a sealed body has no dedupe key to hash
    /// under. **There is no arm that falls back to the unkeyed address**; that fallback is the
    /// hole this function exists to close.
    fn memory_content_hash(&self, stored: &ContentWrite, content: &str)
    -> Result<String, AppError>;
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
        cipher
            .seal(identity, plaintext.as_bytes())
            .map_err(|error| seal_failed(error, identity))
    }

    fn memory_content_hash(
        &self,
        stored: &ContentWrite,
        content: &str,
    ) -> Result<String, AppError> {
        match stored {
            ContentWrite::Omitted | ContentWrite::Plain(_) => Ok(request_hash(content.as_bytes())),
            ContentWrite::Encrypt(_) => {
                let hasher = self
                    .keyring
                    .as_ref()
                    .and_then(|keyring| keyring.active_memory_dedupe())
                    .ok_or_else(|| {
                        warn!(
                            reason = "no_active_memory_dedupe_key",
                            "refusing to store a sealed memory: this keyring carries no active \
                             `memory_dedupe` key, and writing the unkeyed content address beside \
                             a sealed body would make the ciphertext recoverable from a database \
                             dump plus a wordlist"
                        );
                        content_key_unavailable()
                    })?;
                Ok(hasher.hash(content.as_bytes()))
            }
        }
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
        open_with_snapshot(&keyring.snapshot(), envelope, identity)
    }
}

/// A [`ContentOpener`] over one frozen [`KeyringSnapshot`].
///
/// Exists for the paths that must prove they touched the keyring exactly once — and for tests
/// that need to hold a snapshot still while asserting that a read never went back to custody.
#[derive(Clone, Debug)]
pub struct SnapshotContentOpener {
    snapshot: Arc<KeyringSnapshot>,
}

impl SnapshotContentOpener {
    pub fn new(snapshot: Arc<KeyringSnapshot>) -> Self {
        Self { snapshot }
    }
}

impl ContentOpener for SnapshotContentOpener {
    fn open_content(
        &self,
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
    ) -> Result<String, AppError> {
        open_with_snapshot(&self.snapshot, envelope, identity)
    }
}

/// The one implementation of the read path, shared by both openers so they cannot drift.
fn open_with_snapshot(
    snapshot: &KeyringSnapshot,
    envelope: &[u8],
    identity: &ContentIdentity<'_>,
) -> Result<String, AppError> {
    // Framing first, and *before* any key is looked at. A v2 blob on a v1 binary must be refused
    // as an unreadable format, never as a missing key — the remedies are opposite.
    let header = crate::security::EnvelopeHeader::parse(envelope)
        .map_err(|error| envelope_refused(error, identity))?;

    let Some(entry) = snapshot.entry(header.data_key_id) else {
        warn!(
            reason = "unknown_data_key",
            data_key_id = %header.data_key_id,
            table = identity.profile().table(),
            column = identity.profile().column(),
            "cannot open sealed content: the keyring snapshot has no such data key. Either it \
             is retired, or it was minted after this replica's last keyring refresh"
        );
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
            AppError::coded(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "content_key_abandoned",
                "the key that protects this content was abandoned and the content cannot be read",
            )
        }
    })?;

    let plaintext = cipher
        .open(envelope, identity)
        .map_err(|error| envelope_refused(error, identity))?;
    // Post-AEAD, so these bytes are authenticated: this is not an oracle, and it can only mean
    // that something other than this build's writers produced the row.
    String::from_utf8(plaintext.to_vec()).map_err(|_| {
        warn!(
            reason = "plaintext_not_utf8",
            data_key_id = %header.data_key_id,
            table = identity.profile().table(),
            column = identity.profile().column(),
            "sealed content authenticated but its plaintext is not UTF-8"
        );
        content_decryption_failed()
    })
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
        // Never rendered — `envelope_refused` short-circuits this variant above so its
        // discriminant cannot reach a log line. Present because the match is exhaustive and a
        // `_` arm would let a *new* variant inherit an opaque name by accident.
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
