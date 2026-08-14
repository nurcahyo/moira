//! `conversation_content_persistence = 'encrypted_content'`, wired — issue #139.
//!
//! # The one assertion that matters most
//!
//! `encrypted_content_leaves_no_plaintext_in_the_raw_column` asserts that the message body does
//! **not appear as a byte substring of the raw `bytea`**. That single assertion catches the
//! "forgot to actually call the cipher" class, and every other case in this file passes straight
//! through it: a build that wrote the plaintext into `content_encrypted` unchanged would satisfy
//! "`content_plain` is null", "`content_encrypted` is not null", "reads round-trip", and the
//! whole mixed-conversation case, while encrypting nothing at all.
//!
//! It is deliberately on the **raw column bytes** read straight out of PostgreSQL, not on
//! anything the application layer produced. An assertion over a `Vec<u8>` that the sealer handed
//! back would be asserting against the code under test.
//!
//! # What the other cases are for
//!
//! * `plain_content_still_writes_the_plain_column` is the premise. Every absence assertion here
//!   ("no plaintext in `content_plain`") passes trivially against a build that stores nothing at
//!   all, so the file needs one case proving the writer works.
//! * The **mixed conversation** is the permanent steady state, not an edge case. An application
//!   that flips the policy keeps its old rows exactly as they were, so every read path has to
//!   serve both storage forms from one query, forever. It is tested as a first-class case.
//! * The **refusal** case proves no row was written, by counting rows — not by observing an
//!   error. An error plus a plaintext row would be strictly worse than either alone, and only
//!   the count can tell them apart.
//! * The **zero-unwrap** case is here now, before there is a KMS custody to break. Under the
//!   environment backend a per-row `unwrap` costs microseconds and nobody notices; under KMS it
//!   is a network round trip per message and makes the custody swap this whole design was chosen
//!   for impossible.

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::http::StatusCode;
use moira::{
    application::ConversationService,
    domain::{
        ContentWrite, ConversationContentPersistence, ConversationCreateRequest,
        ConversationMessageCreateRequest, ConversationMessageQuery, ConversationMessageRole,
        ConversationPolicyPutRequest,
    },
    infra::repositories::{
        ConversationMessageInsert, ConversationRepository, PgConversationRepository,
    },
    security::{
        ContentKeyring, ENVELOPE_HEADER_LEN, ENVELOPE_MAGIC, KeyCustodyError, MasterKeyCustody,
        WrappedKey,
    },
};
use serde_json::json;
use sqlx::Row;
use support::{LifecycleFixture, admin_actor, request_context};
use zeroize::Zeroizing;

/// High-entropy so a substring search over raw ciphertext cannot match by accident, and so a
/// `content_plain = $1` equality is exact.
const BODY: &str = "MOE1-CANARY-8b41f0d7c96e2a53-CONVERSATION-MESSAGE-BODY";

/// A second, equally distinctive body for the mixed-conversation case.
const SECOND_BODY: &str = "MOE1-CANARY-1c7d5e93af064b28-SECOND-MESSAGE-BODY";

/// What a message row actually holds, read back from PostgreSQL rather than from the API.
struct StoredMessage {
    public_id: String,
    content_plain: Option<String>,
    content_encrypted: Option<Vec<u8>>,
    content_hash: String,
    content_size_bytes: i64,
    token_count: Option<i64>,
}

struct Case {
    fixture: LifecycleFixture,
    actor: moira::security::Actor,
    conversation_id: String,
}

