//! Ingest → retrieve → plan → cite, over real HTTP — plan 11 Sub-Phases C, D and G.
//!
//! This is the file that closes P0-1's second half. `tests/rag_ingestion_honesty.rs` already
//! proves a document really produces `rag_chunks` and `rag_chunk_embeddings`; this proves those
//! rows then reach the model's context on a `POST /api/v1/responses` call and come back as
//! `citations` that are **not** unconditionally empty.
//!
//! # Why the assertions are three-way
//!
//! A citation test that only counts is worthless — "there are two citations" is satisfied by a
//! `Vec` of the wrong two. So every case here cross-checks three independent records:
//!
//! 1. the `citations` array on the HTTP response,
//! 2. the `context_plans.included_chunk_ids` row written during the request, and
//! 3. the `rag_chunks` rows those ids name.
//!
//! A citation that does not correspond to a retrieved chunk fails (2); a plan that records ids
//! the response does not cite fails (1); an id that names no real chunk fails (3).

mod support;

use std::{collections::HashMap, f64::consts::PI, time::Duration};

use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy,
    mock_openai::{EmbeddingBehaviour, MockOpenAiServer, ProviderScript, planar_vector},
    request_context,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

/// The user turn. Embedded at angle 0.
const QUERY: &str = "how long do we keep customer records";
/// The document body, embedded at angle 0 too — a perfect match, so retrieval is not left to
/// the mock's hash function.
const DOCUMENT: &str = "customer records are retained for ninety days after closure";
/// A second document with no relation to the query, at 90 degrees — orthogonal, so it is a
/// candidate but scores exactly at the "no better than unrelated" mark.
const UNRELATED: &str = "the office coffee machine is descaled every second tuesday";

const ASSISTANT_REPLY: &str = "Ninety days.";

fn embedding_behaviour() -> EmbeddingBehaviour {
    EmbeddingBehaviour::Fixed {
        vectors: HashMap::from([
            (QUERY.to_string(), planar_vector(0.0)),
            (DOCUMENT.to_string(), planar_vector(0.0)),
            (UNRELATED.to_string(), planar_vector(PI / 2.0)),
        ]),
    }
}

struct Case {
    fixture: LifecycleFixture,
    completion: MockOpenAiServer,
    embeddings: MockOpenAiServer,
    moira: MoiraHttpServer,
    consumer_key: String,
    client: reqwest::Client,
}

