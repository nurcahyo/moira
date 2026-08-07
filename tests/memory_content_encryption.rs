//! `memory_records` under every `conversation_content_persistence` value, and the `content_hash`
//! guessing oracle — issue #140, widened to all four policy values by issue #168.
//!
//! # The hole this file exists to prove closed
//!
//! Since migration `0021`, `memory_records.content_hash` was an **unkeyed SHA-256 of the
//! plaintext**. Memory bodies are short, low-entropy and highly guessable — "user prefers dark
//! mode", "user's timezone is Asia/Jakarta" — so that digest is an offline verifier: a database
//! dump plus a wordlist recovers the body for free.
//!
//! #140 closed that for `encrypted_content`, where the digest sat beside its own ciphertext and
//! undid the encryption outright. It **named the residual it left**: under `none` and
//! `metadata_only` the row stores no body and still stored the unkeyed digest, so the operator who
//! picked the strongest privacy value got the weakest outcome. #168 took that decision and keyed
//! the digest under **all four** values, so there is no branch left that can select an unkeyed
//! one.
//!
//! So `an_encrypted_memory_leaves_no_plaintext_in_the_raw_column` is necessary but nowhere near
//! sufficient here, and that is the difference between this file and
//! `tests/conversation_content_encryption.rs`. The load-bearing cases are
//! `a_sealed_memory_hash_is_keyed_and_is_not_the_digest_of_its_own_plaintext` and
//! `no_memory_row_carries_an_unkeyed_digest_under_any_policy`.
//!
//! # The claims the design rests on, tested rather than asserted in prose
//!
//! **`d1:` is unambiguous.** A `request_hash` value is unpadded base64url over a fixed 32-byte
//! digest, so it can never contain `:` — migration `0021`'s own rule. A row written before the
//! change and one written after it therefore **miss, never falsely match**, and
//! `a_pre_change_unkeyed_hash_never_matches_a_keyed_hash_under_any_policy` drives both directions
//! under every policy value rather than reasoning from one.
//!
//! **Dedupe still works where no body is stored.** `dedupe_still_matches_under_none_and_metadata_only`
//! is the feature #168 put a key dependency on; if keying those arms had broken them, the honest
//! answer would have been the issue's option 2 (drop the digest) rather than this.
//!
//! **The dedupe key rides master-key rotation.** It is a `content_data_keys` row wrapped by the
//! master key, so a rotation re-wraps the envelope and the 32 bytes inside never change.
//! `memory_dedupe_hashes_survive_a_master_key_rotation` performs a real rotation — rewrap, then
//! a fresh `AppState` holding **only** the new master key — and asserts every stored
//! `content_hash` is byte-identical afterwards, over rows written under **all four** policies
//! rather than trusting the sealed-only version of that test to generalise. That is what
//! dissolves F14's objection to a keyed hash on a table with no retention, and a test that never
//! rotates would prove none of it.
//!
//! # What #168 costs, and where that shows up here
//!
//! Keying every arm makes dedupe under `none` and `metadata_only` depend on a key those
//! deployments have no other use for.
//! `a_memory_write_with_no_keyring_is_refused_under_every_policy` is where that cost is visible as
//! behaviour: a write that cannot key its digest is refused rather than falling back, under a
//! policy that stores no body at all.

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::STANDARD};
use moira::{
    app::AppState,
    application::ConversationService,
    config::Settings,
    domain::{
        ContentWrite, ConversationContentPersistence, ConversationPolicyPutRequest,
        MemoryCreateRequest, MemoryPatchRequest, MemoryRecord, MemoryType,
    },
    infra::repositories::{ConversationRepository, MemoryInsert, PgConversationRepository},
    security::{
        Actor, ENVELOPE_HEADER_LEN, ENVELOPE_MAGIC, EnvironmentMasterKeyCustody, KeyringAdmin,
        MEMORY_DEDUPE_HASH_PREFIX, MasterKeyCustody, request_hash,
    },
};
use serde_json::json;
use sqlx::Row;
use support::{LifecycleFixture, request_context};
use uuid::Uuid;
use zeroize::Zeroizing;

const WAIT: Duration = Duration::from_secs(20);

/// High-entropy, so a byte-substring search over raw ciphertext cannot match by accident — and
/// so an equality against `content_plain` is exact.
const BODY: &str = "MOE1-CANARY-4f2a9e17b03c6d85-MEMORY-RECORD-BODY";
/// A second, equally distinctive body, for the patch and never-match cases.
const SECOND_BODY: &str = "MOE1-CANARY-9d31c7f0ae54b26a-SECOND-MEMORY-BODY";

/// Two master keys, so a rotation is a rotation rather than a relabelling.
const MASTER_A: [u8; 32] = [0x31; 32];
const MASTER_B: [u8; 32] = [0x32; 32];

/// `"id:base64,id:base64"`, the shape `content_encryption.keys` parses.
fn keys_setting(entries: &[(&str, [u8; 32])]) -> String {
    entries
        .iter()
        .map(|(id, bytes)| format!("{id}:{}", STANDARD.encode(bytes)))
        .collect::<Vec<_>>()
        .join(",")
}