impl Case {
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let actor = moira::security::Actor {
            internal_application_id: Some(fixture.application_id),
            scopes: vec![
                "moira:admin".to_string(),
                "moira:conversations:create".to_string(),
                "moira:conversations:read".to_string(),
                "moira:conversations:write".to_string(),
                "moira:conversation-policies:read".to_string(),
                "moira:conversation-policies:write".to_string(),
            ],
            ..admin_actor()
        };
        let conversation = ConversationService::new(&fixture.state)
            .expect("conversation service")
            .create_conversation(
                &actor,
                &request_context(),
                ConversationCreateRequest {
                    title: Some("f33".to_string()),
                    metadata: json!({}),
                },
            )
            .await
            .expect("create conversation");
        Some(Self {
            fixture,
            actor,
            conversation_id: conversation.id,
        })
    }

    fn service(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// Sets the policy through the **application service**, the way an operator does.
    ///
    /// `encrypted_content` goes through this path too, which is itself part of the contract: the
    /// 422 narrowed to "unusable at write time", so a deployment with a loaded keyring must be
    /// able to select it. A case that reached round the API and wrote the column directly would
    /// not notice the refusal coming back.
    async fn set_policy(&self, persistence: ConversationContentPersistence) {
        self.service()
            .put_conversation_policy(
                &self.actor,
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

    /// Writes a message through the public entry point and reads the raw row back.
    async fn write(&self, body: &str) -> StoredMessage {
        let record = self
            .service()
            .create_message(
                &self.actor,
                &request_context(),
                &self.conversation_id,
                ConversationMessageCreateRequest {
                    role: ConversationMessageRole::User,
                    content: body.to_string(),
                    metadata: json!({ "case": "f33" }),
                },
            )
            .await
            .expect("create message");
        // The record the caller received must describe what was stored. Under
        // `encrypted_content` that means the *decrypted* body: a record that said `null` while a
        // sealed body sat in the row would be the mirror image of finding F32's dishonesty.
        assert_eq!(
            record.content.as_deref(),
            Some(body),
            "the record handed back to the caller does not carry the body that was written"
        );
        self.read_row(&record.id).await
    }

    async fn read_row(&self, public_id: &str) -> StoredMessage {
        let row = sqlx::query(
            "select public_id, content_plain, content_encrypted, content_hash, \
                    content_size_bytes, token_count \
             from conversation_messages where public_id = $1",
        )
        .bind(public_id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("read the message row");
        StoredMessage {
            public_id: row.try_get("public_id").expect("public_id"),
            content_plain: row.try_get("content_plain").expect("content_plain"),
            content_encrypted: row.try_get("content_encrypted").expect("content_encrypted"),
            content_hash: row.try_get("content_hash").expect("content_hash"),
            content_size_bytes: row
                .try_get("content_size_bytes")
                .expect("content_size_bytes"),
            token_count: row.try_get("token_count").expect("token_count"),
        }
    }

    /// Every message in this conversation, in sequence order, as the public API renders it.
    async fn history(&self) -> Vec<(ConversationMessageRole, Option<String>, i64)> {
        self.service()
            .list_messages(
                &self.actor,
                &self.conversation_id,
                &ConversationMessageQuery::default(),
            )
            .await
            .expect("list messages")
            .data
            .into_iter()
            .map(|record| (record.role, record.content.clone(), record.sequence_number))
            .collect()
    }

    async fn message_count(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from conversation_messages m join conversations c \
             on c.id = m.conversation_id where c.public_id = $1",
        )
        .bind(&self.conversation_id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("count messages")
    }
}

// ---------------------------------------------------------------------------
// Storage form
// ---------------------------------------------------------------------------

/// The premise. Without this passing, every absence assertion below is vacuous.
#[tokio::test]
async fn plain_content_still_writes_the_plain_column_and_nothing_encrypted() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;

    let stored = case.write(BODY).await;
    assert_eq!(stored.content_plain.as_deref(), Some(BODY));
    assert_eq!(
        stored.content_encrypted, None,
        "plain_content must not also write the encrypted column; the CHECK constraint from \
         migration 0027 forbids holding both"
    );
}

/// **The assertion this file exists for.**
///
/// The three column-shape assertions are necessary but nowhere near sufficient — a build that
/// bound the plaintext bytes straight into `content_encrypted` satisfies all three. The
/// substring assertion is what separates "wrote something into the encrypted column" from
/// "encrypted it", and it is taken over the raw `bytea` as PostgreSQL returns it.
#[tokio::test]
async fn encrypted_content_leaves_no_plaintext_in_the_raw_column() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let stored = case.write(BODY).await;
    assert_eq!(
        stored.content_plain, None,
        "encrypted_content stored the body in the clear — the value promises encryption"
    );
    let sealed = stored
        .content_encrypted
        .as_ref()
        .expect("encrypted_content must write the encrypted column");

    // Read the column a second time, as raw bytes and with no application type in the way, so
    // the assertion below cannot be satisfied by anything the sealer returned.
    let raw: Vec<u8> = sqlx::query_scalar(
        "select content_encrypted from conversation_messages where public_id = $1",
    )
    .bind(&stored.public_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("read content_encrypted as raw bytes");
    assert_eq!(&raw, sealed);

    assert!(
        !contains_subslice(&raw, BODY.as_bytes()),
        "the message body appears verbatim inside content_encrypted — the cipher was not \
         called, or its output was discarded. Raw column bytes: {} bytes, first 64: {:?}",
        raw.len(),
        &raw[..raw.len().min(64)]
    );

    // And the bytes really are a v1 envelope rather than, say, a base64 or JSON wrapper that
    // happens not to contain the literal body.
    assert!(
        raw.len() > ENVELOPE_HEADER_LEN,
        "a sealed body must be longer than the 42-byte header alone"
    );
    assert_eq!(
        &raw[..4],
        &ENVELOPE_MAGIC,
        "content_encrypted does not begin with the MOE1 envelope magic"
    );
    assert_eq!(
        raw.len(),
        ENVELOPE_HEADER_LEN + BODY.len() + 16,
        "a sealed body is the header, the ciphertext and a 16-byte GCM tag and nothing else"
    );
}

/// The counters must measure the plaintext, not the ciphertext.
///
/// Asserted as a *comparison against the plaintext-policy row* rather than as a bare number: a
/// build that measured the ciphertext would still produce "some positive integer", and only the
/// equality with the `plain_content` row's value distinguishes the two. The envelope adds 58
/// bytes, so the wrong answer is not subtle — but it is invisible to any assertion that only
/// checks the field is non-zero.
#[tokio::test]
async fn the_counters_are_computed_on_the_plaintext_not_the_ciphertext() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    let plain = case.write(BODY).await;

    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let sealed = case.write(BODY).await;

    assert_eq!(
        sealed.content_size_bytes,
        BODY.len() as i64,
        "content_size_bytes measured the ciphertext; a limit or a metric would move under an \
         operator the moment they flipped the policy"
    );
    assert_eq!(
        sealed.content_size_bytes, plain.content_size_bytes,
        "the same body must produce the same content_size_bytes under both policies"
    );
    assert_eq!(
        sealed.token_count, plain.token_count,
        "the same body must produce the same token estimate under both policies"
    );
    assert_ne!(
        sealed.content_size_bytes,
        sealed
            .content_encrypted
            .as_ref()
            .expect("a sealed body")
            .len() as i64,
        "premise: the ciphertext really is a different length, so the assertions above are not \
         trivially true"
    );
    // `content_hash` is a peppered HMAC over the plaintext, so it is identical under both
    // policies — which is what makes it usable for idempotency across a policy change.
    assert_eq!(
        sealed.content_hash, plain.content_hash,
        "content_hash must be a fingerprint of the plaintext under every policy"
    );
    assert!(
        sealed.content_hash.contains(':'),
        "content_hash must keep its published \"{{pepper_version}}:{{base64url}}\" shape: {}",
        sealed.content_hash
    );
}

/// A body at exactly the documented cap must be accepted under `encrypted_content`.
///
/// Its ciphertext is 262,202 bytes, well over the 262,144-byte limit, so a build that moved the
/// cap onto the sealed bytes fails here and passes every other case in this file.
#[tokio::test]
async fn the_content_cap_applies_to_the_plaintext_not_the_ciphertext() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let body = "z".repeat(262_144);
    let stored = case.write(&body).await;
    assert_eq!(stored.content_size_bytes, 262_144);
    assert!(
        stored
            .content_encrypted
            .as_ref()
            .is_some_and(|sealed| sealed.len() > 262_144),
        "premise: the ciphertext really does exceed the cap, so this case is not vacuous"
    );
}