impl Case {
    async fn new(scripts: Vec<ProviderScript>) -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let completion = MockOpenAiServer::start(scripts).await;
        fixture
            .add_provider(completion.base_url(), 10, RuntimePolicy::default())
            .await;
        let embeddings = MockOpenAiServer::start(Vec::new()).await;
        embeddings
            .set_embedding_behaviour(embedding_behaviour())
            .await;
        fixture
            .enable_rag_embeddings(embeddings.base_url(), "text-embedding-3-small")
            .await;
        fixture
            .enable_retrieval(moira::domain::RetrievalPolicyPutRequest::default())
            .await;
        let consumer_key = fixture.enable_public_streaming().await;
        let moira = MoiraHttpServer::start(fixture.state.clone()).await;
        Some(Self {
            fixture,
            completion,
            embeddings,
            moira,
            consumer_key,
            client: reqwest::Client::new(),
        })
    }

    /// Ingests one document through the real admin HTTP surface.
    async fn ingest(&self, label: &str, content: &str) -> String {
        let suffix = Uuid::now_v7().simple().to_string();
        let collection: Value = self
            .admin_post(
                "/api/v1/admin/rag-collections",
                json!({
                    "application_id": self.fixture.application_id,
                    "external_tenant_id": null,
                    "collection_key": format!("{label}-{suffix}"),
                    "display_name": format!("{label} {suffix}"),
                    "description": null,
                    "visibility": "application",
                    "metadata": {}
                }),
                StatusCode::CREATED,
            )
            .await;
        let collection_id = collection["id"]
            .as_str()
            .expect("collection id")
            .to_string();
        let document: Value = self
            .admin_post(
                &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
                json!({
                    "external_document_id": format!("{label}-{suffix}"),
                    "title": format!("{label} handbook"),
                    "source_type": "direct_text",
                    "source_uri": null,
                    "mime_type": "text/plain",
                    "content": null,
                    "metadata": {}
                }),
                StatusCode::CREATED,
            )
            .await;
        let document_id = document["id"].as_str().expect("document id").to_string();
        self.admin_post::<Value>(
            &format!("/api/v1/admin/rag-documents/{document_id}/ingest"),
            json!({ "content": content }),
            StatusCode::OK,
        )
        .await;
        document_id
    }

    async fn admin_post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: Value,
        expected: StatusCode,
    ) -> T {
        let response = tokio::time::timeout(
            WAIT,
            self.client
                .post(format!("{}{path}", self.moira.base_url))
                .header("x-request-id", format!("p11-{}", Uuid::now_v7()))
                .json(&body)
                .send(),
        )
        .await
        .expect("admin request timed out")
        .expect("admin request");
        let status = response.status();
        let text = response.text().await.expect("admin response body");
        assert_eq!(status, expected, "{path} returned {status}: {text}");
        serde_json::from_str(&text).expect("admin JSON response")
    }

    /// Issues a `POST /api/v1/responses` as the consumer key, returning `(status, body)`.
    async fn respond(&self, text: &str) -> (StatusCode, Value) {
        let response = tokio::time::timeout(
            WAIT,
            self.client
                .post(format!("{}/api/v1/responses", self.moira.base_url))
                .header("x-consumer-key", &self.consumer_key)
                .header("x-request-id", format!("p11-resp-{}", Uuid::now_v7()))
                .json(&json!({
                    "model": null,
                    "route": self.fixture.route_key,
                    "input": [{
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }]
                    }],
                    "conversation": { "create": true, "title": "retrieval e2e" },
                    "metadata": {}
                }))
                .send(),
        )
        .await
        .expect("responses request timed out")
        .expect("responses request");
        let status = response.status();
        let body = response.text().await.expect("responses body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
        )
    }

    async fn shutdown(self) {
        self.completion.shutdown().await;
        self.embeddings.shutdown().await;
        self.moira.shutdown().await;
    }
}

/// The headline case: ingest, respond, and prove the citations name the chunks the plan says
/// were included, and that those chunks are real rows containing the ingested text.
#[tokio::test]
async fn ingest_then_respond_populates_citations_from_real_provenance() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: ASSISTANT_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };
    case.ingest("retention", DOCUMENT).await;

    let (status, body) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "response failed: {body}");

    let citations = body["citations"].as_array().expect("citations array");
    assert!(
        !citations.is_empty(),
        "citations must no longer be unconditionally empty: {body}"
    );
    assert!(
        citations
            .iter()
            .all(|citation| citation["type"] == "rag_chunk"),
        "unexpected citation type: {citations:?}"
    );

    // (2) The plan row written during this request.
    let execution_id: Uuid = body["execution_id"]
        .as_str()
        .expect("execution_id")
        .trim_start_matches("exec_")
        .parse()
        .expect("execution uuid");
    let included: Vec<Uuid> = sqlx::query_scalar(
        "select unnest(included_chunk_ids) from context_plans where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_all(&case.fixture.pool)
    .await
    .expect("a context_plans row must exist for a retrieval-backed execution");
    assert_eq!(
        included.len(),
        citations.len(),
        "the plan and the response disagree on how many chunks were used"
    );

    // (3) Those ids name real chunks, and the cited public ids are exactly those chunks'.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select public_id, coalesce(chunk_text_plain, '') from rag_chunks where id = any($1)",
    )
    .bind(&included)
    .fetch_all(&case.fixture.pool)
    .await
    .expect("resolve the planned chunk ids");
    assert_eq!(
        rows.len(),
        included.len(),
        "the plan names chunk ids that do not exist"
    );
    let mut cited: Vec<String> = citations
        .iter()
        .map(|citation| citation["id"].as_str().expect("citation id").to_string())
        .collect();
    let mut planned: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    cited.sort();
    planned.sort();
    assert_eq!(
        cited, planned,
        "every citation must name a chunk the plan actually included, and vice versa"
    );
    assert!(
        rows.iter().any(|(_, text)| text == DOCUMENT),
        "the cited chunk does not contain the ingested document text"
    );

    // The document's own title and the collection's section survive into the citation.
    let citation = &citations[0];
    assert_eq!(citation["title"], "retention handbook");
    assert!(citation["document_id"].is_string());
    assert!(citation["memory_id"].is_null());
    case.shutdown().await;
}