/// The environment custody a `KeyringAdmin` needs, built from the same bytes as the settings.
fn custody(entries: &[(&str, [u8; 32])], active: &str) -> Arc<dyn MasterKeyCustody> {
    Arc::new(
        EnvironmentMasterKeyCustody::new(
            entries
                .iter()
                .map(|(id, bytes)| ((*id).to_string(), Zeroizing::new(*bytes)))
                .collect(),
            active,
        )
        .expect("environment custody"),
    )
}

/// What a memory row actually holds, read back from PostgreSQL rather than from the API.
#[derive(Debug)]
struct StoredMemory {
    public_id: String,
    content_plain: Option<String>,
    content_encrypted: Option<Vec<u8>>,
    content_hash: String,
}

struct Case {
    fixture: LifecycleFixture,
    caller: Actor,
}

impl Case {
    async fn new() -> Option<Self> {
        Self::with_master_keys(&[("dev", MASTER_A)], "dev").await
    }

    /// A fixture whose content keyring is wrapped by a **named** master key rather than the
    /// dev sentinel, so a rotation between two of them is expressible.
    async fn with_master_keys(entries: &[(&str, [u8; 32])], active: &str) -> Option<Self> {
        let keys = keys_setting(entries);
        let active = active.to_string();
        let fixture = LifecycleFixture::with_settings(move |settings: &mut Settings| {
            settings.content_encryption.keys = keys;
            settings.content_encryption.active_key_id = active;
            settings.content_encryption.allow_insecure_dev_key = false;
        })
        .await?;
        fixture.enable_manual_memory().await;
        let mut caller = fixture.caller_actor(Some("tenant-140"), Some("user-140"));
        caller.scopes.push("moira:memories:create".to_string());
        caller.scopes.push("moira:memories:read".to_string());
        caller.scopes.push("moira:memories:write".to_string());
        caller
            .scopes
            .push("moira:conversation-policies:write".to_string());
        caller
            .scopes
            .push("moira:conversation-policies:read".to_string());
        Some(Self { fixture, caller })
    }

