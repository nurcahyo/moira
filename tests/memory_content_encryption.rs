//! `memory_records` under `conversation_content_persistence = 'encrypted_content'`, and the
//! `content_hash` oracle that sealing the body would otherwise leave wide open — issue #140.
//!
//! # The hole this file exists to prove closed
//!
//! Since migration `0021`, `memory_records.content_hash` has been an **unkeyed SHA-256 of the
//! plaintext**. Memory bodies are short, low-entropy and highly guessable — "user prefers dark
//! mode", "user's timezone is Asia/Jakarta". Sealing `content_plain` while leaving an unkeyed
//! digest of that same plaintext **in the same row** does not half-encrypt the memory; it does
//! not encrypt it at all. A database dump plus a wordlist recovers every body without touching a
//! key.
//!
//! So `an_encrypted_memory_leaves_no_plaintext_in_the_raw_column` is necessary but nowhere near
//! sufficient here, and that is the difference between this file and
//! `tests/conversation_content_encryption.rs`. The load-bearing case is
//! `a_sealed_memory_hash_is_keyed_and_is_not_the_digest_of_its_own_plaintext`.
//!
//! # The two claims the design rests on, tested rather than asserted in prose
//!
//! **`d1:` is unambiguous.** A `request_hash` value is unpadded base64url over a fixed 32-byte
//! digest, so it can never contain `:` — migration `0021`'s own rule. The two forms therefore
//! **miss, never falsely match**, and
//! `a_plaintext_era_hash_and_a_sealed_era_hash_never_match_in_either_direction` drives both
//! directions rather than reasoning about one.
//!
//! **The dedupe key rides master-key rotation.** It is a `content_data_keys` row wrapped by the
//! master key, so a rotation re-wraps the envelope and the 32 bytes inside never change.
//! `memory_dedupe_hashes_survive_a_master_key_rotation` performs a real rotation — rewrap, then
//! a fresh `AppState` holding **only** the new master key — and asserts every stored
//! `content_hash` is byte-identical afterwards. That is what dissolves F14's objection to a
//! keyed hash on a table with no retention, and a test that never rotates would prove none of
//! it.

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
}

// ---------------------------------------------------------------------------
// Storage form
// ---------------------------------------------------------------------------

/// The premise. Every "no plaintext anywhere" assertion below passes trivially against a build
/// that stores nothing at all, so the file needs one case proving the writer works — and one
/// proving the *unsealed* digest is still the content address `0021` established, because
/// changing that would orphan every existing row.
#[tokio::test]
async fn plain_content_memories_keep_the_unkeyed_content_address() {
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
    assert_eq!(
        stored.content_hash,
        request_hash(BODY.as_bytes()),
        "an unsealed memory must keep the unkeyed content address; moving it would orphan every \
         row already stored on a table that has no retention — finding F14 exactly"
    );
    assert!(
        !stored.content_hash.contains(':'),
        "a content address is base64url and can never contain `:`; the whole `d1:` argument \
         rests on that, and this is where it is checked against a real stored value: {}",
        stored.content_hash
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

/// **Both directions.** A plaintext-era hash must never find a sealed row, and a sealed-era hash
/// must never find a plaintext row.
///
/// One direction is not enough: a build that keyed *both* arms would pass "plaintext hash finds
/// no sealed row" while breaking every stored value on the unsealed corpus.
#[tokio::test]
async fn a_plaintext_era_hash_and_a_sealed_era_hash_never_match_in_either_direction() {
    let Some(case) = Case::new().await else {
        return;
    };

    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    let plain = case.create(BODY).await;
    let plain_hash = case.read_row(&plain.id).await.content_hash;

    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let sealed = case.create(BODY).await;
    let sealed_hash = case.read_row(&sealed.id).await.content_hash;

    assert_ne!(
        plain_hash, sealed_hash,
        "the two eras produced the same value for the same body, so one of them is not doing \
         what it claims"
    );

    // Direction one: the plaintext-era value finds the plaintext row and nothing else.
    assert_eq!(
        case.memories_matching(&plain_hash).await,
        vec![plain.id.clone()],
        "a plaintext-era hash matched a sealed row"
    );
    // Direction two: the sealed-era value finds the sealed row and nothing else.
    assert_eq!(
        case.memories_matching(&sealed_hash).await,
        vec![sealed.id.clone()],
        "a sealed-era hash matched a plaintext row"
    );

    // A **miss, never a false match** — which is only unambiguous because `:` cannot occur in a
    // content address. Checked against the real stored values rather than restated.
    assert!(sealed_hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX));
    assert!(!plain_hash.contains(':'));
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
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    // A plaintext-era row too, so the assertion covers *every* hash in the table rather than
    // only the keyed ones — a rotation that broke the unkeyed arm would be just as fatal.
    let plain = case.create("a memory stored in the clear").await;
    let before = case.every_stored_hash().await;
    assert_eq!(before.len(), 3, "premise: three rows to compare");
    assert!(
        before
            .iter()
            .any(|(_, hash)| hash.starts_with(MEMORY_DEDUPE_HASH_PREFIX)),
        "premise: at least one keyed hash, or this test rotates past nothing"
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

    // The other two rows are named so a failure says which one moved.
    assert_eq!(
        case.read_row(&second.id).await.content_hash,
        before
            .iter()
            .find(|(id, _)| id == &second.id)
            .expect("second row")
            .1
    );
    assert_eq!(
        case.read_row(&plain.id).await.content_hash,
        before
            .iter()
            .find(|(id, _)| id == &plain.id)
            .expect("plain row")
            .1
    );
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
    assert!(!before.content_hash.contains(':'));

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
    assert_eq!(
        back.content_hash,
        request_hash(BODY.as_bytes()),
        "a patch back to plaintext must restore the unkeyed content address, or the row stops \
         deduping against every other plaintext row holding the same body"
    );
    assert_eq!(
        back.content_hash, before.content_hash,
        "the round trip must land on exactly the value the row started with"
    );
}

// ---------------------------------------------------------------------------
// Refusal
// ---------------------------------------------------------------------------

/// No usable key means **no row**, not a row with an unkeyed digest.
///
/// The count is the assertion, not the error: an error *plus* a row would be strictly worse than
/// either alone, and only counting can tell them apart.
#[tokio::test]
async fn a_sealed_memory_write_with_no_keyring_is_refused_and_writes_no_row() {
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
    let error = repo
        .create_memory(&MemoryInsert {
            id,
            public_id: &public_id,
            application_id: case.fixture.application_id,
            external_tenant_id: Some("tenant-140"),
            external_user_id: Some("user-140"),
            scope: moira::domain::MemoryScope::UserApplication,
            request: &request,
            content: ContentWrite::Encrypt(Zeroizing::new(SECOND_BODY.to_string())),
        })
        .await
        .expect_err("a sealed memory write with no usable key must be refused");

    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        error.error_response(None).error.code,
        "content_key_unavailable"
    );
    assert_eq!(
        case.memory_count().await,
        before,
        "the refused write inserted a row anyway"
    );
    let leaked: i64 =
        sqlx::query_scalar("select count(*) from memory_records where content_plain = $1")
            .bind(SECOND_BODY)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count leaked plaintext");
    assert_eq!(leaked, 0, "the refused body was written as plaintext");
    let addressed: i64 =
        sqlx::query_scalar("select count(*) from memory_records where content_hash = $1")
            .bind(request_hash(SECOND_BODY.as_bytes()))
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count leaked digests");
    assert_eq!(
        addressed, 0,
        "the refused write left an unkeyed digest of its body behind — the body itself was not \
         stored, but the digest alone is the guessing oracle this design closes"
    );
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