// ---------------------------------------------------------------------------
// Round trip and the mixed steady state
// ---------------------------------------------------------------------------

/// Byte-identical, through the public API, for a body that is not plain ASCII.
///
/// The multibyte and control characters are the point: a build that round-tripped through a
/// lossy conversion — `String::from_utf8_lossy`, a `char` filter, a trim — would pass an
/// ASCII-only assertion and corrupt real user text.
#[tokio::test]
async fn a_sealed_body_reads_back_byte_identical_through_the_public_api() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let body = "MOE1-CANARY-روبن — 日本語 — \u{1F600}\n\ttrailing spaces   ";
    let stored = case.write(body).await;
    assert!(stored.content_encrypted.is_some());

    let history = case.history().await;
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].1.as_deref(),
        Some(body),
        "the body did not survive the round trip byte for byte"
    );
    assert_eq!(
        history[0].1.as_deref().map(str::len),
        Some(body.len()),
        "the round-tripped body has a different byte length"
    );
}

/// **The permanent steady state.** Three plaintext turns, a policy flip, three sealed turns,
/// then one read.
///
/// This is not an edge case and it never stops being reachable: switching to `encrypted_content`
/// does not encrypt existing history, so every conversation that ever flips carries both forms
/// for the rest of its life and every read path must serve them from one query.
#[tokio::test]
async fn a_conversation_that_flips_the_policy_reads_back_whole_and_in_order() {
    let Some(case) = Case::new().await else {
        return;
    };

    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;
    let mut expected = Vec::new();
    for index in 0..3 {
        let body = format!("{BODY}-plain-{index}");
        case.write(&body).await;
        expected.push(body);
    }

    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    for index in 0..3 {
        let body = format!("{SECOND_BODY}-sealed-{index}");
        case.write(&body).await;
        expected.push(body);
    }

    // Premise: the two halves really are stored differently. Without this the case would pass
    // against a build that ignored the flip entirely.
    let (plain_rows, sealed_rows): (i64, i64) = {
        let row = sqlx::query(
            "select count(*) filter (where m.content_plain is not null) as plain, \
                    count(*) filter (where m.content_encrypted is not null) as sealed \
             from conversation_messages m join conversations c on c.id = m.conversation_id \
             where c.public_id = $1",
        )
        .bind(&case.conversation_id)
        .fetch_one(&case.fixture.pool)
        .await
        .expect("count storage forms");
        (
            row.try_get("plain").expect("plain"),
            row.try_get("sealed").expect("sealed"),
        )
    };
    assert_eq!(
        (plain_rows, sealed_rows),
        (3, 3),
        "the conversation is not actually mixed, so this case proves nothing"
    );

    let history = case.history().await;
    assert_eq!(history.len(), 6, "{history:?}");
    let bodies: Vec<Option<String>> = history.iter().map(|entry| entry.1.clone()).collect();
    let sequences: Vec<i64> = history.iter().map(|entry| entry.2).collect();
    assert_eq!(
        bodies,
        expected.iter().cloned().map(Some).collect::<Vec<_>>(),
        "the mixed history came back with the wrong bodies or in the wrong order"
    );
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "the mixed history is not in ascending sequence order: {sequences:?}"
    );
}

