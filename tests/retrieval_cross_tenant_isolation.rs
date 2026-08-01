//! Cross-application / tenant / user retrieval isolation — plan 11 Sub-Phase C.
//!
//! **This is the security-critical suite of plan 11.** The property under test is that the
//! isolation predicate is evaluated *inside* the vector query, in the same statement as the
//! `order by <=>`, and never as a post-fetch filter in Rust.
//!
//! # How each case is made adversarial
//!
//! Every case seeds the *other* scope with content whose raw cosine similarity to the query is
//! **strictly higher** than anything in the caller's own scope. So:
//!
//! * A correct implementation returns the caller's own, worse-scoring row.
//! * An implementation that filtered after fetching top-K would still pass a naive
//!   "did I get my own row" test, but would have had to fetch the other scope's row first.
//! * An implementation with a **missing** filter returns the other scope's row, because it
//!   sorts first — which is exactly what these assertions catch.
//!
//! The vectors are hand-chosen ([`planar_vector`]) rather than hashed, so "strictly higher" is
//! arithmetic rather than a property of a hash function that could quietly change.
//!
//! # Why this drives `ConversationService` rather than HTTP
//!
//! The scope is three bound parameters — `application_id`, `external_tenant_id`,
//! `external_user_id` — derived from the acting `Actor`. A consumer-key actor carries only the
//! first: `authenticate_caller` returns from `verify_api_key` before any `x-moira-*` header is
//! read, so there is no HTTP path that attaches a tenant or a user to one without standing up a
//! full trusted-JWT issuer with signed tokens per case. Constructing the `Actor` directly tests
//! the predicate exactly, against real PostgreSQL and real pgvector; `tests/rag_retrieval_end_to_end.rs`
//! covers the HTTP path that populates those fields.

mod support;

use std::{collections::HashMap, f64::consts::PI};

use moira::{
    application::ConversationService,
    domain::{
        EmbeddingPolicyPutRequest, MemoryCreateRequest, MemoryType, PublicContentPart,
        PublicInputMessage, PublicMessageRole, RagCollectionCreateRequest, RagCollectionVisibility,
        RagDocumentCreateRequest, RagDocumentIngestRequest, ResponseConversationInput,
        RetrievalPolicyPutRequest,
    },
    security::Actor,
};
use serde_json::json;
use support::{
    LifecycleFixture, mock_openai::EmbeddingBehaviour, mock_openai::MockOpenAiServer,
    mock_openai::planar_vector, request_context,
};
use uuid::Uuid;

/// The text every case queries with. Embedded at angle 0.
const QUERY: &str = "what is the retention window";

/// The caller's own content: a *worse* match than the other scope's, deliberately.
const OWN_TEXT: &str = "our own scope holds a merely adequate answer";
/// The other scope's content: a *better* match, deliberately.
const OTHER_TEXT: &str = "the other scope holds a perfect answer";

/// Query at 0, own content at 60 degrees (cosine 0.5), other content at 0 (cosine 1.0).
///
/// So the other scope's row is the global nearest neighbour by a wide margin. If it is ever
/// returned, the filter is not in the query.
fn fixed_vectors() -> EmbeddingBehaviour {
    EmbeddingBehaviour::Fixed {
        vectors: HashMap::from([
            (QUERY.to_string(), planar_vector(0.0)),
            (OWN_TEXT.to_string(), planar_vector(PI / 3.0)),
            (OTHER_TEXT.to_string(), planar_vector(0.0)),
        ]),
    }
}

struct Case {
    provider: MockOpenAiServer,
    fixture: LifecycleFixture,
    embedding_provider: Uuid,
    embedding_model: Uuid,
}

