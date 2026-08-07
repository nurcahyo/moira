//! The on-disk envelope format for content encrypted at rest, and nothing else.
//!
//! This module is pure. It performs no I/O, touches no database, and knows nothing about
//! [`crate::security::key_custody`]. It takes a data key that somebody else already unwrapped
//! and turns a plaintext into the exact bytes that go into a `bytea` column — and back.
//!
//! # Why the format has to be self-describing *and* authenticated
//!
//! The five `*_encrypted` columns are bare `bytea` with **no sibling algorithm, nonce or key-id
//! columns**. `provider_credentials` has six such siblings; these have none. So one blob must
//! carry everything needed to interpret it — and all of it must be authenticated, because
//! "self-describing but unauthenticated" is just a downgrade oracle with extra steps
//! (`docs/decision-encryption-at-rest.md` §6).
//!
//! # Layout — 42-byte header, minimum legal blob 58 bytes
//!
//! ```text
//! off  len  field           value / notes
//! ---  ---  --------------  -----------------------------------------------------------
//!   0    4  magic           4D 4F 45 31  ("MOE1", Moira Object Envelope)
//!   4    1  format_version  0x01
//!   5    1  algorithm_id    0x01 = AES-256-GCM, 12-byte nonce, 16-byte tag
//!   6    1  key_mode        0x01 = generation DEK wrapped by a custody master key
//!   7    1  reserved        0x00 — MUST be zero; non-zero is a hard refusal, never ignored
//!   8   16  data_key_id     raw UUID bytes = content_data_keys.id
//!  24   12  nonce           CSPRNG, fresh per row, never derived, never a counter
//!  36    2  aad_profile     u16 big-endian
//!  38    4  body_len        u32 big-endian = length of body (ciphertext + tag)
//!  42    N  body            AES-256-GCM ciphertext || 16-byte tag
//! ```
//!
//! ```text
//! AAD = header_bytes[0..42] || 0x00 || profile_identity_string
//! ```
//!
//! An empty plaintext still has a tag, hence the 58-byte floor.
//!
//! # Why each field earns its place
//!
//! - **The whole header is inside the AAD**, so `format_version`, `algorithm_id`, `key_mode`,
//!   `data_key_id`, `reserved` and `body_len` cannot be edited without breaking the tag. That
//!   is what makes "self-describing" safe rather than an invitation to downgrade.
//! - **Magic *and* version is deliberate redundancy, not an oversight.** Magic answers "is this
//!   a Moira envelope at all" for bytes of unknown origin; version answers "which layout". A
//!   reader that had only one of the two would have to guess at the other question.
//! - **`body_len` exists although `octet_length` already implies it.** A mismatch proves
//!   truncation, so damaged storage surfaces as a *framing* error ("your storage is damaged")
//!   rather than as a GCM tag failure, which reads as "wrong key, or someone tampered". The
//!   check is key-independent, therefore not an oracle, and at 3 a.m. it sends the operator
//!   down the right path.
//! - **A 16-byte UUID key id, not a `u16` version.** No rotation cap that could only be
//!   corrected by a format-version bump, and a blob restored from `pg_dump` into another
//!   deployment still names the key it wants. 14 bytes on a ~1 KB row.
//!
//! # Validation order
//!
//! Header validation runs **before any key lookup and before any crypto call**, and every
//! failure has **its own discriminant** — see [`ContentEnvelopeError`]. A truncated blob is a
//! framing error, not a tag failure. The only departure from the order written in the decision
//! record is that the length floor is checked first: the other fields cannot be read at all
//! until the buffer is known to be long enough to contain them.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use uuid::Uuid;
use zeroize::Zeroizing;

/// `"MOE1"` — Moira Object Envelope, version-1 family.
pub const ENVELOPE_MAGIC: [u8; 4] = *b"MOE1";

/// The only `format_version` this build writes, and the only one it reads.
pub const FORMAT_VERSION_V1: u8 = 0x01;

/// AES-256-GCM with a 12-byte nonce and a 16-byte tag.
pub const ALGORITHM_AES_256_GCM: u8 = 0x01;

/// The data key is a generation DEK wrapped by a custody master key.
pub const KEY_MODE_WRAPPED_DEK: u8 = 0x01;

/// Fixed header width. Every offset below is relative to the start of the blob.
pub const ENVELOPE_HEADER_LEN: usize = 42;

/// AES-GCM nonce width for [`ALGORITHM_AES_256_GCM`].
pub const ENVELOPE_NONCE_LEN: usize = 12;

/// AES-GCM tag width for [`ALGORITHM_AES_256_GCM`].
pub const ENVELOPE_TAG_LEN: usize = 16;

/// The shortest legal blob: header plus the tag of an empty plaintext.
pub const MIN_ENVELOPE_LEN: usize = ENVELOPE_HEADER_LEN + ENVELOPE_TAG_LEN;

/// Byte separating the header from the identity string inside the AAD. `0x00` cannot occur in
/// the identity string, which is printable ASCII, so the join is unambiguous.
pub const AAD_SEPARATOR: u8 = 0x00;

/// Prefix on every profile identity string. Bumping it re-keys every AAD, which is a format
/// change, not an edit.
pub const IDENTITY_PREFIX: &str = "moira/content/v1";

const OFF_MAGIC: usize = 0;
const OFF_FORMAT_VERSION: usize = 4;
const OFF_ALGORITHM_ID: usize = 5;
const OFF_KEY_MODE: usize = 6;
const OFF_RESERVED: usize = 7;
const OFF_DATA_KEY_ID: usize = 8;
const OFF_NONCE: usize = 24;
const OFF_AAD_PROFILE: usize = 36;
const OFF_BODY_LEN: usize = 38;

/// Every way an envelope can be refused, each with its own discriminant.
///
/// The split is the point. A framing failure means "these bytes are not a well-formed v1
/// envelope" and is decided without touching a key, so it can say precisely what is wrong. A
/// [`ContentEnvelopeError::Decrypt`] means the tag did not verify and deliberately says nothing
/// further, because distinguishing "wrong key" from "tampered blob" is an oracle — the same
/// rule [`crate::security::key_custody::KeyCustodyError::UnwrapFailed`] follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentEnvelopeError {
    /// Shorter than [`MIN_ENVELOPE_LEN`]. Checked first, because nothing else can be read
    /// until the buffer is known to hold a header.
    TooShort {
        len: usize,
    },
    /// The first four bytes are not [`ENVELOPE_MAGIC`]. These bytes are not a Moira envelope.
    BadMagic {
        found: [u8; 4],
    },
    /// A Moira envelope, but of a layout this build does not know. Returned **before** any key
    /// lookup, so a future v2 blob never reaches custody on an old binary.
    UnsupportedFormatVersion {
        format_version: u8,
    },
    UnsupportedAlgorithm {
        algorithm_id: u8,
    },
    UnsupportedKeyMode {
        key_mode: u8,
    },
    /// `reserved` was not zero. Never ignored: a non-zero byte means the writer knew something
    /// about this blob that this reader does not.
    ReservedNotZero {
        reserved: u8,
    },
    UnknownAadProfile {
        aad_profile: u16,
    },
    /// `body_len` disagrees with the actual buffer length. Proof of truncation or of extra
    /// trailing bytes — a storage problem, not a key problem.
    BodyLengthMismatch {
        declared: u32,
        actual: usize,
    },
    /// The header names a different profile than the identity the caller supplied. A caller
    /// bug, caught before the tag would have failed for a reason that looks like tampering.
    ProfileMismatch {
        header: AadProfile,
        identity: AadProfile,
    },
    /// The header names a different data key than the cipher holds. The keyring is expected to
    /// select the cipher *by* the header's key id, so this is a wiring bug, not an attack.
    DataKeyMismatch {
        header: Uuid,
        cipher: Uuid,
    },
    /// The body would not fit in the `u32` `body_len` field.
    PlaintextTooLarge {
        len: usize,
    },
    /// The tag did not verify. Says nothing about why, on purpose.
    Decrypt,
    /// AES-GCM refused to seal. Not reachable with a valid key and an in-range plaintext.
    Encrypt,
}

impl std::fmt::Display for ContentEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { len } => write!(
                f,
                "content envelope is {len} bytes, shorter than the {MIN_ENVELOPE_LEN}-byte minimum"
            ),
            Self::BadMagic { .. } => f.write_str("not a Moira content envelope"),
            Self::UnsupportedFormatVersion { format_version } => write!(
                f,
                "content envelope format version {format_version} is not supported by this build"
            ),
            Self::UnsupportedAlgorithm { algorithm_id } => write!(
                f,
                "content envelope algorithm id {algorithm_id} is not supported by this build"
            ),
            Self::UnsupportedKeyMode { key_mode } => write!(
                f,
                "content envelope key mode {key_mode} is not supported by this build"
            ),
            Self::ReservedNotZero { reserved } => write!(
                f,
                "content envelope reserved byte is {reserved:#04x}, must be zero"
            ),
            Self::UnknownAadProfile { aad_profile } => {
                write!(f, "unknown content AAD profile {aad_profile}")
            }
            Self::BodyLengthMismatch { declared, actual } => write!(
                f,
                "content envelope declares a {declared}-byte body but carries {actual}; \
                 the stored value is damaged"
            ),
            Self::ProfileMismatch { header, identity } => write!(
                f,
                "content envelope was written under AAD profile {} but is being opened as {}",
                header.id(),
                identity.id()
            ),
            Self::DataKeyMismatch { header, cipher } => write!(
                f,
                "content envelope names data key {header} but the supplied cipher holds {cipher}"
            ),
            Self::PlaintextTooLarge { len } => {
                write!(f, "content of {len} bytes is too large to envelope")
            }
            Self::Decrypt => f.write_str("content decryption failed"),
            Self::Encrypt => f.write_str("content encryption failed"),
        }
    }
}