/// The retrieved text has to reach the provider, not merely the `context_plans` row.
///
/// Asserted against the request body the mock provider actually received — the only place that
/// cannot be satisfied by bookkeeping.
#[tokio::test]
async fn retrieved_chunk_text_reaches_the_provider_request() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: ASSISTANT_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };
    case.ingest("retention", DOCUMENT).await;
    let (status, body) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let requests = case.completion.requests().await;
    assert_eq!(requests.len(), 1, "exactly one completion call is expected");
    let messages = requests[0].body["messages"]
        .as_array()
        .expect("provider messages")
        .clone();
    let serialised = serde_json::to_string(&messages).expect("serialise messages");
    assert!(
        serialised.contains(DOCUMENT),
        "the retrieved chunk never reached the provider: {serialised}"
    );

    // Structural separation, at the HTTP level: nothing retrieved may be a system message.
    for message in &messages {
        if message["role"] == "system" || message["role"] == "developer" {
            let content = serde_json::to_string(&message["content"]).expect("content");
            assert!(
                !content.contains(DOCUMENT),
                "retrieved content reached an instruction-role message: {content}"
            );
        }
    }
    // And the caller's own turn is last, after everything the planner prepended.
    let last = messages.last().expect("at least one message");
    assert_eq!(last["role"], "user");
    assert!(
        serde_json::to_string(&last["content"])
            .expect("content")
            .contains(QUERY)
    );
    case.shutdown().await;
}

/// A retrieval that clears the threshold for nothing must return `[]`, not a missing field and
/// not a fabricated entry.
#[tokio::test]
async fn a_below_threshold_retrieval_yields_an_empty_citation_array() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: ASSISTANT_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };
    // Only the unrelated document exists, and it is orthogonal to the query — semantic score
    // exactly 0.5. With `minimum_chunk_score` at 0.5 and a strictly-greater comparison it is
    // excluded, which also pins the `>` vs `>=` decision at the boundary end to end.
    case.fixture
        .enable_retrieval(moira::domain::RetrievalPolicyPutRequest {
            minimum_chunk_score: Some(0.5),
            ..moira::domain::RetrievalPolicyPutRequest::default()
        })
        .await;
    case.ingest("coffee", UNRELATED).await;

    let (status, body) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["citations"],
        json!([]),
        "an empty retrieval must serialise as [] and never as null"
    );

    // The retrieval still happened and is still recorded — the candidate was found and then
    // scored out, which is a different fact from "retrieval did not run".
    let execution_id: Uuid = body["execution_id"]
        .as_str()
        .expect("execution_id")
        .trim_start_matches("exec_")
        .parse()
        .expect("execution uuid");
    let row: (i32, i32) = sqlx::query_as(
        "select chunk_candidate_count, chunk_returned_count from retrieval_runs where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("a retrieval_runs row");
    assert_eq!(row.0, 1, "the candidate was found");
    assert_eq!(row.1, 0, "and then scored out");
    case.shutdown().await;
}

/// Content ingested before this pipeline existed has no chunks, so retrieval finds nothing —
/// the backward-compatibility claim, asserted rather than assumed.
#[tokio::test]
async fn a_document_with_no_chunks_produces_no_citations() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: ASSISTANT_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };
    let document_id = case.ingest("retention", DOCUMENT).await;
    // Simulate a pre-pipeline row: the version exists, the chunks do not.
    sqlx::query(
        "delete from rag_chunks where document_version_id in \
         (select v.id from rag_document_versions v join rag_documents d on d.id = v.document_id \
          where d.public_id = $1)",
    )
    .bind(&document_id)
    .execute(&case.fixture.pool)
    .await
    .expect("strip the chunks");

    let (status, body) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["citations"], json!([]));
    case.shutdown().await;
}