impl Case {
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let provider = MockOpenAiServer::start(Vec::new()).await;
        provider.set_embedding_behaviour(fixed_vectors()).await;
        let embedding = fixture
            .enable_rag_embeddings(provider.base_url(), "text-embedding-3-small")
            .await;
        fixture
            .patch_embedding_policy(EmbeddingPolicyPutRequest {
                memory_embeddings_enabled: Some(true),
                ..EmbeddingPolicyPutRequest::default()
            })
            .await;
        fixture
            .enable_retrieval(RetrievalPolicyPutRequest::default())
            .await;
        fixture.enable_manual_memory().await;
        Some(Self {
            provider,
            fixture,
            embedding_provider: embedding.provider_id,
            embedding_model: embedding.model_id,
        })
    }

    fn service(&self) -> ConversationService {
        ConversationService::new(&self.fixture.state).expect("conversation service")
    }

    /// Runs one turn for `actor` and returns the plan's provenance.
    async fn plan_for(&self, actor: &Actor) -> Planned {
        let execution_id = Uuid::now_v7();
        let link = self
            .service()
            .prepare_response_conversation(
                actor,
                &request_context(),
                execution_id,
                // No route hint: this suite drives the planner directly and never reaches the
                // extraction path, which is the only consumer of it.
                None,
                Some(&ResponseConversationInput {
                    create: true,
                    id: None,
                    title: None,
                    metadata: json!({}),
                }),
                &[PublicInputMessage {
                    role: PublicMessageRole::User,
                    content: vec![PublicContentPart::InputText {
                        text: QUERY.to_string(),
                    }],
                }],
            )
            .await
            .expect("plan a turn")
            .expect("a conversation link");
        Planned {
            citation_ids: link
                .context
                .citations
                .iter()
                .map(|citation| citation.id.clone())
                .collect(),
            assembled_text: link
                .context
                .messages
                .iter()
                .filter_map(|message| message.first_text().map(str::to_string))
                .collect::<Vec<_>>()
                .join("\n"),
            execution_id,
        }
    }

    /// Creates a memory owned by `actor`'s scope.
    async fn seed_memory(&self, actor: &Actor, content: &str) -> String {
        self.service()
            .create_memory(
                actor,
                &request_context(),
                MemoryCreateRequest {
                    memory_type: MemoryType::Fact,
                    content: content.to_string(),
                    importance: None,
                    confidence: None,
                    valid_until: None,
                    metadata: json!({}),
                },
            )
            .await
            .expect("create memory")
            .id
    }

    /// Creates a collection + document + ingested version under `application_id`.
    async fn seed_document(
        &self,
        application_id: Uuid,
        tenant: Option<&str>,
        visibility: RagCollectionVisibility,
        content: &str,
    ) -> (Uuid, String) {
        let admin = self.fixture.actor.clone();
        let service = self.service();
        let suffix = Uuid::now_v7().simple().to_string();
        let collection = service
            .create_rag_collection(
                &admin,
                &request_context(),
                RagCollectionCreateRequest {
                    application_id,
                    external_tenant_id: tenant.map(str::to_string),
                    collection_key: format!("c-{suffix}"),
                    display_name: format!("C {suffix}"),
                    description: None,
                    visibility,
                    metadata: json!({}),
                },
            )
            .await
            .expect("create collection");
        let document = service
            .create_rag_document(
                &admin,
                &request_context(),
                &collection.id,
                RagDocumentCreateRequest {
                    external_document_id: Some(format!("d-{suffix}")),
                    title: format!("Doc {suffix}"),
                    source_type: "direct_text".to_string(),
                    source_uri: None,
                    mime_type: "text/plain".to_string(),
                    content: None,
                    metadata: json!({}),
                },
            )
            .await
            .expect("create document");
        service
            .ingest_rag_document(
                &admin,
                &request_context(),
                &document.id,
                RagDocumentIngestRequest {
                    content: Some(content.to_string()),
                    source_etag: None,
                    source_last_modified: None,
                    metadata: json!({}),
                },
            )
            .await
            .expect("ingest document");
        let collection_uuid: Uuid =
            sqlx::query_scalar("select id from rag_collections where public_id = $1")
                .bind(&collection.id)
                .fetch_one(&self.fixture.pool)
                .await
                .expect("collection uuid");
        (collection_uuid, document.id)
    }

    /// Creates a second application, its own embedding + retrieval policy, and returns its id.
    async fn second_application(&self) -> Uuid {
        let suffix = Uuid::now_v7().simple().to_string();
        let application = moira::application::AdminService::new(&self.fixture.state)
            .expect("admin service")
            .create_application(
                &self.fixture.actor,
                &request_context(),
                moira::domain::ApplicationCreateRequest {
                    external_application_id: Some(format!("other-{suffix}")),
                    application_slug: Some(format!("other-{suffix}")),
                    display_name: format!("Other {suffix}"),
                    metadata: json!({}),
                },
            )
            .await
            .expect("create second application");
        // The other application needs its own embedding policy, pointing at the same mock.
        // Without it `plan_rag_ingestion` finds no policy, stores chunks with no vectors, and
        // the "the other application scores higher" premise silently evaporates — the
        // isolation assertion would then pass against an implementation with no application
        // filter at all. `assert_is_indexed` below turns that from a comment into a check.
        self.service()
            .put_embedding_policy(
                &self.fixture.actor,
                &request_context(),
                application.id,
                EmbeddingPolicyPutRequest {
                    embedding_provider_id: Some(self.embedding_provider),
                    embedding_model_id: Some(self.embedding_model),
                    embedding_dimension: Some(1536),
                    batch_size: Some(2),
                    rag_embeddings_enabled: Some(true),
                    memory_embeddings_enabled: Some(true),
                    ..EmbeddingPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable embeddings for the second application");
        application.id
    }

    /// Asserts that `content` is genuinely in the vector index.
    ///
    /// Every adversarial case in this file rests on the out-of-scope row being a *better* match
    /// than the caller's own. A row with no embedding is not a match at all, so an unasserted
    /// premise here would turn each isolation test into a tautology.
    async fn assert_chunk_is_indexed(&self, content: &str) {
        let indexed: i64 = sqlx::query_scalar(
            "select count(*) from rag_chunk_embeddings e join rag_chunks c on c.id = e.chunk_id \
             where c.chunk_text_plain = $1 and e.embedding is not null",
        )
        .bind(content)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("count indexed chunks");
        assert!(
            indexed > 0,
            "premise failed: {content:?} has no embedding, so it could never have been a \
             retrieval candidate and this isolation assertion proves nothing"
        );
    }

    async fn assert_memory_is_indexed(&self, public_id: &str) {
        let indexed: i64 = sqlx::query_scalar(
            "select count(*) from memory_embeddings e join memory_records m on m.id = e.memory_id \
             where m.public_id = $1 and e.embedding is not null",
        )
        .bind(public_id)
        .fetch_one(&self.fixture.pool)
        .await
        .expect("count indexed memories");
        assert!(
            indexed > 0,
            "premise failed: {public_id} has no embedding, so this isolation assertion proves \
             nothing"
        );
    }

    async fn shutdown(self) {
        self.provider.shutdown().await;
    }
}

struct Planned {
    citation_ids: Vec<String>,
    assembled_text: String,
    execution_id: Uuid,
}

impl Planned {
    /// Asserts the plan is free of a specific piece of content **and** of a specific id.
    ///
    /// Both, deliberately: a citation list that merely has the right *length* proves nothing,
    /// and text that merely lacks a substring could still carry the id in provenance.
    fn must_not_contain(&self, forbidden_id: &str, forbidden_text: &str) {
        assert!(
            !self.citation_ids.iter().any(|id| id == forbidden_id),
            "an out-of-scope row was cited: {forbidden_id} in {:?}",
            self.citation_ids
        );
        assert!(
            !self.assembled_text.contains(forbidden_text),
            "out-of-scope content reached the assembled context:\n{}",
            self.assembled_text
        );
    }

    fn must_contain(&self, expected_id: &str, expected_text: &str) {
        assert!(
            self.citation_ids.iter().any(|id| id == expected_id),
            "the in-scope row was not cited: {expected_id} in {:?}",
            self.citation_ids
        );
        assert!(
            self.assembled_text.contains(expected_text),
            "in-scope content did not reach the assembled context:\n{}",
            self.assembled_text
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Memory isolation
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn memory_user_isolation_holds_even_when_the_other_user_scores_higher() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));
    let theirs = case.fixture.caller_actor(Some("tenant-a"), Some("user-b"));

    let own_id = case.seed_memory(&mine, OWN_TEXT).await;
    let other_id = case.seed_memory(&theirs, OTHER_TEXT).await;
    case.assert_memory_is_indexed(&own_id).await;
    case.assert_memory_is_indexed(&other_id).await;

    let planned = case.plan_for(&mine).await;
    planned.must_contain(&own_id, OWN_TEXT);
    planned.must_not_contain(&other_id, OTHER_TEXT);
    case.shutdown().await;
}

#[tokio::test]
async fn memory_tenant_isolation_holds_even_when_the_other_tenant_scores_higher() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));
    let theirs = case.fixture.caller_actor(Some("tenant-b"), Some("user-a"));

    let own_id = case.seed_memory(&mine, OWN_TEXT).await;
    let other_id = case.seed_memory(&theirs, OTHER_TEXT).await;
    case.assert_memory_is_indexed(&own_id).await;
    case.assert_memory_is_indexed(&other_id).await;

    let planned = case.plan_for(&mine).await;
    planned.must_contain(&own_id, OWN_TEXT);
    planned.must_not_contain(&other_id, OTHER_TEXT);
    case.shutdown().await;
}