impl std::error::Error for ContentEnvelopeError {}

/// One profile per encrypted column. All five are declared from day one so that no column is
/// ever added to the format later without a registry entry to point at.
///
/// The numeric ids are part of the on-disk format. **An id, once used, is never reused**, even
/// if its column is dropped — a blob restored from an old dump still names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AadProfile {
    /// `conversation_messages.content_encrypted`
    ConversationMessageContent = 1,
    /// `conversation_summaries.summary_text_encrypted`
    ConversationSummaryText = 2,
    /// `memory_records.content_encrypted`
    MemoryRecordContent = 3,
    /// `rag_document_versions.content_encrypted`
    RagDocumentVersionContent = 4,
    /// `rag_chunks.chunk_text_encrypted`
    RagChunkText = 5,
}

impl AadProfile {
    /// Every declared profile. Adding a variant without extending this array fails to compile,
    /// and the table-driven tests iterate it, so a profile cannot arrive untested.
    pub const ALL: [AadProfile; 5] = [
        AadProfile::ConversationMessageContent,
        AadProfile::ConversationSummaryText,
        AadProfile::MemoryRecordContent,
        AadProfile::RagDocumentVersionContent,
        AadProfile::RagChunkText,
    ];

    /// The wire id, written big-endian at offset 36.
    pub const fn id(self) -> u16 {
        self as u16
    }

    /// Resolve a wire id. Iterating [`Self::ALL`] rather than matching means a new profile is
    /// recognised the moment it joins the registry.
    pub fn from_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|profile| profile.id() == id)
    }

    /// The table this profile's column lives in.
    pub const fn table(self) -> &'static str {
        match self {
            Self::ConversationMessageContent => "conversation_messages",
            Self::ConversationSummaryText => "conversation_summaries",
            Self::MemoryRecordContent => "memory_records",
            Self::RagDocumentVersionContent => "rag_document_versions",
            Self::RagChunkText => "rag_chunks",
        }
    }

    /// The encrypted column this profile covers.
    pub const fn column(self) -> &'static str {
        match self {
            Self::ConversationMessageContent => "content_encrypted",
            Self::ConversationSummaryText => "summary_text_encrypted",
            Self::MemoryRecordContent => "content_encrypted",
            Self::RagDocumentVersionContent => "content_encrypted",
            Self::RagChunkText => "chunk_text_encrypted",
        }
    }
}

/// The row identity bound into the AAD, one variant per [`AadProfile`].
///
/// Binding row identity is what stops an attacker holding database *write* access from lifting
/// tenant A's ciphertext into tenant B's conversation — the property
/// [`crate::security::crypto::credential_aad`] already buys for provider credentials.
///
/// **Only values that are final at encrypt time may be bound.** Everything here is generated
/// immediately before its insert and never updated afterwards. Anything mutable is deliberately
/// left out: binding a value that can change creates a re-encryption requirement, which is
/// precisely what this design refuses. Do not add a title, a status, or an `updated_at` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentIdentity<'a> {
    ConversationMessage {
        message_id: Uuid,
        conversation_id: Uuid,
        sequence_number: i64,
    },
    ConversationSummary {
        summary_id: Uuid,
        conversation_id: Uuid,
        covers_through_sequence: i64,
    },
    MemoryRecord {
        memory_id: Uuid,
        application_id: Uuid,
        memory_scope: &'a str,
    },
    RagDocumentVersion {
        version_id: Uuid,
        document_id: Uuid,
        version_number: i32,
    },
    RagChunk {
        chunk_id: Uuid,
        document_version_id: Uuid,
        chunk_index: i32,
    },
}

impl ContentIdentity<'_> {
    /// The profile this identity belongs to. Exhaustive by construction.
    pub const fn profile(&self) -> AadProfile {
        match self {
            Self::ConversationMessage { .. } => AadProfile::ConversationMessageContent,
            Self::ConversationSummary { .. } => AadProfile::ConversationSummaryText,
            Self::MemoryRecord { .. } => AadProfile::MemoryRecordContent,
            Self::RagDocumentVersion { .. } => AadProfile::RagDocumentVersionContent,
            Self::RagChunk { .. } => AadProfile::RagChunkText,
        }
    }

    /// The identity tail of the AAD, semicolon-joined in the
    /// [`crate::security::crypto::credential_aad`] house style.
    ///
    /// **This string is part of the on-disk contract.** Every byte of it is authenticated, so
    /// renaming a field here does not "clean up a name" — it orphans every row already written.
    /// The pinned-literal tests exist to make that impossible to do by accident.
    ///
    /// The `match` is exhaustive on purpose: a new profile cannot be added without deciding,
    /// here, exactly what it binds.
    pub fn identity_string(&self) -> String {
        let profile = self.profile();
        let head = format!(
            "{IDENTITY_PREFIX};table={};column={}",
            profile.table(),
            profile.column()
        );
        match self {
            Self::ConversationMessage {
                message_id,
                conversation_id,
                sequence_number,
            } => format!(
                "{head};message_id={message_id};conversation_id={conversation_id};\
                 sequence_number={sequence_number}"
            ),
            Self::ConversationSummary {
                summary_id,
                conversation_id,
                covers_through_sequence,
            } => format!(
                "{head};summary_id={summary_id};conversation_id={conversation_id};\
                 covers_through_sequence={covers_through_sequence}"
            ),
            Self::MemoryRecord {
                memory_id,
                application_id,
                memory_scope,
            } => format!(
                "{head};memory_id={memory_id};application_id={application_id};\
                 memory_scope={memory_scope}"
            ),
            Self::RagDocumentVersion {
                version_id,
                document_id,
                version_number,
            } => format!(
                "{head};version_id={version_id};document_id={document_id};\
                 version_number={version_number}"
            ),
            Self::RagChunk {
                chunk_id,
                document_version_id,
                chunk_index,
            } => format!(
                "{head};chunk_id={chunk_id};document_version_id={document_version_id};\
                 chunk_index={chunk_index}"
            ),
        }
    }
}

/// A parsed, validated 42-byte header.
///
/// Parsing is separated from decryption so a caller can learn which data key a blob wants —
/// and refuse a blob outright — without holding any key at all. That ordering is the reason an
/// unknown `format_version` never reaches key custody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeHeader {
    pub format_version: u8,
    pub algorithm_id: u8,
    pub key_mode: u8,
    pub data_key_id: Uuid,
    pub nonce: [u8; ENVELOPE_NONCE_LEN],
    pub aad_profile: AadProfile,
    pub body_len: u32,
}

impl EnvelopeHeader {
    /// Validate the framing of `envelope` and return its header.
    ///
    /// Runs before any key lookup and before any crypto call. Order: length floor (forced
    /// first — the fields are unreadable otherwise), magic, format version, algorithm, key
    /// mode, reserved-is-zero, profile-known, then `body_len == len - 42`.
    ///
    /// `envelope` is attacker-influenced the moment anyone holds database write access, so
    /// every path out of here is a typed error and none of them can panic: the length floor is
    /// established before the first index.
    pub fn parse(envelope: &[u8]) -> Result<Self, ContentEnvelopeError> {
        if envelope.len() < MIN_ENVELOPE_LEN {
            return Err(ContentEnvelopeError::TooShort {
                len: envelope.len(),
            });
        }

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&envelope[OFF_MAGIC..OFF_MAGIC + 4]);
        if magic != ENVELOPE_MAGIC {
            return Err(ContentEnvelopeError::BadMagic { found: magic });
        }

        let format_version = envelope[OFF_FORMAT_VERSION];
        if format_version != FORMAT_VERSION_V1 {
            return Err(ContentEnvelopeError::UnsupportedFormatVersion { format_version });
        }

        let algorithm_id = envelope[OFF_ALGORITHM_ID];
        if algorithm_id != ALGORITHM_AES_256_GCM {
            return Err(ContentEnvelopeError::UnsupportedAlgorithm { algorithm_id });
        }

        let key_mode = envelope[OFF_KEY_MODE];
        if key_mode != KEY_MODE_WRAPPED_DEK {
            return Err(ContentEnvelopeError::UnsupportedKeyMode { key_mode });
        }

        let reserved = envelope[OFF_RESERVED];
        if reserved != 0 {
            return Err(ContentEnvelopeError::ReservedNotZero { reserved });
        }

        let mut key_id_bytes = [0u8; 16];
        key_id_bytes.copy_from_slice(&envelope[OFF_DATA_KEY_ID..OFF_DATA_KEY_ID + 16]);
        let data_key_id = Uuid::from_bytes(key_id_bytes);

        let mut nonce = [0u8; ENVELOPE_NONCE_LEN];
        nonce.copy_from_slice(&envelope[OFF_NONCE..OFF_NONCE + ENVELOPE_NONCE_LEN]);

        let profile_id =
            u16::from_be_bytes([envelope[OFF_AAD_PROFILE], envelope[OFF_AAD_PROFILE + 1]]);
        let aad_profile =
            AadProfile::from_id(profile_id).ok_or(ContentEnvelopeError::UnknownAadProfile {
                aad_profile: profile_id,
            })?;