/// Retrieval off is the default, and it must stay a no-op — no plan row, no retrieval row, no
/// embedding call.
#[tokio::test]
async fn retrieval_disabled_writes_no_provenance_and_calls_no_embedding_provider() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: ASSISTANT_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };
    case.ingest("retention", DOCUMENT).await;
    let embedding_calls_after_ingest = case.embeddings.embedding_requests().await.len();
    case.fixture
        .enable_retrieval(moira::domain::RetrievalPolicyPutRequest {
            enabled: Some(false),
            ..moira::domain::RetrievalPolicyPutRequest::default()
        })
        .await;

    let (status, body) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["citations"], json!([]));

    let execution_id: Uuid = body["execution_id"]
        .as_str()
        .expect("execution_id")
        .trim_start_matches("exec_")
        .parse()
        .expect("execution uuid");
    let plans: i64 =
        sqlx::query_scalar("select count(*) from context_plans where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count plans");
    let runs: i64 =
        sqlx::query_scalar("select count(*) from retrieval_runs where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count runs");
    assert_eq!(
        plans, 0,
        "a disabled planner must write no context_plans row"
    );
    assert_eq!(
        runs, 0,
        "a disabled planner must write no retrieval_runs row"
    );
    assert_eq!(
        case.embeddings.embedding_requests().await.len(),
        embedding_calls_after_ingest,
        "a disabled planner must not embed the query"
    );
    case.shutdown().await;
}

/// The second turn of a conversation replays the first, so history really is loaded and really
/// is recorded on the plan.
#[tokio::test]
async fn a_second_turn_replays_the_first_turn_as_planned_history() {
    let Some(case) = Case::new(vec![
        ProviderScript::Completion {
            text: ASSISTANT_REPLY.to_string(),
        },
        ProviderScript::Completion {
            text: "Still ninety days.".to_string(),
        },
    ])
    .await
    else {
        return;
    };
    case.ingest("retention", DOCUMENT).await;

    let (status, first) = case.respond(QUERY).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let conversation_id = first["conversation"]["id"]
        .as_str()
        .expect("conversation id")
        .to_string();

    let response = tokio::time::timeout(
        WAIT,
        case.client
            .post(format!("{}/api/v1/responses", case.moira.base_url))
            .header("x-consumer-key", &case.consumer_key)
            .header("x-request-id", format!("p11-turn2-{}", Uuid::now_v7()))
            .json(&json!({
                "route": case.fixture.route_key,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": QUERY }]
                }],
                "conversation": { "id": conversation_id, "create": false },
                "metadata": {}
            }))
            .send(),
    )
    .await
    .expect("second turn timed out")
    .expect("second turn");
    assert_eq!(response.status(), StatusCode::OK);
    let second: Value = response.json().await.expect("second turn body");

    let execution_id: Uuid = second["execution_id"]
        .as_str()
        .expect("execution_id")
        .trim_start_matches("exec_")
        .parse()
        .expect("execution uuid");
    let planned_messages: Vec<Uuid> = sqlx::query_scalar(
        "select unnest(included_message_ids) from context_plans where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_all(&case.fixture.pool)
    .await
    .expect("plan row");
    assert_eq!(
        planned_messages.len(),
        2,
        "the first turn's user message and the assistant reply must both be replayed"
    );

    // The current turn is excluded from history: the plan must not name the message the
    // request itself just wrote, or the model sees the same turn twice.
    let current_turn: i64 = sqlx::query_scalar(
        "select count(*) from conversation_messages where id = any($1) and metadata->>'source' = 'response_request' \
         and sequence_number = (select max(sequence_number) from conversation_messages m2 \
                                join conversations c on c.id = m2.conversation_id where c.public_id = $2)",
    )
    .bind(&planned_messages)
    .bind(&conversation_id)
    .fetch_one(&case.fixture.pool)
    .await
    .expect("check the current turn is excluded");
    assert_eq!(
        current_turn, 0,
        "the current turn must not be replayed as history"
    );

    let requests = case.completion.requests().await;
    let serialised = serde_json::to_string(&requests[1].body["messages"]).expect("serialise");
    assert!(
        serialised.contains(ASSISTANT_REPLY),
        "the prior assistant turn did not reach the provider: {serialised}"
    );
    case.shutdown().await;
}

/// `request_context` is used by the fixture helpers; referenced here so the import is not
/// flagged when the helpers change.
#[allow(dead_code)]
fn _uses_request_context() {
    let _ = request_context();
}