// ---------------------------------------------------------------------------
// Refusal, never fallback
// ---------------------------------------------------------------------------

/// The write is **refused and nothing is written**.
///
/// The row count is the assertion, not the error. A build that returned `503` *and* wrote the
/// plaintext would satisfy every error-shaped assertion while being strictly worse than one that
/// only wrote the plaintext, because the operator would believe the refusal.
#[tokio::test]
async fn an_encrypted_write_with_no_usable_key_is_refused_and_writes_no_row() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    // One successful write first, so the count below is a *change* rather than a comparison
    // against zero — a repository that wrote nothing at all under any condition would pass an
    // `== 0` assertion.
    case.write(BODY).await;
    let before = case.message_count().await;
    assert_eq!(before, 1);

    // A repository with no content keyring is the shape of a process that cannot seal. It is a
    // real constructor arm, not a test hook: `AppState` passes `None` whenever there is no
    // database, and a future custody backend that fails to unwrap the active key after boot
    // reaches the same refusal through `active_cipher`.
    let repo = PgConversationRepository::new(case.fixture.pool.clone(), None);
    let error = repo
        .add_message(&ConversationMessageInsert {
            conversation_public_id: case.conversation_id.clone(),
            response_id: None,
            execution_id: None,
            role: ConversationMessageRole::User,
            message_type: moira::domain::ConversationMessageType::Input,
            content: ContentWrite::Plain(SECOND_BODY.to_string()),
            content_hash: "test:refusal".to_string(),
            content_size_bytes: SECOND_BODY.len() as i64,
            token_count: Some(1),
            metadata: json!({}),
        })
        .await
        .expect_err("an encrypted write with no usable key must be refused");

    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = error.error_response(None);
    assert_eq!(response.error.code, "content_key_unavailable");

    let after = case.message_count().await;
    assert_eq!(
        after, before,
        "the refused write inserted a row anyway. A row plus an error is worse than either: \
         under an encrypted policy that row can only hold plaintext, which is finding F32 with \
         extra steps"
    );
    let leaked: i64 =
        sqlx::query_scalar("select count(*) from conversation_messages where content_plain = $1")
            .bind(SECOND_BODY)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count leaked plaintext");
    assert_eq!(leaked, 0, "the refused body was written as plaintext");
}