/// The degenerate case: the caller's own scope has **zero** candidates.
///
/// The failure this catches is a fallback to the global nearest neighbour when a scoped query
/// returns nothing — which reads as "helpful" and is a cross-tenant disclosure.
#[tokio::test]
async fn an_empty_scope_returns_nothing_rather_than_the_global_nearest_neighbour() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));
    let theirs = case.fixture.caller_actor(Some("tenant-b"), Some("user-b"));

    let other_id = case.seed_memory(&theirs, OTHER_TEXT).await;
    case.assert_memory_is_indexed(&other_id).await;

    let planned = case.plan_for(&mine).await;
    planned.must_not_contain(&other_id, OTHER_TEXT);
    assert!(
        planned.citation_ids.is_empty(),
        "an empty scope must produce no citations at all, got {:?}",
        planned.citation_ids
    );
    case.shutdown().await;
}

/// `memory_scope = 'application'` is shared across tenants **on purpose**.
///
/// Pinned so the deliberate exception in `find_memory_candidates` can never become an accident:
/// if someone tightens the predicate this fails, and if someone loosens the tenant-scoped
/// predicate the two tests above fail. Both directions are covered.
#[tokio::test]
async fn application_scoped_memories_are_deliberately_shared_across_tenants() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));

    // `create_memory` always writes `user_application` scope, so the application-scoped row is
    // written directly — there is no service path that produces one today.
    let memory_id = Uuid::now_v7();
    let public_id = format!("mem_{memory_id}");
    sqlx::query(
        "insert into memory_records (id, public_id, application_id, memory_scope, memory_type, \
         content_plain, content_hash) values ($1, $2, $3, 'application', 'fact', $4, 'h')",
    )
    .bind(memory_id)
    .bind(&public_id)
    .bind(case.fixture.application_id)
    .bind(OTHER_TEXT)
    .execute(&case.fixture.pool)
    .await
    .expect("seed an application-scoped memory");
    sqlx::query(
        "insert into memory_embeddings (memory_id, embedding_version, dimension, embedding) \
         values ($1, 1, 1536, $2::vector)",
    )
    .bind(memory_id)
    .bind(encode(&planar_vector(0.0)))
    .execute(&case.fixture.pool)
    .await
    .expect("seed its embedding");

    let planned = case.plan_for(&mine).await;
    planned.must_contain(&public_id, OTHER_TEXT);
    case.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// RAG chunk isolation
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn rag_application_isolation_holds_even_when_the_other_application_scores_higher() {
    let Some(case) = Case::new().await else {
        return;
    };
    let other_application = case.second_application().await;
    let mine = case.fixture.caller_actor(None, Some("user-a"));

    case.seed_document(
        case.fixture.application_id,
        None,
        RagCollectionVisibility::Application,
        OWN_TEXT,
    )
    .await;
    case.seed_document(
        other_application,
        None,
        RagCollectionVisibility::Application,
        OTHER_TEXT,
    )
    .await;
    case.assert_chunk_is_indexed(OWN_TEXT).await;
    case.assert_chunk_is_indexed(OTHER_TEXT).await;

    let planned = case.plan_for(&mine).await;
    assert!(
        planned.assembled_text.contains(OWN_TEXT),
        "the caller's own chunk did not reach the context:\n{}",
        planned.assembled_text
    );
    assert!(
        !planned.assembled_text.contains(OTHER_TEXT),
        "another application's chunk reached the context:\n{}",
        planned.assembled_text
    );
    case.shutdown().await;
}