        let body_len = u32::from_be_bytes([
            envelope[OFF_BODY_LEN],
            envelope[OFF_BODY_LEN + 1],
            envelope[OFF_BODY_LEN + 2],
            envelope[OFF_BODY_LEN + 3],
        ]);
        let actual = envelope.len() - ENVELOPE_HEADER_LEN;
        if u64::from(body_len) != actual as u64 {
            return Err(ContentEnvelopeError::BodyLengthMismatch {
                declared: body_len,
                actual,
            });
        }

        Ok(Self {
            format_version,
            algorithm_id,
            key_mode,
            data_key_id,
            nonce,
            aad_profile,
            body_len,
        })
    }

    /// Serialise back to the exact 42 bytes that prefix the blob and open the AAD.
    pub fn to_bytes(self) -> [u8; ENVELOPE_HEADER_LEN] {
        let mut out = [0u8; ENVELOPE_HEADER_LEN];
        out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&ENVELOPE_MAGIC);
        out[OFF_FORMAT_VERSION] = self.format_version;
        out[OFF_ALGORITHM_ID] = self.algorithm_id;
        out[OFF_KEY_MODE] = self.key_mode;
        out[OFF_RESERVED] = 0;
        out[OFF_DATA_KEY_ID..OFF_DATA_KEY_ID + 16].copy_from_slice(self.data_key_id.as_bytes());
        out[OFF_NONCE..OFF_NONCE + ENVELOPE_NONCE_LEN].copy_from_slice(&self.nonce);
        out[OFF_AAD_PROFILE..OFF_AAD_PROFILE + 2]
            .copy_from_slice(&self.aad_profile.id().to_be_bytes());
        out[OFF_BODY_LEN..OFF_BODY_LEN + 4].copy_from_slice(&self.body_len.to_be_bytes());
        out
    }
}

/// Build the AAD: `header_bytes || 0x00 || identity_string`.
///
/// Takes the **raw** 42 header bytes rather than a parsed [`EnvelopeHeader`], and that is
/// load-bearing. Re-serialising a parsed header would launder any byte the parser normalises —
/// `reserved`, most obviously, which [`EnvelopeHeader::to_bytes`] always writes as zero. The
/// decrypt path must authenticate the bytes that are actually stored, so that dropping or
/// weakening a framing check can never turn into silent acceptance of a tampered header.
///
/// Exposed so the whole authenticated surface can be asserted against a literal in tests
/// without going through a cipher.
pub fn envelope_aad(
    header_bytes: &[u8; ENVELOPE_HEADER_LEN],
    identity: &ContentIdentity<'_>,
) -> Vec<u8> {
    let identity_string = identity.identity_string();
    let mut aad = Vec::with_capacity(ENVELOPE_HEADER_LEN + 1 + identity_string.len());
    aad.extend_from_slice(header_bytes);
    aad.push(AAD_SEPARATOR);
    aad.extend_from_slice(identity_string.as_bytes());
    aad
}

/// A content data key, already unwrapped, held as a cipher object rather than as bytes.
///
/// After construction no Moira-owned type holds a printable DEK: the key is consumed into the
/// AES state and the `Debug` impl below is hand-written to print the key *id* and nothing else.
/// A derived `Debug` here would turn any stray `{:?}` into key material in a log aggregator.
#[derive(Clone)]
pub struct ContentCipher {
    data_key_id: Uuid,
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for ContentCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCipher")
            .field("data_key_id", &self.data_key_id)
            .finish_non_exhaustive()
    }
}

impl ContentCipher {
    /// Take an unwrapped 32-byte DEK and the keyring id it was stored under.
    ///
    /// The `Zeroizing` argument is the caller's; this constructor copies the bytes into the
    /// AES key schedule and never keeps a second copy of its own.
    pub fn new(data_key_id: Uuid, data_key: &Zeroizing<[u8; 32]>) -> Self {
        Self {
            data_key_id,
            cipher: Aes256Gcm::new_from_slice(data_key.as_slice()).expect("32-byte data key"),
        }
    }

    /// The keyring id written into every envelope this cipher seals.
    pub fn data_key_id(&self) -> Uuid {
        self.data_key_id
    }

    /// Encrypt `plaintext` for `identity` and return the complete envelope.
    ///
    /// The nonce is drawn fresh from the OS CSPRNG for every call. It is never derived from
    /// row identity and never a counter: a repeat under one key is catastrophic for GCM, and a
    /// derived nonce would repeat the moment a row was rewritten.
    pub fn seal(
        &self,
        identity: &ContentIdentity<'_>,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, ContentEnvelopeError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let mut nonce_bytes = [0u8; ENVELOPE_NONCE_LEN];
        nonce_bytes.copy_from_slice(nonce.as_slice());
        self.seal_with_nonce(identity, plaintext, nonce_bytes)
    }

    /// Deterministic seal. Private, and it stays private: the only legitimate caller is a test
    /// that pins a golden vector. Any production path that could choose a nonce is a path that
    /// can repeat one.
    fn seal_with_nonce(
        &self,
        identity: &ContentIdentity<'_>,
        plaintext: &[u8],
        nonce: [u8; ENVELOPE_NONCE_LEN],
    ) -> Result<Vec<u8>, ContentEnvelopeError> {
        // `body_len` is known before the ciphertext exists — GCM adds exactly the tag — which
        // is what lets the length live inside the AAD it protects.
        let body_len = plaintext
            .len()
            .checked_add(ENVELOPE_TAG_LEN)
            .and_then(|len| u32::try_from(len).ok())
            .ok_or(ContentEnvelopeError::PlaintextTooLarge {
                len: plaintext.len(),
            })?;

        let header = EnvelopeHeader {
            format_version: FORMAT_VERSION_V1,
            algorithm_id: ALGORITHM_AES_256_GCM,
            key_mode: KEY_MODE_WRAPPED_DEK,
            data_key_id: self.data_key_id,
            nonce,
            aad_profile: identity.profile(),
            body_len,
        };
        let header_bytes = header.to_bytes();
        let aad = envelope_aad(&header_bytes, identity);

        let body = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| ContentEnvelopeError::Encrypt)?;