/// The admin API's narrowed 422: `encrypted_content` is accepted when the keyring is loaded.
///
/// The old test asserted the opposite (`encrypted_content_is_refused_because_no_cipher_exists`)
/// and was correct until this release. Kept as the positive half so the narrowing cannot be
/// quietly widened back.
#[tokio::test]
async fn the_admin_api_now_accepts_encrypted_content_and_stores_it() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.service()
        .put_conversation_policy(
            &case.actor,
            &request_context(),
            case.fixture.application_id,
            ConversationPolicyPutRequest {
                conversation_content_persistence: Some(
                    ConversationContentPersistence::EncryptedContent,
                ),
                ..ConversationPolicyPutRequest::default()
            },
        )
        .await
        .expect("encrypted_content must be accepted once a cipher is wired");

    let persisted: String = sqlx::query_scalar(
        "select conversation_content_persistence from application_conversation_policies \
         where application_id = $1",
    )
    .bind(case.fixture.application_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("read the policy back");
    assert_eq!(persisted, "encrypted_content");
}

// ---------------------------------------------------------------------------
// Damage
// ---------------------------------------------------------------------------

/// A corrupted ciphertext **body** is an AEAD failure: one opaque code, and a response body that
/// carries nothing.
///
/// The literal assertions are the point. A message that named the key id, the reason, or any
/// fragment of the plaintext would turn a damaged row into an oracle, and a `contains(...)`
/// assertion would not notice a message that grew one.
#[tokio::test]
async fn a_corrupted_ciphertext_body_is_one_opaque_decryption_failure() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let stored = case.write(BODY).await;
    let mut sealed = stored.content_encrypted.expect("a sealed body");
    // Flip a bit in the ciphertext body, past the 42-byte header, so the framing still validates
    // and the failure is genuinely the tag rather than the length check.
    let index = ENVELOPE_HEADER_LEN + 1;
    sealed[index] ^= 0b0000_0001;
    overwrite_ciphertext(&case, &stored.public_id, &sealed).await;

    let error = case
        .service()
        .list_messages(
            &case.actor,
            &case.conversation_id,
            &ConversationMessageQuery::default(),
        )
        .await
        .expect_err("a corrupted ciphertext must not read back as content");
    let response = error.error_response(None);
    assert_eq!(response.error.code, "content_decryption_failed");
    assert_eq!(
        response.error.message, "the stored content could not be decrypted",
        "the public message must be this constant and nothing else"
    );
    assert!(
        !response.error.message.contains(BODY),
        "the plaintext leaked into the failure message"
    );
    let rendered = serde_json::to_string(&response).expect("render the error response");
    for needle in [BODY, "aead", "nonce", "data_key", "key_id"] {
        assert!(
            !rendered.to_lowercase().contains(&needle.to_lowercase()),
            "{needle:?} reached the response body: {rendered}"
        );
    }
}