    fn service(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// Sets the policy through the **application service**, the way an operator does — so a
    /// refusal at policy-write time would surface here rather than being routed around.
    async fn set_policy(&self, persistence: ConversationContentPersistence) {
        self.service()
            .put_conversation_policy(
                &self.fixture.actor,
                &request_context(),
                self.fixture.application_id,
                ConversationPolicyPutRequest {
                    conversation_content_persistence: Some(persistence),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{persistence:?} must be accepted: {error:?}"));
    }

    async fn create(&self, content: &str) -> MemoryRecord {
        tokio::time::timeout(
            WAIT,
            self.service().create_memory(
                &self.caller,
                &request_context(),
                MemoryCreateRequest {
                    memory_type: MemoryType::Fact,
                    content: content.to_string(),
                    importance: None,
                    confidence: None,
                    valid_until: None,
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create memory timed out")
        .expect("create memory")
    }

    async fn patch(&self, public_id: &str, content: &str) -> MemoryRecord {
        tokio::time::timeout(
            WAIT,
            self.service().patch_memory(
                &self.caller,
                &request_context(),
                public_id,
                MemoryPatchRequest {
                    content: Some(content.to_string()),
                    ..MemoryPatchRequest::default()
                },
            ),
        )
        .await
        .expect("patch memory timed out")
        .expect("patch memory")
    }

    async fn read_row(&self, public_id: &str) -> StoredMemory {
        let row = sqlx::query(
            "select public_id, content_plain, content_encrypted, content_hash \
             from memory_records where public_id = $1",
        )
        .bind(public_id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("read the memory row");
        StoredMemory {
            public_id: row.try_get("public_id").expect("public_id"),
            content_plain: row.try_get("content_plain").expect("content_plain"),
            content_encrypted: row.try_get("content_encrypted").expect("content_encrypted"),
            content_hash: row.try_get("content_hash").expect("content_hash"),
        }
    }

    /// The lookup `find_memory_by_content_hash` performs: exact match, scoped to one
    /// application. Written as SQL for the same reason
    /// `tests/memory_content_hash_rotation.rs` does — the repository function is `pub(crate)`,
    /// and the predicate is the thing under test.
    async fn memories_matching(&self, hash: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "select public_id from memory_records \
             where application_id = $1 and content_hash = $2 and deleted_at is null \
             order by public_id",
        )
        .bind(self.fixture.application_id)
        .bind(hash)
        .fetch_all(&self.fixture.pool)
        .await
        .expect("dedupe lookup")
    }

    async fn every_stored_hash(&self) -> Vec<(String, String)> {
        sqlx::query(
            "select public_id, content_hash from memory_records \
             where application_id = $1 order by public_id",
        )
        .bind(self.fixture.application_id)
        .fetch_all(&self.fixture.pool)
        .await
        .expect("read every hash")
        .into_iter()
        .map(|row| {
            (
                row.try_get("public_id").expect("public_id"),
                row.try_get("content_hash").expect("content_hash"),
            )
        })
        .collect()
    }

    async fn memory_count(&self) -> i64 {
        sqlx::query_scalar("select count(*) from memory_records where application_id = $1")
            .bind(self.fixture.application_id)
            .fetch_one(&self.fixture.pool)
            .await
            .expect("count memories")
    }

    /// `content_hash` straight out of the column, with no record type, no `StoredMemory` and no
    /// repository function in the way.
    ///
    /// `read_row` would do, and that is exactly why this exists: the property #168 closes is
    /// about **what is on disk**, so the assertion that proves it must not be able to be satisfied
    /// by a wrapper that normalises, prefixes or re-derives anything on the way out.
    async fn raw_stored_hash(&self, public_id: &str) -> String {
        sqlx::query_scalar::<_, String>(
            "select content_hash from memory_records where public_id = $1",
        )
        .bind(public_id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("read content_hash as a raw column value")
    }

    /// How many rows of this application carry exactly `hash`. Used to ask "does the unkeyed
    /// digest of this body appear **anywhere**", which is a stronger question than "is this row's
    /// hash unkeyed".
    async fn rows_with_hash(&self, hash: &str) -> i64 {
        sqlx::query_scalar(
            "select count(*) from memory_records where application_id = $1 and content_hash = $2",
        )
        .bind(self.fixture.application_id)
        .bind(hash)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("count rows by hash")
    }

    /// A row in the form every memory carried before #168 — body in the clear, digest unkeyed —
    /// inserted straight into the table because no writer in this build can produce one any more.
    ///
    /// That is the point. The cross-era assertions need a genuine pre-change value, and a value
    /// produced by the current writer would prove nothing about the corpus already on disk.
    async fn seed_pre_change_row(&self, content: &str) -> String {
        let id = Uuid::now_v7();
        let public_id = format!("mem_{id}");
        sqlx::query(
            "insert into memory_records \
             (id, public_id, application_id, memory_scope, memory_type, content_plain, \
              content_hash) \
             values ($1, $2, $3, 'application', 'fact', $4, $5)",
        )
        .bind(id)
        .bind(&public_id)
        .bind(self.fixture.application_id)
        .bind(content)
        .bind(request_hash(content.as_bytes()))
        .execute(&self.fixture.pool)
        .await
        .expect("seed a pre-#168 memory row");
        public_id
    }
}

/// Every value of the policy, written once so the tests that walk it cannot drift apart and
/// quietly end up covering three. A fifth value has to be added here by hand — `ContentWrite`'s
/// exhaustive matches are what make it a compile error in `src`, not this array.
const EVERY_PERSISTENCE: [ConversationContentPersistence; 4] = [
    ConversationContentPersistence::None,
    ConversationContentPersistence::MetadataOnly,
    ConversationContentPersistence::PlainContent,
    ConversationContentPersistence::EncryptedContent,
];

// ---------------------------------------------------------------------------
// Storage form
// ---------------------------------------------------------------------------

/// The premise. Every "no plaintext anywhere" assertion below passes trivially against a build
/// that stores nothing at all, so the file needs one case proving the writer works — and, since
/// #168, one proving the digest is keyed even where the body is *not* sealed.
#[tokio::test]
async fn a_plain_content_memory_stores_its_body_and_still_keys_the_digest() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;

    let record = case.create(BODY).await;
    let stored = case.read_row(&record.id).await;

    assert_eq!(stored.content_plain.as_deref(), Some(BODY));
    assert_eq!(
        stored.content_encrypted, None,
        "plain_content must not also write the encrypted column; migration 0027's CHECK forbids \
         holding both"
    );
    assert!(
        stored.content_hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
        "since #168 the digest is keyed under every policy value, `plain_content` included — one \
         rule with no branch, because a three-of-four rule leaves the arm a later edit widens \
         back into an oracle. Got {}",
        stored.content_hash
    );
    assert_ne!(
        stored.content_hash,
        request_hash(BODY.as_bytes()),
        "the stored digest is the unkeyed content address of its own body"
    );
}

/// The "forgot to call the cipher" catcher, on the **raw column bytes**.
///
/// Read a second time through `query_scalar::<_, Vec<u8>>` with no application type in the way,
/// so the assertion cannot be satisfied by anything the sealer handed back.
#[tokio::test]
async fn an_encrypted_memory_leaves_no_plaintext_in_the_raw_column() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let record = case.create(BODY).await;
    assert_eq!(
        record.content.as_deref(),
        Some(BODY),
        "the record handed back to the caller must carry the body that was written; a record \
         saying `null` beside a sealed row would be finding F32's dishonesty inverted"
    );
    let stored = case.read_row(&record.id).await;
    assert_eq!(
        stored.content_plain, None,
        "encrypted_content stored the memory in the clear — the value promises encryption"
    );
    let sealed = stored
        .content_encrypted
        .as_ref()
        .expect("encrypted_content must write the encrypted column");

    let raw: Vec<u8> =
        sqlx::query_scalar("select content_encrypted from memory_records where public_id = $1")
            .bind(&stored.public_id)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("read content_encrypted as raw bytes");
    assert_eq!(&raw, sealed);

    assert!(
        !contains_subslice(&raw, BODY.as_bytes()),
        "the memory body appears verbatim inside content_encrypted — the cipher was not called, \
         or its output was discarded. Raw column bytes: {} bytes, first 64: {:?}",
        raw.len(),
        &raw[..raw.len().min(64)]
    );
    // And the bytes really are a v1 envelope, not a wrapper that happens not to contain the
    // literal body.
    assert_eq!(&raw[..4], &ENVELOPE_MAGIC);
    assert_eq!(
        raw.len(),
        ENVELOPE_HEADER_LEN + BODY.len() + 16,
        "a sealed body is the header, the ciphertext and a 16-byte GCM tag and nothing else"
    );
}

// ---------------------------------------------------------------------------
// The content_hash oracle
// ---------------------------------------------------------------------------

/// **The case this whole PR is for.**
///
/// Every other assertion in this file passes against a build that seals `content_plain` and
/// leaves `request_hash(body)` in `content_hash` — a row whose encryption a wordlist undoes.
/// This one does not.
#[tokio::test]
async fn a_sealed_memory_hash_is_keyed_and_is_not_the_digest_of_its_own_plaintext() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let record = case.create(BODY).await;
    let stored = case.read_row(&record.id).await;

    assert_ne!(
        stored.content_hash,
        request_hash(BODY.as_bytes()),
        "the sealed row carries the UNKEYED digest of its own plaintext. The body is short and \
         guessable, so this single value undoes the encryption: a database dump plus a wordlist \
         recovers it with no key at all"
    );
    assert!(
        stored.content_hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
        "a sealed memory's hash must carry the `d1:` marker so a dedupe reader can tell the two \
         eras apart, got {}",
        stored.content_hash
    );

    // The shape is stored, so it is pinned: 3 characters of prefix plus 43 of unpadded base64url
    // over a 32-byte HMAC. Changing any of it orphans every hash already written.
    let digest = stored
        .content_hash
        .strip_prefix(MEMORY_DEDUPE_HASH_PREFIX)
        .expect("prefix");
    assert_eq!(
        digest.len(),
        43,
        "not a 32-byte digest in base64url: {digest}"
    );
    assert!(
        !digest.contains('='),
        "base64url here is unpadded: {digest}"
    );
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "outside the base64url alphabet: {digest}"
    );
    assert_eq!(
        stored.content_hash.matches(':').count(),
        1,
        "exactly one `:`, the marker's own — a second would make the prefix ambiguous"
    );
    assert!(
        stored.content_hash.len() <= 128,
        "content_hash must fit varchar(128)"
    );

    // The keying is real rather than a relabelling: the same body under a *different* keyring
    // hashes differently. Without this, `"d1:" + base64url(sha256(body))` would pass everything
    // above while remaining exactly the oracle the prefix claims to have closed.
    let Some(other) = Case::new().await else {
        return;
    };
    other
        .set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let elsewhere = other.create(BODY).await;
    assert_ne!(
        other.read_row(&elsewhere.id).await.content_hash,
        stored.content_hash,
        "the same body hashed identically under two independently minted dedupe keys, so the \
         digest is not actually keyed"
    );
}

/// Dedupe still works **within** the sealed era: the same body twice is the same value.
///
/// Paired with the cross-era case below, this is what makes the `d1:` form a usable dedupe key
/// rather than merely an opaque one — a digest that were somehow salted per row would satisfy
/// every shape assertion above and silently disable dedupe entirely.
#[tokio::test]
async fn dedupe_still_matches_between_two_sealed_memories_with_the_same_body() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let first = case.create(BODY).await;
    let second = case.create(BODY).await;
    let first_hash = case.read_row(&first.id).await.content_hash;
    let second_hash = case.read_row(&second.id).await.content_hash;

    assert_eq!(
        first_hash, second_hash,
        "two sealed memories with identical bodies must produce identical hashes, or exact \
         dedupe cannot work at all under an encrypted policy"
    );
    let matched = case.memories_matching(&first_hash).await;
    assert_eq!(
        matched.len(),
        2,
        "the application-scoped exact-match lookup must find both rows; it found {matched:?}"
    );
    assert!(matched.contains(&first.id) && matched.contains(&second.id));

    // And a *different* body does not match, so the equality above is content-dependent rather
    // than a constant.
    let unrelated = case.create(SECOND_BODY).await;
    assert_ne!(case.read_row(&unrelated.id).await.content_hash, first_hash);
}

/// **Both directions, under every policy value.** A pre-#168 unkeyed hash must never find a row
/// this build wrote, and a hash this build wrote must never find a pre-#168 row — whichever
/// policy that row was written under.
///
/// One direction is not enough: a build that had quietly gone *back* to the unkeyed address for
/// some arm would pass "the old hash finds no new row" for the arms it did not touch while
/// falsely matching on the one it did. Walking all four is what turns this from a claim about
/// `encrypted_content` into the claim #168 actually makes.
///
/// The old-era row is seeded straight into the table because **no writer in this build can
/// produce one**, which is the whole point: the corpus on a real deployment predates the change
/// and nothing rewrites it.
#[tokio::test]
async fn a_pre_change_unkeyed_hash_never_matches_a_keyed_hash_under_any_policy() {
    let Some(case) = Case::new().await else {
        return;
    };

    for persistence in EVERY_PERSISTENCE {
        // A distinct body per policy, so a match found below can only have come from this
        // iteration's two rows and not from a neighbour's.
        let body = format!("{BODY}-cross-era-{persistence:?}");
        let unkeyed = request_hash(body.as_bytes());
        let legacy = case.seed_pre_change_row(&body).await;

        case.set_policy(persistence).await;
        let fresh = case.create(&body).await;
        let keyed = case.raw_stored_hash(&fresh.id).await;

        assert_ne!(
            keyed, unkeyed,
            "{persistence:?}: this build wrote the unkeyed content address of the body — the \
             oracle #168 closed, reopened for this policy value"
        );
        assert!(
            keyed.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
            "{persistence:?}: a keyed digest must carry the `d1:` marker so a reader can tell the \
             eras apart, got {keyed}"
        );
        assert!(
            !unkeyed.contains(':'),
            "the `d1:` argument rests on a content address never containing `:`, and this is \
             where that is checked against a real stored value: {unkeyed}"
        );

        // Direction one: the pre-change value finds the pre-change row and nothing else.
        assert_eq!(
            case.memories_matching(&unkeyed).await,
            vec![legacy.clone()],
            "{persistence:?}: a pre-#168 hash matched a row written after the change"
        );
        // Direction two: this build's value finds this build's row and nothing else.
        assert_eq!(
            case.memories_matching(&keyed).await,
            vec![fresh.id.clone()],
            "{persistence:?}: a keyed hash matched a pre-#168 row"
        );
    }
}

/// **The property issue #168 exists to establish: no unkeyed digest of memory content survives
/// anywhere, under any policy.**
///
/// Every other case in this file could pass while one arm still wrote `request_hash`. This one
/// asks the database directly — for each of the four values, is the body's unkeyed content
/// address present in *any* row of this application — and asserts against the **raw column**
/// through `raw_stored_hash`, not through `MemoryRecord` or any other wrapper that could
/// normalise the answer.
///
/// It is deliberately the weakest-looking and hardest-to-fake assertion in the file: a build
/// that keyed three arms and left one is red here and green almost everywhere else.
#[tokio::test]
async fn no_memory_row_carries_an_unkeyed_digest_under_any_policy() {
    let Some(case) = Case::new().await else {
        return;
    };

    for persistence in EVERY_PERSISTENCE {
        let body = format!("{BODY}-no-oracle-{persistence:?}");
        case.set_policy(persistence).await;
        let record = case.create(&body).await;

        let stored = case.raw_stored_hash(&record.id).await;
        assert!(
            stored.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
            "{persistence:?}: the raw stored digest is not keyed, got {stored}"
        );
        assert_ne!(
            stored,
            request_hash(body.as_bytes()),
            "{persistence:?}: the raw stored digest is the unkeyed content address of the body. \
             A memory body is short and guessable, so this single value is a dictionary attack \
             on content the row may not even hold"
        );
        assert_eq!(
            case.rows_with_hash(&request_hash(body.as_bytes())).await,
            0,
            "{persistence:?}: the unkeyed content address of this body appears in some row of \
             this application. It does not matter which row wrote it — its presence anywhere is \
             the oracle"
        );

        // And under the two values that store nothing, nothing is stored: the digest is the only
        // thing left, which is exactly why keying it was the whole issue.
        if !matches!(
            persistence,
            ConversationContentPersistence::PlainContent
                | ConversationContentPersistence::EncryptedContent
        ) {
            let row = case.read_row(&record.id).await;
            assert_eq!(row.content_plain, None, "{persistence:?} stored a body");
            assert_eq!(
                row.content_encrypted, None,
                "{persistence:?} stored a sealed body"
            );
        }
    }
}

/// Dedupe under `none` and `metadata_only` — the function #168 put a key dependency on.
///
/// If keying those arms had broken dedupe for them, the honest resolution would have been the
/// issue's option 2 (drop the digest entirely there) rather than a key dependency bought for
/// nothing. So this is not a nice-to-have alongside the security property; it is the thing that
/// makes the security property's price worth paying, and it asserts the resubmission actually
/// matches rather than merely hashing to something opaque.
#[tokio::test]
async fn dedupe_still_matches_under_none_and_metadata_only() {
    let Some(case) = Case::new().await else {
        return;
    };

    for persistence in [
        ConversationContentPersistence::None,
        ConversationContentPersistence::MetadataOnly,
    ] {
        let body = format!("{BODY}-bodyless-dedupe-{persistence:?}");
        case.set_policy(persistence).await;

        let first = case.create(&body).await;
        let second = case.create(&body).await;
        let first_hash = case.raw_stored_hash(&first.id).await;

        assert_eq!(
            first_hash,
            case.raw_stored_hash(&second.id).await,
            "{persistence:?}: the same body written twice produced two different digests, so \
             exact dedupe cannot work at all for an application that stores no bodies"
        );
        let matched = case.memories_matching(&first_hash).await;
        assert_eq!(
            matched.len(),
            2,
            "{persistence:?}: the application-scoped exact-match lookup must find both rows; it \
             found {matched:?}"
        );
        assert!(matched.contains(&first.id) && matched.contains(&second.id));

        // The stored row really is bodyless, so this is dedupe over content the row does not
        // hold — the shape the issue called close to a contradiction, kept working on purpose.
        let row = case.read_row(&first.id).await;
        assert_eq!(row.content_plain, None);
        assert_eq!(row.content_encrypted, None);

        // And a different body still does not match, so the equality above is content-dependent
        // rather than a constant every bodyless row shares.
        let other = case.create(&format!("{body}-different")).await;
        assert_ne!(case.raw_stored_hash(&other.id).await, first_hash);
    }
}

// ---------------------------------------------------------------------------
// The rotation property — the reason the key lives in the keyring
// ---------------------------------------------------------------------------

/// **Rotate the master key; every `content_hash` must be byte-identical afterwards.**
///
/// This is the claim that dissolves F14. A *pepper* rotation permanently orphaned every stored
/// hash on a table with no retention, which is why `0021` moved this column to an unkeyed
/// digest in the first place. A keyring row behaves differently in kind: the rotation re-wraps
/// the envelope and the 32 bytes inside it never change.
///
/// The rotation performed here is the real operator procedure, not a simulation:
///
/// 1. `KeyringAdmin::rewrap` under a custody holding **both** master keys, targeting the new one;
/// 2. a brand-new `AppState` configured with **only** the new master key — which is what makes
///    step 1 load-bearing. A process that still held the old key would read the keyring whether
///    or not the rewrap did anything.
///
/// A test that skipped step 2 would pass against a `rewrap` that was a no-op.
///
/// **Widened for #168.** The #140 version wrote sealed rows plus one plaintext row, and the
/// plaintext row's hash was unkeyed — so it proved the rotation property for `encrypted_content`
/// and merely proved the unkeyed arm was untouched for the rest. Now every policy value writes a
/// keyed hash, so every policy value has something to lose in a rotation, and this walks all four
/// rather than assuming the sealed case generalises.
#[tokio::test]
async fn memory_dedupe_hashes_survive_a_master_key_rotation() {
    let both = [("m-old", MASTER_A), ("m-new", MASTER_B)];
    let Some(case) = Case::with_master_keys(&[("m-old", MASTER_A)], "m-old").await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let sealed = case.create(BODY).await;
    let second = case.create(SECOND_BODY).await;
    // One row per remaining policy value, so a rotation that moved the digest for any of them
    // fails here and names which. Under `none` and `metadata_only` the digest is the *only*
    // content-derived thing the row holds, which makes orphaning it the whole loss rather than
    // part of one.
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    let plain = case.create("a memory stored in the clear").await;
    case.set_policy(ConversationContentPersistence::None).await;
    let bodyless = case.create("a memory whose body is not stored").await;
    case.set_policy(ConversationContentPersistence::MetadataOnly)
        .await;
    let metadata_only = case.create("a memory kept as metadata only").await;

    let before = case.every_stored_hash().await;
    assert_eq!(
        before.len(),
        5,
        "premise: one row per policy value plus a second sealed one, to compare"
    );
    assert!(
        before
            .iter()
            .all(|(_, hash)| hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX)),
        "premise: since #168 every one of these is keyed, so every one of them has something a \
         rotation could break. If any is unkeyed this test is rotating past the arm it is \
         supposed to cover: {before:?}"
    );

    // --- the rotation -------------------------------------------------------------------
    let report = KeyringAdmin::new(case.fixture.pool.clone(), custody(&both, "m-old"))
        .rewrap("m-new")
        .await
        .expect("rewrap onto the new master key");
    assert!(
        report.rewrapped.len() >= 2,
        "the rewrap must have moved the content key AND the memory_dedupe key; it moved {:?}",
        report.rewrapped
    );
    assert!(report.left_alone.is_empty());

    // A process that holds **only** the new master key. If the dedupe key had not been
    // rewrapped, this would refuse to boot — which is itself half the property.
    let mut settings = (*case.fixture.state.settings).clone();
    settings.content_encryption.keys = keys_setting(&[("m-new", MASTER_B)]);
    settings.content_encryption.active_key_id = "m-new".to_string();
    let rotated = AppState::new(settings, Some(case.fixture.pool.clone()))
        .await
        .expect("a process holding only the new master key must boot");

    // --- the assertion ------------------------------------------------------------------
    let after = case.every_stored_hash().await;
    assert_eq!(
        after, before,
        "a master-key rotation changed a stored content_hash. Every dedupe lookup written \
         before the rotation now misses, permanently and silently, on a table with no \
         retention — which is finding F14, re-created by the very mechanism chosen to avoid it"
    );

    // The rotated process hashes the same body to the same value, so *new* writes still dedupe
    // against the pre-rotation corpus. Byte-identical stored values alone would not prove that:
    // a process whose dedupe key had changed would leave the old rows untouched and simply stop
    // matching them.
    let rotated_service = ConversationService::new(&rotated).expect("rotated service");
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let repeat = tokio::time::timeout(
        WAIT,
        rotated_service.create_memory(
            &case.caller,
            &request_context(),
            MemoryCreateRequest {
                memory_type: MemoryType::Fact,
                content: BODY.to_string(),
                importance: None,
                confidence: None,
                valid_until: None,
                metadata: json!({}),
            },
        ),
    )
    .await
    .expect("create memory timed out")
    .expect("create memory on the rotated process");

    let sealed_hash = case.read_row(&sealed.id).await.content_hash;
    assert_eq!(
        case.read_row(&repeat.id).await.content_hash,
        sealed_hash,
        "the rotated process hashed the same body to a different value, so cross-rotation \
         dedupe is broken even though the stored bytes did not move"
    );
    let matched = case.memories_matching(&sealed_hash).await;
    assert_eq!(matched.len(), 2, "the lookup found {matched:?}");
    assert!(matched.contains(&sealed.id) && matched.contains(&repeat.id));

    // And the pre-rotation ciphertext still opens under the new master key alone.
    let reread = tokio::time::timeout(WAIT, rotated_service.get_memory(&case.caller, &sealed.id))
        .await
        .expect("get memory timed out")
        .expect("a pre-rotation sealed memory must still read");
    assert_eq!(reread.content.as_deref(), Some(BODY));

    // The other rows are named individually so a failure says which policy value moved, rather
    // than only that the vectors differ.
    for (label, id) in [
        ("second sealed", &second.id),
        ("plain_content", &plain.id),
        ("none", &bodyless.id),
        ("metadata_only", &metadata_only.id),
    ] {
        assert_eq!(
            case.raw_stored_hash(id).await,
            before
                .iter()
                .find(|(row_id, _)| row_id == id)
                .unwrap_or_else(|| panic!("{label} row"))
                .1,
            "the {label} row's content_hash moved across the master-key rotation"
        );
    }
}

// ---------------------------------------------------------------------------
// patch_memory
// ---------------------------------------------------------------------------

/// A patch must reseal the body **and** rewrite the hash into the matching form, in both
/// directions — including clearing the column the previous form used.
///
/// The clearing half is not cosmetic. `content_plain = coalesce($2, content_plain)` — the old
/// statement — would leave the previous plaintext standing beside the new ciphertext, which
/// migration 0027's CHECK refuses outright and which on a row predating that constraint would
/// be a sealed body serving its own plaintext.
#[tokio::test]
async fn patch_memory_reseals_and_rewrites_the_hash_in_both_directions() {
    let Some(case) = Case::new().await else {
        return;
    };

    // plain → encrypted
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    let record = case.create(BODY).await;
    let before = case.read_row(&record.id).await;
    assert_eq!(before.content_plain.as_deref(), Some(BODY));
    assert!(
        before.content_hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
        "since #168 even a plain_content row is keyed, got {}",
        before.content_hash
    );

    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let patched = case.patch(&record.id, SECOND_BODY).await;
    assert_eq!(
        patched.content.as_deref(),
        Some(SECOND_BODY),
        "the patched record must carry the new body"
    );
    let after = case.read_row(&record.id).await;
    assert_eq!(
        after.content_plain, None,
        "the pre-patch plaintext survived beside the new ciphertext. A `coalesce` cannot clear \
         the other column, and a row holding both is exactly what migration 0027 forbids"
    );
    let raw: Vec<u8> =
        sqlx::query_scalar("select content_encrypted from memory_records where public_id = $1")
            .bind(&record.id)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("read the patched ciphertext");
    assert_eq!(&raw[..4], &ENVELOPE_MAGIC);
    assert!(
        !contains_subslice(&raw, SECOND_BODY.as_bytes())
            && !contains_subslice(&raw, BODY.as_bytes()),
        "a patched body appears verbatim in the sealed column"
    );
    assert!(
        after.content_hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX),
        "a patch that seals the body must also key the digest, got {}",
        after.content_hash
    );
    assert_ne!(after.content_hash, request_hash(SECOND_BODY.as_bytes()));
    assert_ne!(
        after.content_hash, before.content_hash,
        "patching the content must re-address the row"
    );