        let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + body.len());
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Validate the framing of `envelope`, then decrypt it under `identity`.
    ///
    /// Every framing failure is decided before the cipher is touched, so a damaged blob is
    /// reported as damaged rather than as a tag failure that reads like tampering.
    pub fn open(
        &self,
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, ContentEnvelopeError> {
        let header = EnvelopeHeader::parse(envelope)?;

        if header.aad_profile != identity.profile() {
            return Err(ContentEnvelopeError::ProfileMismatch {
                header: header.aad_profile,
                identity: identity.profile(),
            });
        }
        if header.data_key_id != self.data_key_id {
            return Err(ContentEnvelopeError::DataKeyMismatch {
                header: header.data_key_id,
                cipher: self.data_key_id,
            });
        }

        // The bytes as stored, not a re-serialisation of the parsed header. See `envelope_aad`.
        let mut header_bytes = [0u8; ENVELOPE_HEADER_LEN];
        header_bytes.copy_from_slice(&envelope[..ENVELOPE_HEADER_LEN]);
        let aad = envelope_aad(&header_bytes, identity);

        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&header.nonce),
                Payload {
                    msg: &envelope[ENVELOPE_HEADER_LEN..],
                    aad: &aad,
                },
            )
            .map_err(|_| ContentEnvelopeError::Decrypt)?;
        Ok(Zeroizing::new(plaintext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::rand_core::RngCore;

    /// Obviously non-random, obviously test-only. Generated for this suite and deployed
    /// nowhere: a counting pattern is a key no operator could mistake for a real one.
    const GOLDEN_DEK: [u8; 32] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    const GOLDEN_NONCE: [u8; ENVELOPE_NONCE_LEN] = [
        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
    ];
    const GOLDEN_KEY_ID: Uuid = Uuid::from_bytes([
        0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x55, 0x55, 0x55,
        0x55,
    ]);
    const GOLDEN_MESSAGE_ID: Uuid = Uuid::from_bytes([
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10,
    ]);
    const GOLDEN_CONVERSATION_ID: Uuid = Uuid::from_bytes([
        0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
        0x30,
    ]);
    const GOLDEN_PLAINTEXT: &[u8] = b"moira content envelope golden vector";

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    fn unhex(text: &str) -> Vec<u8> {
        assert!(text.len().is_multiple_of(2), "hex literal must be even");
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("hex literal"))
            .collect()
    }

    fn cipher_with(key: [u8; 32], key_id: Uuid) -> ContentCipher {
        ContentCipher::new(key_id, &Zeroizing::new(key))
    }

    fn test_cipher() -> ContentCipher {
        cipher_with(
            [7u8; 32],
            Uuid::from_u128(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10),
        )
    }

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// One identity per profile, built by an **exhaustive** match so that a new profile cannot
    /// be added without deciding what it looks like here.
    fn identity_for(profile: AadProfile) -> ContentIdentity<'static> {
        match profile {
            AadProfile::ConversationMessageContent => ContentIdentity::ConversationMessage {
                message_id: uuid(1),
                conversation_id: uuid(2),
                sequence_number: 42,
            },
            AadProfile::ConversationSummaryText => ContentIdentity::ConversationSummary {
                summary_id: uuid(3),
                conversation_id: uuid(4),
                covers_through_sequence: 99,
            },
            AadProfile::MemoryRecordContent => ContentIdentity::MemoryRecord {
                memory_id: uuid(5),
                application_id: uuid(6),
                memory_scope: "application",
            },
            AadProfile::RagDocumentVersionContent => ContentIdentity::RagDocumentVersion {
                version_id: uuid(7),
                document_id: uuid(8),
                version_number: 3,
            },
            AadProfile::RagChunkText => ContentIdentity::RagChunk {
                chunk_id: uuid(9),
                document_version_id: uuid(10),
                chunk_index: 17,
            },
        }
    }

    /// Every single-field mutation of an identity, again by exhaustive match, so test 2 covers
    /// each bound field rather than whichever one someone remembered.
    fn mutations_of(identity: ContentIdentity<'static>) -> Vec<ContentIdentity<'static>> {
        match identity {
            ContentIdentity::ConversationMessage {
                message_id,
                conversation_id,
                sequence_number,
            } => vec![
                ContentIdentity::ConversationMessage {
                    message_id: uuid(0xdead),
                    conversation_id,
                    sequence_number,
                },
                ContentIdentity::ConversationMessage {
                    message_id,
                    conversation_id: uuid(0xdead),
                    sequence_number,
                },
                ContentIdentity::ConversationMessage {
                    message_id,
                    conversation_id,
                    sequence_number: sequence_number + 1,
                },
            ],
            ContentIdentity::ConversationSummary {
                summary_id,
                conversation_id,
                covers_through_sequence,
            } => vec![
                ContentIdentity::ConversationSummary {
                    summary_id: uuid(0xdead),
                    conversation_id,
                    covers_through_sequence,
                },
                ContentIdentity::ConversationSummary {
                    summary_id,
                    conversation_id: uuid(0xdead),
                    covers_through_sequence,
                },
                ContentIdentity::ConversationSummary {
                    summary_id,
                    conversation_id,
                    covers_through_sequence: covers_through_sequence + 1,
                },
            ],
            ContentIdentity::MemoryRecord {
                memory_id,
                application_id,
                memory_scope,
            } => vec![
                ContentIdentity::MemoryRecord {
                    memory_id: uuid(0xdead),
                    application_id,
                    memory_scope,
                },
                ContentIdentity::MemoryRecord {
                    memory_id,
                    application_id: uuid(0xdead),
                    memory_scope,
                },
                ContentIdentity::MemoryRecord {
                    memory_id,
                    application_id,
                    memory_scope: "tenant",
                },
            ],
            ContentIdentity::RagDocumentVersion {
                version_id,
                document_id,
                version_number,
            } => vec![
                ContentIdentity::RagDocumentVersion {
                    version_id: uuid(0xdead),
                    document_id,
                    version_number,
                },
                ContentIdentity::RagDocumentVersion {
                    version_id,
                    document_id: uuid(0xdead),
                    version_number,
                },
                ContentIdentity::RagDocumentVersion {
                    version_id,
                    document_id,
                    version_number: version_number + 1,
                },
            ],
            ContentIdentity::RagChunk {
                chunk_id,
                document_version_id,
                chunk_index,
            } => vec![
                ContentIdentity::RagChunk {
                    chunk_id: uuid(0xdead),
                    document_version_id,
                    chunk_index,
                },
                ContentIdentity::RagChunk {
                    chunk_id,
                    document_version_id: uuid(0xdead),
                    chunk_index,
                },
                ContentIdentity::RagChunk {
                    chunk_id,
                    document_version_id,
                    chunk_index: chunk_index + 1,
                },
            ],
        }
    }

    // ---------------------------------------------------------------- 1. round trip

    #[test]
    fn round_trip_every_profile_at_every_boundary_length() {
        let cipher = test_cipher();
        for profile in AadProfile::ALL {
            let identity = identity_for(profile);
            assert_eq!(
                identity.profile(),
                profile,
                "identity_for must stay aligned"
            );

            for len in [0usize, 1, 63, 64, 1024, 16 * 1024, 262_144] {
                let plaintext: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
                let envelope = cipher.seal(&identity, &plaintext).expect("seal");

                assert_eq!(envelope.len(), ENVELOPE_HEADER_LEN + len + ENVELOPE_TAG_LEN);
                let header = EnvelopeHeader::parse(&envelope).expect("parse");
                assert_eq!(header.aad_profile, profile);
                assert_eq!(header.format_version, FORMAT_VERSION_V1);
                assert_eq!(header.algorithm_id, ALGORITHM_AES_256_GCM);
                assert_eq!(header.key_mode, KEY_MODE_WRAPPED_DEK);
                assert_eq!(header.data_key_id, cipher.data_key_id());
                assert_eq!(header.body_len as usize, len + ENVELOPE_TAG_LEN);

                let opened = cipher.open(&envelope, &identity).expect("open");
                assert_eq!(opened.as_slice(), plaintext.as_slice());

                // The plaintext must not survive anywhere in the body. (The header is
                // deliberately excluded: it is public framing, and a 16-byte window of a UUID
                // key id can coincide with a synthetic plaintext without anything leaking.)
                if len >= 16 {
                    assert!(
                        !envelope[ENVELOPE_HEADER_LEN..]
                            .windows(16)
                            .any(|window| window == &plaintext[..16]),
                        "plaintext prefix leaked into the body"
                    );
                }
            }
        }
    }

    #[test]
    fn all_five_profiles_are_declared() {
        assert_eq!(AadProfile::ALL.len(), 5);
    }

    // ---------------------------------------------------------------- 2. AAD binding

    #[test]
    fn mutating_any_bound_identity_field_breaks_decryption() {
        let cipher = test_cipher();
        for profile in AadProfile::ALL {
            let identity = identity_for(profile);
            let envelope = cipher.seal(&identity, b"bound to this row").expect("seal");
            assert!(cipher.open(&envelope, &identity).is_ok());

            let mutations = mutations_of(identity);
            assert_eq!(
                mutations.len(),
                3,
                "profile {} must mutate all three bound fields",
                profile.id()
            );
            for mutated in mutations {
                assert_ne!(
                    mutated, identity,
                    "a mutation must actually change something"
                );
                assert_eq!(
                    mutated.profile(),
                    profile,
                    "a field mutation must not change the profile"
                );
                assert_eq!(
                    cipher.open(&envelope, &mutated),
                    Err(ContentEnvelopeError::Decrypt),
                    "profile {} accepted a mutated identity",
                    profile.id()
                );
            }
        }
    }

    #[test]
    fn opening_under_the_wrong_profile_is_refused_before_the_tag() {
        let cipher = test_cipher();
        let sealed_as = identity_for(AadProfile::ConversationMessageContent);
        let envelope = cipher.seal(&sealed_as, b"tenant a").expect("seal");
        let opened_as = identity_for(AadProfile::RagChunkText);

        assert_eq!(
            cipher.open(&envelope, &opened_as),
            Err(ContentEnvelopeError::ProfileMismatch {
                header: AadProfile::ConversationMessageContent,
                identity: AadProfile::RagChunkText,
            })
        );
    }

    #[test]
    fn a_cipher_holding_a_different_key_id_is_refused_before_the_tag() {
        let alice = cipher_with([7u8; 32], uuid(1));
        let bob = cipher_with([7u8; 32], uuid(2));
        let identity = identity_for(AadProfile::MemoryRecordContent);
        let envelope = alice.seal(&identity, b"tenant a").expect("seal");

        assert_eq!(
            bob.open(&envelope, &identity),
            Err(ContentEnvelopeError::DataKeyMismatch {
                header: uuid(1),
                cipher: uuid(2),
            })
        );
    }

    #[test]
    fn a_different_data_key_under_the_same_id_fails_the_tag() {
        let writer = cipher_with([7u8; 32], uuid(1));
        let reader = cipher_with([8u8; 32], uuid(1));
        let identity = identity_for(AadProfile::MemoryRecordContent);
        let envelope = writer.seal(&identity, b"tenant a").expect("seal");

        assert_eq!(
            reader.open(&envelope, &identity),
            Err(ContentEnvelopeError::Decrypt)
        );
    }

    // ---------------------------------------------------------------- 3. header tamper matrix

    /// Flip every one of the 336 header bits in turn. Every single flip must be refused.
    ///
    /// This is the proof that "self-describing" is also "authenticated": there is no header bit
    /// an attacker can move — algorithm, key mode, reserved, key id, nonce, profile, length —
    /// without the read failing.
    #[test]
    fn every_header_bit_flip_is_refused() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let envelope = cipher
            .seal(&identity, b"header tamper matrix")
            .expect("seal");

        let mut framing = 0usize;
        let mut tag = 0usize;
        for byte_index in 0..ENVELOPE_HEADER_LEN {
            for bit in 0..8u32 {
                let mut tampered = envelope.clone();
                tampered[byte_index] ^= 1 << bit;

                match cipher.open(&tampered, &identity) {
                    Ok(_) => panic!(
                        "header byte {byte_index} bit {bit} was flipped and the envelope still opened"
                    ),
                    Err(ContentEnvelopeError::Decrypt) => tag += 1,
                    Err(_) => framing += 1,
                }
            }
        }

        assert_eq!(framing + tag, ENVELOPE_HEADER_LEN * 8);
        assert_eq!(framing + tag, 336, "the header is 42 bytes, i.e. 336 bits");
        // Both arms must actually be exercised: an all-framing result would mean the tag never
        // ran, and an all-tag result would mean the framing checks are not doing their job.
        assert!(framing > 0 && tag > 0, "framing={framing} tag={tag}");
    }

    #[test]
    fn every_body_bit_flip_is_refused() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::RagChunkText);
        let envelope = cipher.seal(&identity, b"body tamper").expect("seal");

        for byte_index in ENVELOPE_HEADER_LEN..envelope.len() {
            for bit in 0..8u32 {
                let mut tampered = envelope.clone();
                tampered[byte_index] ^= 1 << bit;
                assert_eq!(
                    cipher.open(&tampered, &identity),
                    Err(ContentEnvelopeError::Decrypt),
                    "body byte {byte_index} bit {bit}"
                );
            }
        }
    }

    /// The bit-flip matrix above refuses a tampered `data_key_id` at the `DataKeyMismatch`
    /// check, which proves the *check* works but not that the field is authenticated. This
    /// removes the check from the picture — the cipher is given the tampered id — and shows
    /// the tag still refuses. Same for the profile, whose mismatch check would otherwise mask
    /// the tag.
    #[test]
    fn a_tampered_key_id_still_fails_the_tag_with_the_mismatch_check_out_of_the_way() {
        let cipher = cipher_with([7u8; 32], uuid(1));
        let identity = identity_for(AadProfile::MemoryRecordContent);
        let mut envelope = cipher.seal(&identity, b"authenticated").expect("seal");

        envelope[OFF_DATA_KEY_ID + 15] ^= 0x01;
        let tampered_id = EnvelopeHeader::parse(&envelope).expect("parse").data_key_id;
        assert_ne!(tampered_id, uuid(1));

        // Same key bytes, but an id that now agrees with the tampered header.
        let complicit = cipher_with([7u8; 32], tampered_id);
        assert_eq!(
            complicit.open(&envelope, &identity),
            Err(ContentEnvelopeError::Decrypt),
            "data_key_id is compared but not authenticated"
        );
    }

    #[test]
    fn a_tampered_profile_still_fails_the_tag_with_the_mismatch_check_out_of_the_way() {
        let cipher = test_cipher();
        let sealed_as = identity_for(AadProfile::ConversationMessageContent);
        let mut envelope = cipher.seal(&sealed_as, b"authenticated").expect("seal");

        envelope[OFF_AAD_PROFILE + 1] = AadProfile::RagChunkText.id() as u8;
        let header = EnvelopeHeader::parse(&envelope).expect("parse");
        assert_eq!(header.aad_profile, AadProfile::RagChunkText);

        // Opened with an identity that agrees with the tampered profile, so the mismatch check
        // cannot fire. Only the tag stands between this and a lifted ciphertext.
        assert_eq!(
            cipher.open(&envelope, &identity_for(AadProfile::RagChunkText)),
            Err(ContentEnvelopeError::Decrypt),
            "aad_profile is compared but not authenticated"
        );
    }

    /// `reserved` is normalised to zero by `to_bytes`, so an AAD rebuilt from a *parsed* header
    /// would launder a tampered reserved byte. The decrypt path must use the stored bytes.
    #[test]
    fn the_aad_is_the_stored_header_bytes_not_a_reserialisation() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let envelope = cipher.seal(&identity, b"verbatim").expect("seal");

        let mut header_bytes = [0u8; ENVELOPE_HEADER_LEN];
        header_bytes.copy_from_slice(&envelope[..ENVELOPE_HEADER_LEN]);
        let honest = envelope_aad(&header_bytes, &identity);

        // A tampered reserved byte must change the AAD. If `envelope_aad` took a parsed header
        // it could not, because `to_bytes` writes reserved as zero unconditionally.
        let mut tampered_bytes = header_bytes;
        tampered_bytes[OFF_RESERVED] = 0x01;
        let tampered = envelope_aad(&tampered_bytes, &identity);
        assert_ne!(honest, tampered);

        let mut normalised = EnvelopeHeader::parse(&envelope).expect("parse");
        normalised.format_version = FORMAT_VERSION_V1;
        assert_eq!(
            normalised.to_bytes().as_slice(),
            &envelope[..ENVELOPE_HEADER_LEN]
        );
    }

    #[test]
    fn each_header_field_has_its_own_discriminant() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let good = cipher.seal(&identity, b"discriminants").expect("seal");

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            EnvelopeHeader::parse(&bad_magic),
            Err(ContentEnvelopeError::BadMagic { found }) if found == *b"XOE1"
        ));

        let mut bad_version = good.clone();
        bad_version[OFF_FORMAT_VERSION] = 0x02;
        assert_eq!(
            EnvelopeHeader::parse(&bad_version),
            Err(ContentEnvelopeError::UnsupportedFormatVersion { format_version: 2 })
        );

        let mut bad_algorithm = good.clone();
        bad_algorithm[OFF_ALGORITHM_ID] = 0x02;
        assert_eq!(
            EnvelopeHeader::parse(&bad_algorithm),
            Err(ContentEnvelopeError::UnsupportedAlgorithm { algorithm_id: 2 })
        );

        let mut bad_key_mode = good.clone();
        bad_key_mode[OFF_KEY_MODE] = 0x02;
        assert_eq!(
            EnvelopeHeader::parse(&bad_key_mode),
            Err(ContentEnvelopeError::UnsupportedKeyMode { key_mode: 2 })
        );

        let mut reserved_set = good.clone();
        reserved_set[OFF_RESERVED] = 0x01;
        assert_eq!(
            EnvelopeHeader::parse(&reserved_set),
            Err(ContentEnvelopeError::ReservedNotZero { reserved: 1 })
        );

        let mut unknown_profile = good.clone();
        unknown_profile[OFF_AAD_PROFILE] = 0x00;
        unknown_profile[OFF_AAD_PROFILE + 1] = 0x63;
        assert_eq!(
            EnvelopeHeader::parse(&unknown_profile),
            Err(ContentEnvelopeError::UnknownAadProfile { aad_profile: 99 })
        );

        let mut wrong_len = good.clone();
        wrong_len[OFF_BODY_LEN + 3] = wrong_len[OFF_BODY_LEN + 3].wrapping_add(1);
        let declared = u32::from_be_bytes([
            wrong_len[OFF_BODY_LEN],
            wrong_len[OFF_BODY_LEN + 1],
            wrong_len[OFF_BODY_LEN + 2],
            wrong_len[OFF_BODY_LEN + 3],
        ]);
        assert_eq!(
            EnvelopeHeader::parse(&wrong_len),
            Err(ContentEnvelopeError::BodyLengthMismatch {
                declared,
                actual: good.len() - ENVELOPE_HEADER_LEN,
            })
        );

        // Trailing garbage is framing damage, not a tag failure.
        let mut extended = good.clone();
        extended.push(0x00);
        assert_eq!(
            EnvelopeHeader::parse(&extended),
            Err(ContentEnvelopeError::BodyLengthMismatch {
                declared: (good.len() - ENVELOPE_HEADER_LEN) as u32,
                actual: good.len() + 1 - ENVELOPE_HEADER_LEN,
            })
        );
    }

    // ---------------------------------------------------------------- 4. truncation and fuzz

    #[test]
    fn every_prefix_below_the_floor_is_a_framing_error() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationSummaryText);
        let envelope = cipher.seal(&identity, b"truncate me").expect("seal");
        assert!(envelope.len() > MIN_ENVELOPE_LEN);

        for len in 0..MIN_ENVELOPE_LEN {
            let prefix = &envelope[..len];
            assert_eq!(
                EnvelopeHeader::parse(prefix),
                Err(ContentEnvelopeError::TooShort { len }),
                "prefix of {len} bytes"
            );
            assert_eq!(
                cipher.open(prefix, &identity),
                Err(ContentEnvelopeError::TooShort { len })
            );
        }

        // A prefix at or above the floor is still refused — as truncation, not as a tag failure.
        for len in MIN_ENVELOPE_LEN..envelope.len() {
            assert!(matches!(
                cipher.open(&envelope[..len], &identity),
                Err(ContentEnvelopeError::BodyLengthMismatch { .. })
            ));
        }
    }

    /// A `bytea` column is attacker-influenced the moment anyone holds database write access,
    /// so the parser must return a typed error for arbitrary bytes and must never panic.
    #[test]
    fn ten_thousand_random_blobs_are_typed_errors_never_panics() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::RagDocumentVersionContent);
        let mut rng = OsRng;
        let mut accepted = 0usize;

        for _ in 0..10_000 {
            let len = (rng.next_u32() % 200) as usize;
            let mut blob = vec![0u8; len];
            rng.fill_bytes(&mut blob);

            // Half the blobs get a valid magic and header skeleton, so the fuzz reaches past
            // the very first check instead of bouncing off it ten thousand times.
            if len >= MIN_ENVELOPE_LEN && rng.next_u32().is_multiple_of(2) {
                blob[..4].copy_from_slice(&ENVELOPE_MAGIC);
                blob[OFF_FORMAT_VERSION] = FORMAT_VERSION_V1;
                blob[OFF_ALGORITHM_ID] = ALGORITHM_AES_256_GCM;
                blob[OFF_KEY_MODE] = KEY_MODE_WRAPPED_DEK;
                blob[OFF_RESERVED] = 0;
                blob[OFF_AAD_PROFILE] = 0;
                blob[OFF_AAD_PROFILE + 1] = AadProfile::RagDocumentVersionContent.id() as u8;
                let body_len = (len - ENVELOPE_HEADER_LEN) as u32;
                blob[OFF_BODY_LEN..OFF_BODY_LEN + 4].copy_from_slice(&body_len.to_be_bytes());
                blob[OFF_DATA_KEY_ID..OFF_DATA_KEY_ID + 16]
                    .copy_from_slice(cipher.data_key_id().as_bytes());
            }

            if cipher.open(&blob, &identity).is_ok() {
                accepted += 1;
            }
            let _ = EnvelopeHeader::parse(&blob);
        }

        assert_eq!(accepted, 0, "a random blob decrypted, which is impossible");
    }

    #[test]
    fn an_empty_blob_is_too_short_not_bad_magic() {
        assert_eq!(
            EnvelopeHeader::parse(&[]),
            Err(ContentEnvelopeError::TooShort { len: 0 })
        );
    }

    // ---------------------------------------------------------------- 5. golden vector

    /// If this test fails you have changed the on-disk format. That is a v2, not an edit.
    /// Every row already written under v1 must still open.
    #[test]
    fn golden_vector_v1() {
        let cipher = ContentCipher::new(GOLDEN_KEY_ID, &Zeroizing::new(GOLDEN_DEK));
        let identity = ContentIdentity::ConversationMessage {
            message_id: GOLDEN_MESSAGE_ID,
            conversation_id: GOLDEN_CONVERSATION_ID,
            sequence_number: 7,
        };

        assert_eq!(
            identity.identity_string(),
            "moira/content/v1;table=conversation_messages;column=content_encrypted;\
             message_id=01020304-0506-0708-090a-0b0c0d0e0f10;\
             conversation_id=21222324-2526-2728-292a-2b2c2d2e2f30;sequence_number=7"
        );

        let envelope = cipher
            .seal_with_nonce(&identity, GOLDEN_PLAINTEXT, GOLDEN_NONCE)
            .expect("seal");

        // If this test fails you have changed the on-disk format. That is a v2, not an edit.
        // Every row already written under v1 must still open.
        assert_eq!(
            hex(&envelope),
            concat!(
                "4d4f4531",                         // magic "MOE1"
                "01",                               // format_version
                "01",                               // algorithm_id = AES-256-GCM
                "01",                               // key_mode = wrapped DEK
                "00",                               // reserved
                "11111111222233334444555555555555", // data_key_id
                "a0a1a2a3a4a5a6a7a8a9aaab",         // nonce
                "0001",                             // aad_profile = 1
                "00000034",                         // body_len = 36 + 16
                "8b77155f24eb61d00c11e2bd735aa5b006c9357f",
                "e2d2620bf36242e3118b0364b102288dde773f05",
                "247df4310956e43b8c5eb9ea",
            ),
            "GOLDEN VECTOR CHANGED"
        );

        let opened = cipher.open(&envelope, &identity).expect("open");
        assert_eq!(opened.as_slice(), GOLDEN_PLAINTEXT);
    }

    /// The AAD itself, pinned. The golden vector above already covers it transitively, but a
    /// direct assertion says *which* byte moved when the envelope hex changes.
    #[test]
    fn golden_aad_bytes_v1() {
        let identity = ContentIdentity::ConversationMessage {
            message_id: GOLDEN_MESSAGE_ID,
            conversation_id: GOLDEN_CONVERSATION_ID,
            sequence_number: 7,
        };
        let header = EnvelopeHeader {
            format_version: FORMAT_VERSION_V1,
            algorithm_id: ALGORITHM_AES_256_GCM,
            key_mode: KEY_MODE_WRAPPED_DEK,
            data_key_id: GOLDEN_KEY_ID,
            nonce: GOLDEN_NONCE,
            aad_profile: AadProfile::ConversationMessageContent,
            body_len: (GOLDEN_PLAINTEXT.len() + ENVELOPE_TAG_LEN) as u32,
        };
        let aad = envelope_aad(&header.to_bytes(), &identity);

        assert_eq!(&aad[..ENVELOPE_HEADER_LEN], &header.to_bytes());
        assert_eq!(aad[ENVELOPE_HEADER_LEN], 0x00);
        assert_eq!(
            std::str::from_utf8(&aad[ENVELOPE_HEADER_LEN + 1..]).unwrap(),
            identity.identity_string()
        );
        // The three assertions above are written in terms of `to_bytes` and `identity_string`,
        // so they would survive a coordinated change to both. This literal would not.
        assert_eq!(
            hex(&aad),
            concat!(
                "4d4f45310101010011111111222233334444555555555555",
                "a0a1a2a3a4a5a6a7a8a9aaab000100000034",
                "00", // the separator
                "6d6f6972612f636f6e74656e742f76313b7461626c653d63",
                "6f6e766572736174696f6e5f6d657373616765733b636f6c",
                "756d6e3d636f6e74656e745f656e637279707465643b6d65",
                "73736167655f69643d30313032303330342d303530362d30",
                "3730382d303930612d3062306330643065306631303b636f",
                "6e766572736174696f6e5f69643d32313232323332342d32",
                "3532362d323732382d323932612d32623263326432653266",
                "33303b73657175656e63655f6e756d6265723d37",
            )
        );
    }

    // ---------------------------------------------------------------- 6. backward compatibility

    /// A blob and its key, both frozen, both generated for this test suite and deployed
    /// nowhere. One vector per `format_version`, kept forever.
    ///
    /// This is the only test that catches "we changed the format *and* the encoder, so the
    /// round-trip still passes". The golden vector pins what the encoder writes; this pins what
    /// the decoder must still be able to read.
    const COMPAT_V1_DEK: [u8; 32] = [
        0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xcb, 0xcc, 0xcd, 0xce,
        0xcf, 0xd0, 0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xdb, 0xdc, 0xdd,
        0xde, 0xdf,
    ];
    const COMPAT_V1_ENVELOPE_HEX: &str = concat!(
        "4d4f453101010100ababababcdcdefef0123456789abcdef",
        "5e110d1a7c339042b608e477000500000024",
        "ddb8f81f4a78359a0a40c0e49411d1475db40a40",
        "191cce3bd50c2880680281427aea5226",
    );
    const COMPAT_V1_PLAINTEXT: &[u8] = b"v1 must open forever";

    #[test]
    fn a_v1_blob_written_before_this_build_still_opens() {
        let cipher = ContentCipher::new(
            Uuid::from_bytes([
                0xab, 0xab, 0xab, 0xab, 0xcd, 0xcd, 0xef, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef,
            ]),
            &Zeroizing::new(COMPAT_V1_DEK),
        );
        let identity = ContentIdentity::RagChunk {
            chunk_id: Uuid::from_u128(0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111),
            document_version_id: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
            chunk_index: 5,
        };

        let envelope = unhex(COMPAT_V1_ENVELOPE_HEX);
        let header = EnvelopeHeader::parse(&envelope).expect("a frozen v1 blob must still parse");
        assert_eq!(header.format_version, FORMAT_VERSION_V1);
        assert_eq!(header.aad_profile, AadProfile::RagChunkText);

        let opened = cipher
            .open(&envelope, &identity)
            .expect("a frozen v1 blob must still decrypt");
        assert_eq!(opened.as_slice(), COMPAT_V1_PLAINTEXT);
    }

    // ---------------------------------------------------------------- 7. no custody on refusal

    /// A custody double that panics on every call. If an unknown `format_version` ever reached
    /// key custody, this test would abort rather than fail politely.
    #[derive(Debug)]
    struct ExplodingCustody;

    #[async_trait::async_trait]
    impl crate::security::MasterKeyCustody for ExplodingCustody {
        fn backend_name(&self) -> &'static str {
            "exploding"
        }
        fn active_master_key_id(&self) -> &str {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        fn can_unwrap(&self, _master_key_id: &str) -> bool {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        fn wrap_algorithm(&self) -> &'static str {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        fn master_key_ids(&self) -> Vec<String> {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        async fn wrap(
            &self,
            _dek: &Zeroizing<[u8; 32]>,
            _aad: &[u8],
        ) -> Result<crate::security::WrappedKey, crate::security::KeyCustodyError> {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        async fn wrap_under(
            &self,
            _master_key_id: &str,
            _dek: &Zeroizing<[u8; 32]>,
            _aad: &[u8],
        ) -> Result<crate::security::WrappedKey, crate::security::KeyCustodyError> {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        async fn unwrap(
            &self,
            _wrapped: &crate::security::WrappedKey,
            _aad: &[u8],
        ) -> Result<Zeroizing<[u8; 32]>, crate::security::KeyCustodyError> {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
        async fn preflight(&self) -> Result<(), crate::security::KeyCustodyError> {
            panic!("custody was consulted for a blob this build cannot interpret");
        }
    }

    /// Mirrors the read path a later PR will build: parse the header, *then* resolve the key.
    /// The custody double proves the second step is never reached for a v2 blob.
    fn open_via_custody(
        envelope: &[u8],
        identity: &ContentIdentity<'_>,
        custody: &dyn crate::security::MasterKeyCustody,
    ) -> Result<Zeroizing<Vec<u8>>, ContentEnvelopeError> {
        let header = EnvelopeHeader::parse(envelope)?;
        // Only now would a real caller consult the keyring, which consults custody.
        let _ = custody.active_master_key_id();
        let cipher = test_cipher();
        assert_eq!(header.data_key_id, cipher.data_key_id());
        cipher.open(envelope, identity)
    }

    #[test]
    fn an_unknown_format_version_is_refused_without_consulting_custody() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let mut envelope = cipher.seal(&identity, b"from the future").expect("seal");
        envelope[OFF_FORMAT_VERSION] = 0x02;

        let result = open_via_custody(&envelope, &identity, &ExplodingCustody);
        assert_eq!(
            result,
            Err(ContentEnvelopeError::UnsupportedFormatVersion { format_version: 2 })
        );
    }

    #[test]
    fn every_framing_refusal_happens_without_consulting_custody() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let good = cipher.seal(&identity, b"framing first").expect("seal");

        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            good[..MIN_ENVELOPE_LEN - 1].to_vec(),
            b"not an envelope at all, but long enough to hold a header and a tag".to_vec(),
        ];
        for (offset, value) in [
            (OFF_FORMAT_VERSION, 0x02u8),
            (OFF_ALGORITHM_ID, 0x02),
            (OFF_KEY_MODE, 0x02),
            (OFF_RESERVED, 0x01),
            (OFF_AAD_PROFILE + 1, 0x63),
            (OFF_BODY_LEN + 3, 0x00),
        ] {
            let mut blob = good.clone();
            blob[offset] = value;
            cases.push(blob);
        }

        for blob in cases {
            // Reaching custody would panic, not fail; every one of these must return an error.
            assert!(open_via_custody(&blob, &identity, &ExplodingCustody).is_err());
        }
    }

    #[test]
    fn the_custody_double_really_does_explode_when_reached() {
        // Without this, the two tests above would still pass if the double were silently
        // inert, which would make them prove nothing at all.
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let envelope = cipher.seal(&identity, b"well formed").expect("seal");

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            open_via_custody(&envelope, &identity, &ExplodingCustody)
        }));
        assert!(
            outcome.is_err(),
            "the custody double must panic when reached"
        );
    }

    // ---------------------------------------------------------------- 8. registry pinning

    #[test]
    fn profile_ids_are_unique_and_round_trip() {
        let mut seen = Vec::new();
        for profile in AadProfile::ALL {
            assert!(
                !seen.contains(&profile.id()),
                "profile id {} is used twice",
                profile.id()
            );
            seen.push(profile.id());
            assert_eq!(AadProfile::from_id(profile.id()), Some(profile));
        }
        assert_eq!(seen, vec![1, 2, 3, 4, 5]);
        assert_eq!(AadProfile::from_id(0), None);
        assert_eq!(AadProfile::from_id(6), None);
        assert_eq!(AadProfile::from_id(u16::MAX), None);
    }

    /// Every identity string, pinned against a literal.
    ///
    /// A rename in a future refactor — a field, a table, a column, the prefix — changes the AAD
    /// and orphans every row already written under the old string. These literals are what turn
    /// that from a silent data-loss event into a failing test.
    #[test]
    fn identity_strings_are_pinned_against_literals() {
        let message = ContentIdentity::ConversationMessage {
            message_id: uuid(0x11),
            conversation_id: uuid(0x22),
            sequence_number: -1,
        };
        assert_eq!(
            message.identity_string(),
            "moira/content/v1;table=conversation_messages;column=content_encrypted;\
             message_id=00000000-0000-0000-0000-000000000011;\
             conversation_id=00000000-0000-0000-0000-000000000022;sequence_number=-1"
        );

        let summary = ContentIdentity::ConversationSummary {
            summary_id: uuid(0x33),
            conversation_id: uuid(0x44),
            covers_through_sequence: 128,
        };
        assert_eq!(
            summary.identity_string(),
            "moira/content/v1;table=conversation_summaries;column=summary_text_encrypted;\
             summary_id=00000000-0000-0000-0000-000000000033;\
             conversation_id=00000000-0000-0000-0000-000000000044;covers_through_sequence=128"
        );

        let memory = ContentIdentity::MemoryRecord {
            memory_id: uuid(0x55),
            application_id: uuid(0x66),
            memory_scope: "application",
        };
        assert_eq!(
            memory.identity_string(),
            "moira/content/v1;table=memory_records;column=content_encrypted;\
             memory_id=00000000-0000-0000-0000-000000000055;\
             application_id=00000000-0000-0000-0000-000000000066;memory_scope=application"
        );

        let version = ContentIdentity::RagDocumentVersion {
            version_id: uuid(0x77),
            document_id: uuid(0x88),
            version_number: 12,
        };
        assert_eq!(
            version.identity_string(),
            "moira/content/v1;table=rag_document_versions;column=content_encrypted;\
             version_id=00000000-0000-0000-0000-000000000077;\
             document_id=00000000-0000-0000-0000-000000000088;version_number=12"
        );

        let chunk = ContentIdentity::RagChunk {
            chunk_id: uuid(0x99),
            document_version_id: uuid(0xaa),
            chunk_index: 0,
        };
        assert_eq!(
            chunk.identity_string(),
            "moira/content/v1;table=rag_chunks;column=chunk_text_encrypted;\
             chunk_id=00000000-0000-0000-0000-000000000099;\
             document_version_id=00000000-0000-0000-0000-0000000000aa;chunk_index=0"
        );
    }

    #[test]
    fn profile_tables_and_columns_are_pinned() {
        let pinned = [
            (
                AadProfile::ConversationMessageContent,
                1u16,
                "conversation_messages",
                "content_encrypted",
            ),
            (
                AadProfile::ConversationSummaryText,
                2,
                "conversation_summaries",
                "summary_text_encrypted",
            ),
            (
                AadProfile::MemoryRecordContent,
                3,
                "memory_records",
                "content_encrypted",
            ),
            (
                AadProfile::RagDocumentVersionContent,
                4,
                "rag_document_versions",
                "content_encrypted",
            ),
            (
                AadProfile::RagChunkText,
                5,
                "rag_chunks",
                "chunk_text_encrypted",
            ),
        ];
        assert_eq!(pinned.len(), AadProfile::ALL.len());
        for (profile, id, table, column) in pinned {
            assert_eq!(profile.id(), id);
            assert_eq!(profile.table(), table);
            assert_eq!(profile.column(), column);
        }
    }

    // ---------------------------------------------------------------- 9. leak test

    #[test]
    fn debug_reveals_neither_plaintext_nor_key_bytes() {
        let key = [0x5au8; 32];
        let cipher = cipher_with(key, uuid(0xfeed));
        let identity = ContentIdentity::MemoryRecord {
            memory_id: uuid(0xbeef),
            application_id: uuid(0xcafe),
            memory_scope: "application",
        };
        let plaintext = b"top secret memory content";
        let envelope = cipher.seal(&identity, plaintext).expect("seal");
        let header = EnvelopeHeader::parse(&envelope).expect("parse");
        let opened = cipher.open(&envelope, &identity).expect("open");

        let rendered = format!(
            "{cipher:?} {identity:?} {header:?} {:?} {:?} {:?}",
            AadProfile::ALL,
            ContentEnvelopeError::Decrypt,
            ContentEnvelopeError::DataKeyMismatch {
                header: uuid(1),
                cipher: uuid(2)
            },
        );

        assert!(
            !rendered.contains("top secret"),
            "Debug leaked plaintext: {rendered}"
        );
        assert!(
            !rendered.contains(&hex(plaintext)),
            "Debug leaked hex plaintext: {rendered}"
        );
        assert!(
            !rendered.contains(&hex(&key)),
            "Debug leaked the key: {rendered}"
        );
        assert!(!rendered.contains("90, 90, 90"), "Debug leaked key bytes");
        assert!(!rendered.contains("5a5a5a5a"), "Debug leaked key bytes");
        // `Zeroizing<Vec<u8>>` is the caller's to hold; it is never rendered by us, but assert
        // the value we hand back is at least the plaintext so the test above is meaningful.
        assert_eq!(opened.as_slice(), plaintext);
        // The cipher's Debug must still say which key it holds — that is the operable part.
        assert!(format!("{cipher:?}").contains(&uuid(0xfeed).to_string()));
    }

    // ---------------------------------------------------------------- 10. nonce distinctness

    /// 100k seals under one key must produce 100k distinct nonces. A repeat under one key is
    /// catastrophic for GCM, which is why the nonce is drawn from the CSPRNG per call and never
    /// derived from anything.
    #[test]
    fn one_hundred_thousand_seals_produce_distinct_nonces() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let mut nonces = std::collections::HashSet::with_capacity(100_000);

        for _ in 0..100_000 {
            let envelope = cipher.seal(&identity, b"n").expect("seal");
            let header = EnvelopeHeader::parse(&envelope).expect("parse");
            assert!(
                nonces.insert(header.nonce),
                "a nonce repeated under one key"
            );
        }
        assert_eq!(nonces.len(), 100_000);
    }

    #[test]
    fn sealing_the_same_row_twice_produces_different_bytes() {
        let cipher = test_cipher();
        let identity = identity_for(AadProfile::ConversationMessageContent);
        let first = cipher.seal(&identity, b"same plaintext").expect("seal");
        let second = cipher.seal(&identity, b"same plaintext").expect("seal");
        assert_ne!(first, second, "the nonce must not be derived from identity");
        assert_eq!(first.len(), second.len());
        assert_eq!(
            cipher.open(&first, &identity).unwrap().as_slice(),
            b"same plaintext"
        );
        assert_eq!(
            cipher.open(&second, &identity).unwrap().as_slice(),
            b"same plaintext"
        );
    }

    // ---------------------------------------------------------------- the column allowlist

    /// The three distinct identifiers the five sealed columns are spelled with.
    ///
    /// Three rather than five because `content_encrypted` names the column on
    /// `conversation_messages`, `memory_records` **and** `rag_document_versions`. None of the
    /// three is a substring of another, so counting them cannot double-count.
    const SEALED_COLUMN_IDENTIFIERS: [&str; 3] = [
        "content_encrypted",
        "summary_text_encrypted",
        "chunk_text_encrypted",
    ];

    /// **Every function in `src/` that may name a sealed column inside a SQL statement.**
    ///
    /// # What this is guarding
    ///
    /// A sealed column is a `bytea` that must only ever receive an envelope produced by
    /// [`ContentCipher::seal`] under the right [`ContentIdentity`], and must only ever be read
    /// back through [`crate::security::ContentOpener`]. Nothing in the type system says so: the
    /// column takes any `Vec<u8>`, and `sqlx` will bind one happily. A future PR that writes
    /// `.bind(some_bytes)` into `chunk_text_encrypted` — a compressed blob, a serialised struct,
    /// a plaintext body cast to bytes — compiles, passes every round-trip test written against
    /// it, and produces rows that the whole rest of this subsystem believes are sealed.
    ///
    /// So the gate is a **source walk**, in the spirit of the coded-error-literal gate in
    /// `src/i18n/catalog/mod.rs`: it does not check that a site is correct, only that the set of
    /// sites has not moved without someone saying so. A red here means "somebody taught a new
    /// function to name a sealed column in SQL — go and read it".
    ///
    /// It is **bidirectional**: a site appearing and a site disappearing both fail, because a
    /// guard that only counts upward cannot notice a writer being deleted.
    ///
    /// # Deliberately not in the list, and why that is the point
    ///
    /// `src/security/keyring_admin.rs` performs `reseal`, which is the only code in the tree that
    /// *updates* a sealed column in place — and it does not appear here, because it never spells
    /// a column name. It builds the statement from [`AadProfile::column`], so the registry in
    /// this module is the single source of the identifier. That is the shape a new site should
    /// copy.
    const SEALED_COLUMN_SQL_SITES: &[(&str, &str)] = &[
        // The four conversation- and memory-side writers and readers wired by issues #139/#140.
        ("src/infra/repositories/conversation.rs", "add_message"),
        ("src/infra/repositories/conversation.rs", "create_memory"),
        (
            "src/infra/repositories/conversation.rs",
            "find_active_conversation_summary",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "find_conversation_context_anchor",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "find_messages_after_sequence",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "find_recent_messages",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "insert_conversation_summary",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "insert_extracted_memory",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "memory_candidates_sql",
        ),
        ("src/infra/repositories/conversation.rs", "patch_memory"),
        // The RAG pair wired by issue #141: two writers for profile 4, one for profile 5, and
        // the one reader that opens profile 5.
        (
            "src/infra/repositories/conversation.rs",
            "create_rag_document_with_connection",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "ingest_rag_document_with_connection",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "write_rag_ingestion_artifacts",
        ),
        (
            "src/infra/repositories/conversation.rs",
            "find_rag_chunk_candidates",
        ),
        // The rotation suite's row seeder. It writes all five columns directly, on purpose:
        // `reseal` has to be given pre-existing envelopes to move, and a fixture that went
        // through the repositories would be testing the repositories.
        ("src/security/keyring_admin/tests.rs", "seed"),
    ];

    /// Statements a literal must look like before its column mentions count.
    ///
    /// Without this the walk would flag every doc comment, every column-name argument to
    /// [`crate::infra::pg_rows::conversation_content_from_row`], and the migration text quoted in
    /// `src/i18n/catalog/errors.rs` — none of which can put bytes in a column. The narrow list is
    /// what keeps a red here meaning something.
    const SQL_STATEMENT_MARKERS: [&str; 3] = ["select", "insert into", "update "];

    /// Every `.rs` file under `dir`, sorted, so a failure names the same file on every machine.
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

    /// Every string literal in `source`, paired with the 1-based line it starts on.
    ///
    /// Handles raw strings (`r"…"`, `r#"…"#`, and more hashes), ordinary strings with escapes,
    /// and skips `//` line comments so that prose is never mistaken for a statement. A `//`
    /// inside a string is not a comment, which is why this walks characters rather than lines.
    fn string_literals(source: &str) -> Vec<(usize, String)> {
        let bytes: Vec<char> = source.chars().collect();
        let mut out = Vec::new();
        let mut index = 0usize;
        let mut line = 1usize;
        while index < bytes.len() {
            let current = bytes[index];
            if current == '\n' {
                line += 1;
                index += 1;
                continue;
            }
            // A `//` comment, but only outside a string — which is where we are.
            if current == '/' && bytes.get(index + 1) == Some(&'/') {
                while index < bytes.len() && bytes[index] != '\n' {
                    index += 1;
                }
                continue;
            }
            // A raw string: `r`, some hashes, a quote; terminated by a quote and the same hashes.
            if current == 'r'
                && matches!(bytes.get(index + 1), Some('"') | Some('#'))
                && index
                    .checked_sub(1)
                    .is_none_or(|before| !bytes[before].is_alphanumeric() && bytes[before] != '_')
            {
                let mut cursor = index + 1;
                let mut hashes = 0usize;
                while bytes.get(cursor) == Some(&'#') {
                    hashes += 1;
                    cursor += 1;
                }
                if bytes.get(cursor) == Some(&'"') {
                    let start_line = line;
                    cursor += 1;
                    let mut body = String::new();
                    loop {
                        if cursor >= bytes.len() {
                            break;
                        }
                        if bytes[cursor] == '"'
                            && (1..=hashes).all(|offset| bytes.get(cursor + offset) == Some(&'#'))
                        {
                            cursor += 1 + hashes;
                            break;
                        }
                        if bytes[cursor] == '\n' {
                            line += 1;
                        }
                        body.push(bytes[cursor]);
                        cursor += 1;
                    }
                    out.push((start_line, body));
                    index = cursor;
                    continue;
                }
            }
            if current == '"' {
                let start_line = line;
                let mut cursor = index + 1;
                let mut body = String::new();
                while cursor < bytes.len() {
                    if bytes[cursor] == '\\' {
                        cursor += 2;
                        continue;
                    }
                    if bytes[cursor] == '"' {
                        cursor += 1;
                        break;
                    }
                    if bytes[cursor] == '\n' {
                        line += 1;
                    }
                    body.push(bytes[cursor]);
                    cursor += 1;
                }
                out.push((start_line, body));
                index = cursor;
                continue;
            }
            index += 1;
        }
        out
    }

    /// The name of the function each 1-based line belongs to, or `"<module level>"`.
    fn function_at_each_line(source: &str) -> Vec<String> {
        let mut current = "<module level>".to_string();
        let mut out = vec![current.clone()];
        for raw in source.lines() {
            let trimmed = raw.trim_start();
            let after_visibility = trimmed
                .strip_prefix("pub(crate) ")
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .or_else(|| trimmed.strip_prefix("pub "))
                .unwrap_or(trimmed);
            let after_async = after_visibility
                .strip_prefix("async ")
                .unwrap_or(after_visibility);
            let after_const = after_async.strip_prefix("const ").unwrap_or(after_async);
            if let Some(rest) = after_const.strip_prefix("fn ") {
                let name: String = rest
                    .chars()
                    .take_while(|character| character.is_alphanumeric() || *character == '_')
                    .collect();
                if !name.is_empty() {
                    current = name;
                }
            }
            out.push(current.clone());
        }
        out
    }

    /// `--` starts a comment inside a SQL statement. Stripping it means a comment *explaining*
    /// why a column is projected does not itself count as projecting it.
    fn without_sql_comments(literal: &str) -> String {
        literal
            .lines()
            .map(|line| line.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_sealed_column_named_in_sql_sits_in_an_allowlisted_function() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_sources_under(&manifest.join("src"), &mut files);
        assert!(
            files.len() > 40,
            "the source walker found only {} files under src/ — it is broken, and a broken \
             walker asserts nothing",
            files.len()
        );

        let mut found: Vec<(String, String)> = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            let functions = function_at_each_line(&source);
            for (line, literal) in string_literals(&source) {
                let lowered = literal.to_ascii_lowercase();
                if !SQL_STATEMENT_MARKERS
                    .iter()
                    .any(|marker| lowered.contains(marker))
                {
                    continue;
                }
                let statement = without_sql_comments(&literal);
                if !SEALED_COLUMN_IDENTIFIERS
                    .iter()
                    .any(|column| statement.contains(column))
                {
                    continue;
                }
                let relative = file
                    .strip_prefix(manifest)
                    .unwrap_or(file)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                let function = functions
                    .get(line)
                    .cloned()
                    .unwrap_or_else(|| "<module level>".to_string());
                found.push((relative, function));
            }
        }
        found.sort();
        found.dedup();

        let mut expected: Vec<(String, String)> = SEALED_COLUMN_SQL_SITES
            .iter()
            .map(|(file, function)| ((*file).to_string(), (*function).to_string()))
            .collect();
        expected.sort();
        expected.dedup();

        assert!(
            !found.is_empty(),
            "the walk found no SQL naming a sealed column at all. Either every writer was \
             deleted, or the literal scanner stopped working — and a scanner that finds nothing \
             passes an allowlist that is also empty"
        );
        assert_eq!(
            found, expected,
            "the set of functions naming a sealed column in SQL has changed.\n\n\
             These five columns hold AES-256-GCM envelopes and nothing else. A function that \
             binds raw bytes into one, or reads one without going through a ContentOpener, \
             produces rows the rest of this subsystem believes are sealed and that nothing can \
             open. If the new site is correct — it seals through ContentCipher under the right \
             ContentIdentity, or opens through ContentOpener — add it to \
             SEALED_COLUMN_SQL_SITES and say why in the same commit. If a site vanished, say \
             which reader lost its sealed column."
        );
    }

    // ---------------------------------------------------------------- constants

    #[test]
    fn the_format_constants_are_what_the_decision_record_says() {
        assert_eq!(ENVELOPE_MAGIC, *b"MOE1");
        assert_eq!(ENVELOPE_HEADER_LEN, 42);
        assert_eq!(MIN_ENVELOPE_LEN, 58);
        assert_eq!(ENVELOPE_NONCE_LEN, 12);
        assert_eq!(ENVELOPE_TAG_LEN, 16);
        assert_eq!(OFF_DATA_KEY_ID, 8);
        assert_eq!(OFF_NONCE, 24);
        assert_eq!(OFF_AAD_PROFILE, 36);
        assert_eq!(OFF_BODY_LEN, 38);
        assert_eq!(IDENTITY_PREFIX, "moira/content/v1");
    }
}