/// A header failure is a **different** code from an AEAD failure.
///
/// If the two collapsed, an operator could no longer tell "this row was written by a newer build"
/// from "your key is wrong" — the two conditions with opposite remedies that the self-describing
/// header exists to separate. Asserted as an inequality against the AEAD code as well as an
/// equality, so a rename that merged them fails here rather than silently agreeing.
#[tokio::test]
async fn a_stored_envelope_from_a_newer_format_is_a_distinct_refusal() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let stored = case.write(BODY).await;
    let mut sealed = stored.content_encrypted.expect("a sealed body");
    // Offset 4 is `format_version`. Bumping it is exactly what a row written by a v2 build and
    // then read by this one looks like.
    sealed[4] = 0x02;
    overwrite_ciphertext(&case, &stored.public_id, &sealed).await;

    let error = case
        .service()
        .list_messages(
            &case.actor,
            &case.conversation_id,
            &ConversationMessageQuery::default(),
        )
        .await
        .expect_err("an unreadable envelope format must be refused");
    let response = error.error_response(None);
    assert_eq!(response.error.code, "content_envelope_unsupported");
    assert_ne!(
        response.error.code, "content_decryption_failed",
        "a framing failure and a tag failure must stay distinguishable"
    );
    // The message is the constant, and none of the twelve framing discriminants appears anywhere
    // in the rendered body. Asserting only against `error.message` would miss a discriminant
    // smuggled into `details`, which is exactly where a well-meaning "help the operator" change
    // would put it.
    assert_eq!(
        response.error.message,
        "the stored content is not in a format this build can read"
    );
    assert_eq!(
        response.error.details, None,
        "a framing refusal must carry no details; the discriminant belongs in the log"
    );
    let rendered = serde_json::to_string(&response).expect("render the error response");
    for discriminant in [
        "too_short",
        "bad_magic",
        "unsupported_format_version",
        "unsupported_algorithm",
        "unsupported_key_mode",
        "reserved_not_zero",
        "unknown_aad_profile",
        "body_length_mismatch",
        "profile_mismatch",
        "data_key_mismatch",
        "plaintext_too_large",
        "format_version",
    ] {
        assert!(
            !rendered.contains(discriminant),
            "{discriminant:?} reached the wire; it belongs in the log only: {rendered}"
        );
    }
}

/// **The impossible row: both columns non-null.** Encrypted wins.
///
/// `migrations/0027_content_encryption_keyring.sql` adds
/// `check (content_plain is null or content_encrypted is null)` — but `NOT VALID`, so rows
/// written before it was added were never checked and this state is reachable in a real upgraded
/// deployment. It is *ambiguous*, not merely untidy: the row carries two contradictory answers
/// about what it stores.
///
/// Encrypted wins because it is the stricter of the two intentions. Serving the plaintext of a
/// row that also carries a sealed body would quietly hand out content an operator believed was
/// encrypted, which is finding F32's shape one more time.
///
/// **Reached by dropping the constraint for this row's lifetime**, because the constraint is
/// doing its job and there is no other way to represent the pre-0027 row this case is about.
#[tokio::test]
async fn a_row_holding_both_a_plaintext_and_a_ciphertext_serves_the_ciphertext() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let stored = case.write(BODY).await;
    assert!(stored.content_encrypted.is_some());

    sqlx::query(
        "alter table conversation_messages \
         drop constraint conversation_messages_content_single_form",
    )
    .execute(&case.fixture.pool)
    .await
    .expect(
        "drop the 0027 CHECK constraint; if this fails the constraint was renamed and this case \
         is no longer exercising the ambiguous row it claims to",
    );
    let affected =
        sqlx::query("update conversation_messages set content_plain = $2 where public_id = $1")
            .bind(&stored.public_id)
            .bind(SECOND_BODY)
            .execute(&case.fixture.pool)
            .await
            .expect("write a second, contradictory body into content_plain")
            .rows_affected();
    assert_eq!(affected, 1);

    let history = case.history().await;
    assert_eq!(history.len(), 1);
    assert_eq!(
        history[0].1.as_deref(),
        Some(BODY),
        "the sealed body must win; serving the plaintext of a row that also carries a ciphertext \
         hands out content an operator believed was encrypted"
    );
    assert_ne!(
        history[0].1.as_deref(),
        Some(SECOND_BODY),
        "the plaintext column won over the sealed body"
    );
}