    // encrypted → plain. The reverse clearing, which the same `case when` handles and which no
    // amount of testing the forward direction reaches.
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    case.patch(&record.id, BODY).await;
    let back = case.read_row(&record.id).await;
    assert_eq!(back.content_plain.as_deref(), Some(BODY));
    assert_eq!(
        back.content_encrypted, None,
        "patching back to plaintext left the old ciphertext in place"
    );
    assert_ne!(
        back.content_hash,
        request_hash(BODY.as_bytes()),
        "a patch back to plaintext dropped to the unkeyed content address. The body being in the \
         clear again does not make its digest safe to hand out — since #168 there is no policy \
         value that writes one"
    );
    assert_eq!(
        back.content_hash, before.content_hash,
        "the round trip must land on exactly the value the row started with, or a patched row \
         stops deduping against every other row holding the same body"
    );
}

// ---------------------------------------------------------------------------
// Refusal
// ---------------------------------------------------------------------------

/// No usable key means **no row**, not a row with an unkeyed digest — under every storage form,
/// including the one that stores no body at all.
///
/// The count is the assertion, not the error: an error *plus* a row would be strictly worse than
/// either alone, and only counting can tell them apart.
///
/// **The `Omitted` case is #168's cost, made visible as behaviour.** Before #168 that write
/// succeeded on a keyring-less process, because its digest needed no key. It now refuses, which
/// is the honest consequence of the row's digest depending on one — and the reason
/// `docs/security.md` states the key-loss dependency for `none` and `metadata_only` rather than
/// leaving it to be discovered here.
#[tokio::test]
async fn a_memory_write_with_no_keyring_is_refused_under_every_policy() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    // One successful write first, so the count below is a *change* rather than a comparison
    // against zero.
    case.create(BODY).await;
    let before = case.memory_count().await;
    assert_eq!(before, 1);

    // Every storage form a policy value can produce. `Plain` is included for the same reason the
    // digest is keyed for it: leaving one arm untested is how one arm gets a fallback.
    for (label, stored) in [
        (
            "Encrypt (encrypted_content)",
            ContentWrite::Encrypt(Zeroizing::new(SECOND_BODY.to_string())),
        ),
        (
            "Plain (plain_content)",
            ContentWrite::Plain(SECOND_BODY.to_string()),
        ),
        ("Omitted (none / metadata_only)", ContentWrite::Omitted),
    ] {
        // A repository with no content keyring is a real constructor arm, not a test hook:
        // `AppState` passes `None` whenever there is no database.
        let repo = PgConversationRepository::new(case.fixture.pool.clone(), None);
        let id = Uuid::now_v7();
        let public_id = format!("mem_{id}");
        let request = MemoryCreateRequest {
            memory_type: MemoryType::Fact,
            content: SECOND_BODY.to_string(),
            importance: None,
            confidence: None,
            valid_until: None,
            metadata: json!({}),
        };
        let outcome = repo
            .create_memory(&MemoryInsert {
                id,
                public_id: &public_id,
                application_id: case.fixture.application_id,
                external_tenant_id: Some("tenant-140"),
                external_user_id: Some("user-140"),
                scope: moira::domain::MemoryScope::UserApplication,
                request: &request,
                content: stored,
            })
            .await;
        let error = match outcome {
            Ok(record) => panic!(
                "{label}: a memory write with no usable dedupe key must be refused, but it wrote \
                 {}",
                record.id
            ),
            Err(error) => error,
        };

        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE, "{label}");
        assert_eq!(
            error.error_response(None).error.code,
            "content_key_unavailable",
            "{label}"
        );
        assert_eq!(
            case.memory_count().await,
            before,
            "{label}: the refused write inserted a row anyway"
        );
        let leaked: i64 =
            sqlx::query_scalar("select count(*) from memory_records where content_plain = $1")
                .bind(SECOND_BODY)
                .fetch_one(&case.fixture.pool)
                .await
                .expect("count leaked plaintext");
        assert_eq!(
            leaked, 0,
            "{label}: the refused body was written as plaintext"
        );
        let addressed: i64 =
            sqlx::query_scalar("select count(*) from memory_records where content_hash = $1")
                .bind(request_hash(SECOND_BODY.as_bytes()))
                .fetch_one(&case.fixture.pool)
                .await
                .expect("count leaked digests");
        assert_eq!(
            addressed, 0,
            "{label}: the refused write left an unkeyed digest of its body behind — the body \
             itself was not stored, but the digest alone is the guessing oracle this design closes"
        );
    }
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Every read path opens a sealed memory: the single-row read, the list, and the record the
/// writer returns. A read that silently rendered `content: null` would look like a memory the
/// caller never stored.
#[tokio::test]
async fn every_memory_read_path_opens_a_sealed_body() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let first = case.create(BODY).await;
    let second = case.create(SECOND_BODY).await;

    let fetched = tokio::time::timeout(WAIT, case.service().get_memory(&case.caller, &first.id))
        .await
        .expect("get memory timed out")
        .expect("get memory");
    assert_eq!(fetched.content.as_deref(), Some(BODY));

    let listed = tokio::time::timeout(
        WAIT,
        case.service()
            .list_memories(&case.caller, &moira::domain::MemoryQuery::default()),
    )
    .await
    .expect("list memories timed out")
    .expect("list memories");
    let mut bodies: Vec<Option<String>> = listed
        .data
        .into_iter()
        .map(|record| record.content)
        .collect();
    bodies.sort();
    let mut expected = vec![Some(BODY.to_string()), Some(SECOND_BODY.to_string())];
    expected.sort();
    assert_eq!(
        bodies, expected,
        "the list read did not open both sealed bodies"
    );
    assert_eq!(second.content.as_deref(), Some(SECOND_BODY));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