#[tokio::test]
async fn rag_tenant_isolation_holds_even_when_the_other_tenant_scores_higher() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));

    case.seed_document(
        case.fixture.application_id,
        Some("tenant-a"),
        RagCollectionVisibility::Tenant,
        OWN_TEXT,
    )
    .await;
    case.seed_document(
        case.fixture.application_id,
        Some("tenant-b"),
        RagCollectionVisibility::Tenant,
        OTHER_TEXT,
    )
    .await;
    case.assert_chunk_is_indexed(OWN_TEXT).await;
    case.assert_chunk_is_indexed(OTHER_TEXT).await;

    let planned = case.plan_for(&mine).await;
    assert!(planned.assembled_text.contains(OWN_TEXT));
    assert!(
        !planned.assembled_text.contains(OTHER_TEXT),
        "another tenant's collection reached the context:\n{}",
        planned.assembled_text
    );
    case.shutdown().await;
}

#[tokio::test]
async fn a_restricted_collection_is_excluded_unless_it_is_allow_listed() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(None, Some("user-a"));
    let (restricted_id, _) = case
        .seed_document(
            case.fixture.application_id,
            None,
            RagCollectionVisibility::Restricted,
            OTHER_TEXT,
        )
        .await;
    case.assert_chunk_is_indexed(OTHER_TEXT).await;

    // Same application, same tenant, best possible score — and still excluded.
    let planned = case.plan_for(&mine).await;
    assert!(
        !planned.assembled_text.contains(OTHER_TEXT),
        "a restricted collection was retrieved without being allow-listed:\n{}",
        planned.assembled_text
    );
    assert!(planned.citation_ids.is_empty());

    // Allow-list it, and now it is a candidate. Without this half the test would also pass
    // against an implementation that simply never returns restricted collections at all.
    case.fixture
        .enable_retrieval(RetrievalPolicyPutRequest {
            allowed_collection_ids: Some(vec![restricted_id]),
            ..RetrievalPolicyPutRequest::default()
        })
        .await;
    let planned = case.plan_for(&mine).await;
    assert!(
        planned.assembled_text.contains(OTHER_TEXT),
        "an allow-listed restricted collection must be retrievable:\n{}",
        planned.assembled_text
    );
    case.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Provenance must not leak counts either