/// The AAD binds the row's identity, so a ciphertext lifted from one message into another does
/// not open — even though both were sealed under the same key, in the same conversation.
///
/// This is the property that stops an attacker with database *write* access from moving one
/// tenant's content into another's, and no other case here would notice its loss: a build that
/// passed an empty AAD would round-trip every message perfectly.
#[tokio::test]
async fn a_ciphertext_moved_to_another_row_does_not_open() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    let first = case.write(BODY).await;
    let second = case.write(SECOND_BODY).await;

    let lifted = first.content_encrypted.expect("a sealed body");
    overwrite_ciphertext(&case, &second.public_id, &lifted).await;

    let error = case
        .service()
        .list_messages(
            &case.actor,
            &case.conversation_id,
            &ConversationMessageQuery::default(),
        )
        .await
        .expect_err("a ciphertext bound to another row must not open here");
    assert_eq!(
        error.error_response(None).error.code,
        "content_decryption_failed",
        "the AAD did not bind the row identity, so a lifted ciphertext decrypted in place"
    );
}

// ---------------------------------------------------------------------------
// Zero unwrap on read
// ---------------------------------------------------------------------------

/// Counts `unwrap` calls and forwards everything else to a real backend.
///
/// Deliberately a wrapper rather than a fake: a stub that "unwrapped" by returning bytes would
/// prove nothing about whether the real path is reached, and the whole point is to count calls
/// on the path production takes.
#[derive(Debug)]
struct CountingCustody {
    inner: Arc<dyn MasterKeyCustody>,
    unwraps: AtomicU64,
}

impl CountingCustody {
    fn new(inner: Arc<dyn MasterKeyCustody>) -> Self {
        Self {
            inner,
            unwraps: AtomicU64::new(0),
        }
    }

    fn unwrap_calls(&self) -> u64 {
        self.unwraps.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MasterKeyCustody for CountingCustody {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }
    fn active_master_key_id(&self) -> &str {
        self.inner.active_master_key_id()
    }
    fn can_unwrap(&self, master_key_id: &str) -> bool {
        self.inner.can_unwrap(master_key_id)
    }
    fn wrap_algorithm(&self) -> &'static str {
        self.inner.wrap_algorithm()
    }
    fn master_key_ids(&self) -> Vec<String> {
        self.inner.master_key_ids()
    }
    async fn wrap(
        &self,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        self.inner.wrap(dek, aad).await
    }
    async fn wrap_under(
        &self,
        master_key_id: &str,
        dek: &Zeroizing<[u8; 32]>,
        aad: &[u8],
    ) -> Result<WrappedKey, KeyCustodyError> {
        self.inner.wrap_under(master_key_id, dek, aad).await
    }
    async fn unwrap(
        &self,
        wrapped: &WrappedKey,
        aad: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
        self.unwraps.fetch_add(1, Ordering::SeqCst);
        self.inner.unwrap(wrapped, aad).await
    }
    async fn preflight(&self) -> Result<(), KeyCustodyError> {
        self.inner.preflight().await
    }
}

