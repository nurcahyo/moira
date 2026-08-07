//! `rag_document_versions` and `rag_chunks` under
//! `conversation_content_persistence = 'encrypted_content'` — AAD profiles 4 and 5, issue #141.
//!
//! This is the last pair of the five columns, and it is the pair with the most asymmetric shape,
//! so the file is organised around that asymmetry rather than around the columns.
//!
//! # Profile 4 has no reader, and that is exactly why it needs a test
//!
//! `rag_document_versions.content_encrypted` is written and never read: the chunker consumes the
//! in-memory ingestion plan and `rag_document_record_from_row` does not project the column. A
//! write-only column is the easiest possible place to seal something under the *wrong* AAD and
//! never find out — every behavioural test passes, because nothing opens it. So
//! `a_sealed_document_version_opens_under_the_identity_it_claims` opens the stored bytes with the
//! identity built from the row's own columns and asserts the plaintext comes back, and
//! `a_sealed_document_version_refuses_a_neighbouring_rows_identity` proves the binding is not
//! decoration.
//!
//! # Profile 5 has a reader, so it gets an end-to-end assertion instead
//!
//! `a_sealed_chunk_reaches_the_model_through_the_retrieval_path` drives a real turn through
//! `prepare_response_conversation` and asserts the chunk body appears in the context the planner
//! assembled. That is the assertion an identity-built-in-the-test cannot make: it proves
//! `chunk_candidate_from_row` builds the **right** identity, not merely that some identity works.
//!
//! # The byte-substring assertions are read as raw `bytea`
//!
//! Every "no plaintext" claim is checked with `sqlx::query_scalar::<_, Vec<u8>>` against the
//! column, with no row mapper and no application type between the assertion and the storage. That
//! is the one assertion that catches "we forgot to actually call the cipher", which every other
//! test in this file passes straight through.

mod support;