// ---------------------------------------------------------------------------------------------

/// `retrieval_runs.*_candidate_count` must count only in-scope candidates.
///
/// A count is an inference channel even when no content is returned: "there are 3 documents
/// somewhere in this cluster that match your query closely" is information the caller has no
/// right to. This is the assertion that a post-fetch filter would fail even if it correctly
/// discarded the rows, because the candidate count would have been taken before the discard.
#[tokio::test]
async fn retrieval_run_counts_never_include_out_of_scope_candidates() {
    let Some(case) = Case::new().await else {
        return;
    };
    let mine = case.fixture.caller_actor(Some("tenant-a"), Some("user-a"));
    let theirs = case.fixture.caller_actor(Some("tenant-b"), Some("user-b"));

    case.seed_memory(&mine, OWN_TEXT).await;
    for _ in 0..3 {
        case.seed_memory(&theirs, OTHER_TEXT).await;
    }
    case.seed_document(
        case.second_application().await,
        None,
        RagCollectionVisibility::Application,
        OTHER_TEXT,
    )
    .await;

    let planned = case.plan_for(&mine).await;
    let row = sqlx::query_as::<_, (i32, i32, i32, i32, String)>(
        "select memory_candidate_count, memory_returned_count, chunk_candidate_count, \
         chunk_returned_count, status from retrieval_runs where execution_id = $1",
    )
    .bind(planned.execution_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("a retrieval_runs row must be written for every retrieval");

    assert_eq!(row.4, "completed");
    assert_eq!(
        row.0, 1,
        "memory_candidate_count must count only the caller's own scope"
    );
    assert_eq!(row.1, 1);
    assert_eq!(
        row.2, 0,
        "chunk_candidate_count must count only the caller's own application"
    );
    assert_eq!(row.3, 0);
    case.shutdown().await;
}

/// pgvector's text input format, duplicated here on purpose.
///
/// `moira::orchestration::encode_vector_literal` is public, but a test that seeds a fixture with
/// the same function the code under test writes with cannot catch an encoding bug — it would
/// agree with itself. Ten lines of duplication buys an independent witness.
fn encode(vector: &[f32]) -> String {
    let parts: Vec<String> = vector
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    format!("[{}]", parts.join(","))
}