/// **Reading history must not touch key custody.**
///
/// Under the environment backend a per-row unwrap costs microseconds and nobody notices. Under a
/// KMS custody it is a network round trip per message — 24 of them on a normal turn — and it
/// makes the backend swap this whole design was chosen for impossible. The guard has to land
/// before there is a KMS implementation to break.
///
/// **What makes this structural, not just measured.** [`moira::security::ContentOpener`] is a
/// **synchronous** trait, and `MasterKeyCustody::unwrap` is `async`. A per-row unwrap on the read
/// path cannot be written without first changing the trait, which is a visible, reviewable edit.
/// This test is the empirical backstop for that argument, and it is what would catch someone
/// reaching for `block_on`.
#[tokio::test]
async fn reading_history_never_calls_key_custody() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;
    for index in 0..6 {
        case.write(&format!("{BODY}-{index}")).await;
    }

    // A second keyring over the same rows, behind a counting custody. Same database, same data
    // keys, same envelopes — only the custody is instrumented.
    let custody = Arc::new(CountingCustody::new(
        case.fixture.state.content_custody.custody(),
    ));
    let keyring = Arc::new(
        ContentKeyring::load(
            case.fixture.pool.clone(),
            custody.clone(),
            &case.fixture.state.settings.content_encryption,
            case.fixture.state.metrics.clone(),
        )
        .await
        .expect("load a counting keyring"),
    );
    let repo = PgConversationRepository::new(case.fixture.pool.clone(), Some(keyring));

    // Warm-up: the load above unwrapped every loadable key exactly once, which is the cost this
    // design pays at boot instead of per row.
    let after_boot = custody.unwrap_calls();
    assert!(
        after_boot > 0,
        "the keyring load unwrapped nothing, so the counter is not wired to the real path and \
         the assertion below would pass against any implementation"
    );

    let before = custody.unwrap_calls();
    let history = repo
        .list_messages(
            &case.conversation_id,
            &ConversationMessageQuery::default(),
            None,
            32,
        )
        .await
        .expect("read history through the counting keyring");
    assert_eq!(history.len(), 6, "premise: six sealed messages were read");
    for (record, _) in &history {
        assert!(
            record
                .content
                .as_deref()
                .is_some_and(|body| body.starts_with(BODY)),
            "premise: the read really did open the sealed bodies, so a zero count below means \
             'opened without custody' rather than 'never opened'"
        );
    }
    assert_eq!(
        custody.unwrap_calls(),
        before,
        "reading history must not touch key custody; a per-row unwrap makes KMS custody unusable"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Writes raw bytes into `content_encrypted`, bypassing every application path.
///
/// The `update` is the only way to represent a row damaged in storage, a row written by another
/// build, or a row an attacker with write access moved. Asserting `rows_affected == 1` matters:
/// a no-op update would leave the original, valid ciphertext in place and every case that uses
/// this helper would assert against an undamaged row.
async fn overwrite_ciphertext(case: &Case, public_id: &str, bytes: &[u8]) {
    let affected =
        sqlx::query("update conversation_messages set content_encrypted = $2 where public_id = $1")
            .bind(public_id)
            .bind(bytes)
            .execute(&case.fixture.pool)
            .await
            .expect("overwrite content_encrypted")
            .rows_affected();
    assert_eq!(
        affected, 1,
        "no row was updated, so this case would assert against the original ciphertext"
    );
}

/// Whether `haystack` contains `needle` as a contiguous byte slice.
///
/// Written out rather than reached for through a crate, because the assertion that depends on it
/// is the most important one in this file and it should be obvious that it does what it says.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod helper_tests {
    use super::contains_subslice;

    /// The substring helper is load-bearing for this file's headline assertion, so it gets its
    /// own coverage. A helper that always returned `false` would make that assertion vacuous and
    /// nothing else here would notice.
    #[test]
    fn the_substring_helper_finds_what_it_should_and_nothing_else() {
        assert!(contains_subslice(b"abcdef", b"cde"));
        assert!(contains_subslice(b"abcdef", b"abcdef"));
        assert!(contains_subslice(b"abcdef", b"a"));
        assert!(contains_subslice(b"abcdef", b"f"));
        assert!(!contains_subslice(b"abcdef", b"ace"));
        assert!(!contains_subslice(b"abcdef", b"abcdefg"));
        assert!(!contains_subslice(b"abc", b""));
    }
}