use std::{collections::HashMap, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use moira::{
    application::ConversationService,
    config::Settings,
    domain::{
        ConversationContentPersistence, ConversationPolicyPutRequest, PublicContentPart,
        PublicInputMessage, PublicMessageRole, RagCollectionCreateRequest, RagCollectionVisibility,
        RagDocumentCreateRequest, RagDocumentIngestRequest, ResponseConversationInput,
        RetrievalPolicyPutRequest,
    },
    security::{
        Actor, ContentIdentity, ContentOpener, ENVELOPE_HEADER_LEN, ENVELOPE_MAGIC,
        KeyringContentAccess,
    },
};
use serde_json::json;
use sqlx::Row;
use support::{
    LifecycleFixture,
    mock_openai::{EmbeddingBehaviour, MockOpenAiServer, planar_vector},
    request_context,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(25);

/// High-entropy on purpose. A byte-substring search over ciphertext cannot match this by
/// accident, and it is short enough to be exactly one chunk.
const DOCUMENT: &str = "MOE1-CANARY-6b1d4a09f27e3c58-RAG-DOCUMENT-BODY";
/// The revised body used by the `/ingest` path, so the two write sites are covered by distinct
/// canaries and a test cannot pass by finding the other one's row.
const REVISED: &str = "MOE1-CANARY-c084e51fa93d7b26-RAG-REVISED-BODY";
/// The user turn. Embedded at the same angle as the document, so retrieval is decided by the
/// mock's fixed vectors rather than by its hash function.
const QUERY: &str = "what does the canary document say";

/// One named master key rather than the dev sentinel, so the fixture is production-shaped.
const MASTER: [u8; 32] = [0x41; 32];

fn keys_setting() -> String {
    format!("m141:{}", STANDARD.encode(MASTER))
}

fn embedding_behaviour() -> EmbeddingBehaviour {
    EmbeddingBehaviour::Fixed {
        vectors: HashMap::from([
            (QUERY.to_string(), planar_vector(0.0)),
            (DOCUMENT.to_string(), planar_vector(0.0)),
            (REVISED.to_string(), planar_vector(0.0)),
        ]),
    }
}

/// What a `rag_document_versions` row actually holds, read back from PostgreSQL.
#[derive(Debug)]
struct StoredVersion {
    id: Uuid,
    document_id: Uuid,
    version_number: i32,
    content_plain: Option<String>,
    content_encrypted: Option<Vec<u8>>,
}

/// What a `rag_chunks` row actually holds, read back from PostgreSQL.
#[derive(Debug)]
struct StoredChunk {
    id: Uuid,
    document_version_id: Uuid,
    chunk_index: i32,
    chunk_text_plain: Option<String>,
    chunk_text_encrypted: Option<Vec<u8>>,
}

struct Case {
    fixture: LifecycleFixture,
    embeddings: Option<MockOpenAiServer>,
    caller: Actor,
}

impl Case {
    /// A fixture with a real content keyring and **no** embedding provider.
    ///
    /// Ingestion still writes chunks; it simply writes no `rag_chunk_embeddings` rows. Every
    /// storage-form assertion in this file needs chunks and none of them needs a vector, so the
    /// cheap fixture is the default and the mock is opt-in.
    async fn new() -> Option<Self> {
        Self::build(false).await
    }

    /// The same fixture plus an embedding provider, for the one test that retrieves.
    async fn with_embeddings() -> Option<Self> {
        Self::build(true).await
    }

    async fn build(with_embeddings: bool) -> Option<Self> {
        let fixture = LifecycleFixture::with_settings(move |settings: &mut Settings| {
            settings.content_encryption.keys = keys_setting();
            settings.content_encryption.active_key_id = "m141".to_string();
            settings.content_encryption.allow_insecure_dev_key = false;
        })
        .await?;
        let embeddings = if with_embeddings {
            let server = MockOpenAiServer::start(Vec::new()).await;
            server.set_embedding_behaviour(embedding_behaviour()).await;
            fixture
                .enable_rag_embeddings(server.base_url(), "text-embedding-3-small")
                .await;
            fixture
                .enable_retrieval(RetrievalPolicyPutRequest {
                    // Memory retrieval would need a second embedding round trip and proves
                    // nothing here; #140 owns it.
                    memory_retrieval_enabled: Some(false),
                    ..RetrievalPolicyPutRequest::default()
                })
                .await;
            Some(server)
        } else {
            None
        };
        let mut caller = fixture.caller_actor(Some("tenant-141"), Some("user-141"));
        for scope in [
            "moira:rag-collections:write",
            "moira:rag-documents:write",
            "moira:rag-documents:ingest",
            "moira:rag-documents:read",
            "moira:conversations:write",
            "moira:conversations:read",
            "moira:conversation-policies:write",
        ] {
            caller.scopes.push(scope.to_string());
        }
        Some(Self {
            fixture,
            embeddings,
            caller,
        })
    }

    fn service(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// The same object the repositories seal and open with, so a test that opens a stored
    /// envelope is using the production reader rather than a parallel one.
    fn access(&self) -> KeyringContentAccess {
        self.fixture.state.content_access()
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
                    conversations_enabled: Some(true),
                    caller_can_create_conversations: Some(true),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{persistence:?} must be accepted: {error:?}"));
    }

    /// Creates a collection and a document with inline content — the `create` write site.
    async fn create_document(&self, content: &str) -> String {
        let suffix = Uuid::now_v7().simple().to_string();
        let collection = tokio::time::timeout(
            WAIT,
            self.service().create_rag_collection(
                &self.caller,
                &request_context(),
                RagCollectionCreateRequest {
                    application_id: self.fixture.application_id,
                    external_tenant_id: None,
                    collection_key: format!("canary-{suffix}"),
                    display_name: format!("Canary {suffix}"),
                    description: None,
                    visibility: RagCollectionVisibility::Application,
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create collection timed out")
        .expect("create collection");
        let document = tokio::time::timeout(
            WAIT,
            self.service().create_rag_document(
                &self.caller,
                &request_context(),
                &collection.id,
                RagDocumentCreateRequest {
                    external_document_id: Some(format!("doc-{suffix}")),
                    title: format!("Canary {suffix}"),
                    source_type: "direct_text".to_string(),
                    source_uri: None,
                    mime_type: "text/plain".to_string(),
                    content: Some(content.to_string()),
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("create document timed out")
        .expect("create document");
        document.id
    }

    /// Re-ingests an existing document — the second `rag_document_versions` write site.
    async fn ingest(&self, document_id: &str, content: &str) {
        tokio::time::timeout(
            WAIT,
            self.service().ingest_rag_document(
                &self.caller,
                &request_context(),
                document_id,
                RagDocumentIngestRequest {
                    content: Some(content.to_string()),
                    source_etag: None,
                    source_last_modified: None,
                    metadata: json!({}),
                },
            ),
        )
        .await
        .expect("ingest timed out")
        .expect("ingest");
    }

    async fn versions(&self, document_public_id: &str) -> Vec<StoredVersion> {
        sqlx::query(
            "select v.id, v.document_id, v.version_number, v.content_plain, v.content_encrypted \
             from rag_document_versions v \
             join rag_documents d on d.id = v.document_id \
             where d.public_id = $1 order by v.version_number",
        )
        .bind(document_public_id)
        .fetch_all(&self.fixture.pool)
        .await
        .expect("read the version rows")
        .into_iter()
        .map(|row| StoredVersion {
            id: row.try_get("id").expect("id"),
            document_id: row.try_get("document_id").expect("document_id"),
            version_number: row.try_get("version_number").expect("version_number"),
            content_plain: row.try_get("content_plain").expect("content_plain"),
            content_encrypted: row.try_get("content_encrypted").expect("content_encrypted"),
        })
        .collect()
    }

    async fn chunks(&self, document_public_id: &str) -> Vec<StoredChunk> {
        sqlx::query(
            "select ch.id, ch.document_version_id, ch.chunk_index, ch.chunk_text_plain, \
                    ch.chunk_text_encrypted \
             from rag_chunks ch \
             join rag_document_versions v on v.id = ch.document_version_id \
             join rag_documents d on d.id = v.document_id \
             where d.public_id = $1 and v.superseded_at is null \
             order by ch.chunk_index",
        )
        .bind(document_public_id)
        .fetch_all(&self.fixture.pool)
        .await
        .expect("read the chunk rows")
        .into_iter()
        .map(|row| StoredChunk {
            id: row.try_get("id").expect("id"),
            document_version_id: row
                .try_get("document_version_id")
                .expect("document_version_id"),
            chunk_index: row.try_get("chunk_index").expect("chunk_index"),
            chunk_text_plain: row.try_get("chunk_text_plain").expect("chunk_text_plain"),
            chunk_text_encrypted: row
                .try_get("chunk_text_encrypted")
                .expect("chunk_text_encrypted"),
        })
        .collect()
    }

    /// The column's bytes, with no row mapper and no application type in the way.
    async fn raw_column(&self, table: &str, column: &str, id: Uuid) -> Vec<u8> {
        let sql = format!("select {column} from {table} where id = $1");
        sqlx::query_scalar::<_, Vec<u8>>(&sql)
            .bind(id)
            .fetch_one(&self.fixture.pool)
            .await
            .unwrap_or_else(|error| panic!("read raw {table}.{column}: {error}"))
    }

    async fn shutdown(self) {
        if let Some(embeddings) = self.embeddings {
            embeddings.shutdown().await;
        }
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// A v1 envelope of exactly `plaintext`, and nothing else — the length arithmetic proves there
/// is no room for the body to be smuggled alongside the ciphertext.
fn assert_is_envelope_of(raw: &[u8], plaintext: &str, what: &str) {
    assert!(
        !contains_subslice(raw, plaintext.as_bytes()),
        "{what} contains the plaintext verbatim — the cipher was not called, or its output was \
         discarded. {} bytes, first 64: {:?}",
        raw.len(),
        &raw[..raw.len().min(64)]
    );
    assert_eq!(&raw[..4], &ENVELOPE_MAGIC, "{what} is not a MOE1 envelope");
    assert_eq!(
        raw.len(),
        ENVELOPE_HEADER_LEN + plaintext.len() + 16,
        "{what} is not the 42-byte header, the ciphertext and a 16-byte GCM tag and nothing else"
    );
}

// ---------------------------------------------------------------------------
// Storage form — the premise, then the two columns
// ---------------------------------------------------------------------------

/// The premise. Every "no plaintext anywhere" assertion below passes trivially against a build
/// that stores nothing at all, so the file needs one case proving the writer works at all.
#[tokio::test]
async fn plain_content_rag_bodies_stay_in_the_plain_columns() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::PlainContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    let versions = case.versions(&document_id).await;
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].content_plain.as_deref(), Some(DOCUMENT));
    assert_eq!(
        versions[0].content_encrypted, None,
        "plain_content must not also write the encrypted column; migration 0027's CHECK forbids \
         holding both"
    );

    let chunks = case.chunks(&document_id).await;
    assert_eq!(chunks.len(), 1, "the canary body is exactly one chunk");
    assert_eq!(chunks[0].chunk_text_plain.as_deref(), Some(DOCUMENT));
    assert_eq!(chunks[0].chunk_text_encrypted, None);

    case.shutdown().await;
}

/// `none` and `metadata_only` deliberately do **not** omit a RAG body.
///
/// This is the one place the RAG rule diverges from the conversation rule, and it is asserted
/// rather than left to `ContentWrite::under_policy_for_rag`'s doc comment — a decision nothing
/// tests is a decision that gets quietly reversed. The reasoning is on that function: dropping
/// the body while still storing the section heading, the offsets, the unkeyed hash and an
/// invertible embedding would be a privacy claim that is not true.
#[tokio::test]
async fn a_storage_policy_of_none_still_stores_rag_bodies_in_the_clear() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::None).await;

    let document_id = case.create_document(DOCUMENT).await;

    assert_eq!(
        case.versions(&document_id).await[0]
            .content_plain
            .as_deref(),
        Some(DOCUMENT),
        "a RAG body under `none` must still be stored; omitting it would suppress the one \
         artifact of a document that the ingest pipeline can suppress, while leaving the \
         heading, the offsets, the hash and the embedding in place"
    );
    assert_eq!(
        case.chunks(&document_id).await[0]
            .chunk_text_plain
            .as_deref(),
        Some(DOCUMENT)
    );

    case.shutdown().await;
}

/// Profile 4, on the create path — the "forgot to call the cipher" catcher on raw column bytes.
#[tokio::test]
async fn an_encrypted_document_version_leaves_no_plaintext_in_the_raw_column() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    let versions = case.versions(&document_id).await;
    assert_eq!(versions.len(), 1);
    assert_eq!(
        versions[0].content_plain, None,
        "encrypted_content stored the document in the clear — the value promises encryption"
    );
    let sealed = versions[0]
        .content_encrypted
        .as_ref()
        .expect("encrypted_content must write rag_document_versions.content_encrypted");

    let raw = case
        .raw_column("rag_document_versions", "content_encrypted", versions[0].id)
        .await;
    assert_eq!(&raw, sealed);
    assert_is_envelope_of(&raw, DOCUMENT, "rag_document_versions.content_encrypted");

    case.shutdown().await;
}

/// Profile 4 again, on the **other** write site.
///
/// Two inserts write this column and they are in different functions with different version
/// numbers. A test that only covered `create` would pass against an `/ingest` path that forgot
/// the cipher entirely, which is the more likely of the two to be forgotten because it is the
/// one that runs after a superseding update.
#[tokio::test]
async fn the_ingest_path_seals_its_document_version_too() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    case.ingest(&document_id, REVISED).await;

    let versions = case.versions(&document_id).await;
    assert_eq!(
        versions.len(),
        2,
        "the re-ingest must write a second version"
    );
    assert_eq!(versions[1].version_number, 2);
    assert_eq!(versions[1].content_plain, None);
    let raw = case
        .raw_column("rag_document_versions", "content_encrypted", versions[1].id)
        .await;
    assert_is_envelope_of(&raw, REVISED, "the re-ingested version's content_encrypted");

    // And the superseded version keeps the bytes it was written with. Re-encryption on write is
    // exactly what this design refuses, so a re-ingest that rewrote version 1 would be a defect
    // even though the plaintext would still come back.
    let first = case
        .raw_column("rag_document_versions", "content_encrypted", versions[0].id)
        .await;
    assert_is_envelope_of(
        &first,
        DOCUMENT,
        "the superseded version's content_encrypted",
    );

    case.shutdown().await;
}

/// Profile 5 — the same catcher, on the column an operator's chunks actually live in.
#[tokio::test]
async fn an_encrypted_chunk_leaves_no_plaintext_in_the_raw_column() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    let chunks = case.chunks(&document_id).await;
    assert_eq!(chunks.len(), 1);
    assert_eq!(
        chunks[0].chunk_text_plain, None,
        "encrypted_content stored the chunk in the clear"
    );
    let sealed = chunks[0]
        .chunk_text_encrypted
        .as_ref()
        .expect("encrypted_content must write rag_chunks.chunk_text_encrypted");

    let raw = case
        .raw_column("rag_chunks", "chunk_text_encrypted", chunks[0].id)
        .await;
    assert_eq!(&raw, sealed);
    assert_is_envelope_of(&raw, DOCUMENT, "rag_chunks.chunk_text_encrypted");

    case.shutdown().await;
}

// ---------------------------------------------------------------------------
// The AAD — what a write-only column cannot prove any other way
// ---------------------------------------------------------------------------

/// Profile 4's round trip, performed by hand because **there is no reader**.
///
/// The identity is rebuilt from the row's own three columns. If the writer bound something else —
/// the document's public id, the collection id, a version number read before the supersede
/// update — the tag fails here and the column would otherwise have stayed unopenable forever,
/// discovered on the day someone finally wrote a reader.
#[tokio::test]
async fn a_sealed_document_version_opens_under_the_identity_it_claims() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    case.ingest(&document_id, REVISED).await;
    let access = case.access();

    for (version, expected) in case
        .versions(&document_id)
        .await
        .iter()
        .zip([DOCUMENT, REVISED])
    {
        let sealed = version
            .content_encrypted
            .as_ref()
            .expect("every version is sealed under this policy");
        let opened = access
            .open_content(
                sealed,
                &ContentIdentity::RagDocumentVersion {
                    version_id: version.id,
                    document_id: version.document_id,
                    version_number: version.version_number,
                },
            )
            .unwrap_or_else(|error| {
                panic!(
                    "version {} did not open under (version_id, document_id, version_number): \
                     {error:?}",
                    version.version_number
                )
            });
        assert_eq!(opened, expected);
    }

    case.shutdown().await;
}

/// The binding is load-bearing, not decoration.
///
/// Two versions of one document differ in exactly `version_id` and `version_number`. Presenting
/// one version's ciphertext under the other's identity must fail — that is the property that
/// stops an attacker with database *write* access from lifting a blob between rows.
#[tokio::test]
async fn a_sealed_document_version_refuses_a_neighbouring_rows_identity() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    case.ingest(&document_id, REVISED).await;
    let versions = case.versions(&document_id).await;
    let sealed = versions[0]
        .content_encrypted
        .as_ref()
        .expect("version 1 is sealed");

    let error = case
        .access()
        .open_content(
            sealed,
            &ContentIdentity::RagDocumentVersion {
                version_id: versions[1].id,
                document_id: versions[1].document_id,
                version_number: versions[1].version_number,
            },
        )
        .expect_err(
            "version 1's ciphertext opened under version 2's identity — the AAD binds nothing, \
             and a row's ciphertext can be moved to another row of the same table",
        );
    assert_eq!(
        error.error_response(None).error.code,
        "content_decryption_failed"
    );

    case.shutdown().await;
}

/// The same property for profile 5, and specifically for `chunk_index`.
///
/// `chunk_index` is the one field of profile 5 whose stability had to be established rather than
/// assumed: it is bound into the AAD, and a value that could change after write would make every
/// sealed chunk unreadable on the day it changed. It is stable because `rag_chunks` is
/// insert-only — a re-ingest writes a **new** version with new chunk rows and supersedes the old
/// one rather than renumbering it. This asserts the other half: that the bound value is really
/// bound, so the stability actually matters.
#[tokio::test]
async fn a_sealed_chunk_refuses_a_different_chunk_index() {
    let Some(case) = Case::new().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    let chunk = &case.chunks(&document_id).await[0];
    let sealed = chunk
        .chunk_text_encrypted
        .as_ref()
        .expect("the chunk is sealed");
    let access = case.access();

    assert_eq!(
        access
            .open_content(
                sealed,
                &ContentIdentity::RagChunk {
                    chunk_id: chunk.id,
                    document_version_id: chunk.document_version_id,
                    chunk_index: chunk.chunk_index,
                },
            )
            .expect("the chunk opens under its own identity"),
        DOCUMENT
    );

    let error = access
        .open_content(
            sealed,
            &ContentIdentity::RagChunk {
                chunk_id: chunk.id,
                document_version_id: chunk.document_version_id,
                chunk_index: chunk.chunk_index + 1,
            },
        )
        .expect_err(
            "the chunk opened under a different chunk_index — the index is not in the AAD, so \
             the profile binds less than the registry says it does",
        );
    assert_eq!(
        error.error_response(None).error.code,
        "content_decryption_failed"
    );

    case.shutdown().await;
}

// ---------------------------------------------------------------------------
// Profile 5 end to end
// ---------------------------------------------------------------------------

/// The assertion the hand-built identities above cannot make.
///
/// Everything else in this file would still pass if `chunk_candidate_from_row` opened the column
/// with the *wrong* identity, or did not open it at all — a sealed collection would simply
/// retrieve as a page of textless chunks, silently, which is the failure mode this whole read
/// wiring exists to prevent. This drives a real turn and asserts the body reached the assembled
/// context.
#[tokio::test]
async fn a_sealed_chunk_reaches_the_model_through_the_retrieval_path() {
    let Some(case) = Case::with_embeddings().await else {
        return;
    };
    case.set_policy(ConversationContentPersistence::EncryptedContent)
        .await;

    let document_id = case.create_document(DOCUMENT).await;
    let chunks = case.chunks(&document_id).await;
    assert!(
        chunks[0].chunk_text_encrypted.is_some() && chunks[0].chunk_text_plain.is_none(),
        "the premise: the chunk retrieval is about to serve is sealed, not plaintext"
    );

    let link = tokio::time::timeout(
        WAIT,
        case.service().prepare_response_conversation(
            &case.caller,
            &request_context(),
            Uuid::now_v7(),
            None,
            Some(&ResponseConversationInput {
                id: None,
                create: true,
                title: Some("canary".to_string()),
                metadata: json!({}),
            }),
            &[PublicInputMessage {
                role: PublicMessageRole::User,
                content: vec![PublicContentPart::InputText {
                    text: QUERY.to_string(),
                }],
            }],
        ),
    )
    .await
    .expect("prepare_response_conversation timed out")
    .expect("prepare_response_conversation")
    .expect("a conversation was requested, so a link must come back");

    assert!(
        !link.context.citations.is_empty(),
        "retrieval returned no chunk at all, so this test would prove nothing about opening one"
    );
    let assembled = link
        .context
        .messages
        .iter()
        .map(|message| format!("{message:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        assembled.contains(DOCUMENT),
        "the sealed chunk did not reach the assembled context. Either the retrieval query stopped \
         projecting chunk_text_encrypted, or chunk_candidate_from_row stopped opening it — either \
         way a collection under `encrypted_content` serves textless chunks and says nothing. \
         Assembled context was:\n{assembled}"
    );

    case.shutdown().await;
}

// ---------------------------------------------------------------------------
// The schema guarantee the read-side precedence rests on
// ---------------------------------------------------------------------------

/// The five exclusivity constraints, asserted against `pg_constraint`.
///
/// `conversation_content_from_row` resolves `(plain, encrypted)` by treating nullness as the
/// discriminator, and `docs/security.md` promises a partially-sealed table stays unambiguous
/// **forever** on the strength of these five `CHECK`s. A later migration that dropped one — or
/// added a column to a sixth table and forgot its constraint — would leave that promise resting
/// on nothing, with no test failing and no error anywhere.
///
/// It asserts three things per constraint and each catches a different regression: that the
/// constraint exists at all (`DROP CONSTRAINT`), that it is still a `CHECK` (replaced by
/// something weaker), and that it is `convalidated` (re-added `NOT VALID` and never scanned, so
/// existing rows were never checked).
#[tokio::test]
async fn the_five_content_exclusivity_constraints_exist_and_are_validated() {
    let Some(case) = Case::new().await else {
        return;
    };

    // (table, constraint, the two columns it makes exclusive)
    let expected: [(&str, &str, &str, &str); 5] = [
        (
            "conversation_messages",
            "conversation_messages_content_single_form",
            "content_plain",
            "content_encrypted",
        ),
        (
            "conversation_summaries",
            "conversation_summaries_summary_single_form",
            "summary_text_plain",
            "summary_text_encrypted",
        ),
        (
            "memory_records",
            "memory_records_content_single_form",
            "content_plain",
            "content_encrypted",
        ),
        (
            "rag_document_versions",
            "rag_document_versions_content_single_form",
            "content_plain",
            "content_encrypted",
        ),
        (
            "rag_chunks",
            "rag_chunks_chunk_text_single_form",
            "chunk_text_plain",
            "chunk_text_encrypted",
        ),
    ];

    for (table, constraint, plain, encrypted) in expected {
        let row = sqlx::query(
            "select c.contype::text as contype, \
                    c.convalidated as validated, \
                    pg_get_constraintdef(c.oid) as definition \
             from pg_constraint c \
             join pg_class t on t.oid = c.conrelid \
             join pg_namespace n on n.oid = t.relnamespace \
             where n.nspname = current_schema() and t.relname = $1 and c.conname = $2",
        )
        .bind(table)
        .bind(constraint)
        .fetch_optional(&case.fixture.pool)
        .await
        .expect("query pg_constraint")
        .unwrap_or_else(|| {
            panic!(
                "{table} has no constraint named {constraint}. The read-side precedence in \
                 conversation_content_from_row treats nullness as the discriminator, and \
                 docs/security.md promises a partially-sealed table stays unambiguous because of \
                 this constraint. Dropping it makes both untrue silently."
            )
        });

        let contype: String = row.try_get("contype").expect("contype");
        assert_eq!(
            contype, "c",
            "{constraint} is no longer a CHECK constraint (pg_constraint.contype = {contype:?})"
        );
        let validated: bool = row.try_get("validated").expect("convalidated");
        assert!(
            validated,
            "{constraint} exists but is NOT VALID, so pre-existing rows were never checked and a \
             row holding both a plaintext and a sealed body can be sitting in {table} right now. \
             Migration 0027 adds it NOT VALID and then validates it in a separate transaction; a \
             migration that re-added it must do the same."
        );
        let definition: String = row.try_get("definition").expect("definition");
        assert!(
            definition.contains(plain) && definition.contains(encrypted),
            "{constraint} no longer mentions both {plain} and {encrypted}: {definition}"
        );
    }

    case.shutdown().await;
}
