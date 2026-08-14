//! E2E coverage for the context router's MVP-static observability slice (issue #213):
//! `execution_attempts.candidate_rank`/`candidate_score`/`selection_reason` (migration 0030),
//! the `CandidateRanked` runtime event, and the `FallbackSelected` payload extension
//! (`to_provider_id`, `candidate_rank`).
//!
//! Companion to `tests/execution_lifecycle.rs`'s `pre_output_failure_falls_back_to_the_next_provider`,
//! which this file's fallback case extends with the new columns and payload fields rather than
//! duplicating the base fallback property that test already pins.
//!
//! Fail-closed behaviour is inherited from `tests/support/mod.rs` (`panic!` when `CI=true` and
//! `MOIRA_TEST_DATABASE_URL` is absent).

mod support;

use axum::http::StatusCode;
use moira::domain::{ExecutionStatus, RuntimeEventType};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use support::{
    LifecycleFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
};

struct AttemptRow {
    provider_id: Uuid,
    candidate_rank: Option<i32>,
    candidate_score: Option<f64>,
    selection_reason: Option<String>,
    status: String,
}

async fn attempts_for(fixture: &LifecycleFixture, execution_id: Uuid) -> Vec<AttemptRow> {
    let rows = sqlx::query(
        "select provider_id, candidate_rank, candidate_score, selection_reason, status \
         from execution_attempts where execution_id = $1 order by attempt_number asc",
    )
    .bind(execution_id)
    .fetch_all(&fixture.pool)
    .await
    .expect("query execution_attempts");
    rows.into_iter()
        .map(|row| AttemptRow {
            provider_id: row.get("provider_id"),
            candidate_rank: row.get("candidate_rank"),
            candidate_score: row.get("candidate_score"),
            selection_reason: row.get("selection_reason"),
            status: row.get("status"),
        })
        .collect()
}

fn candidate_ranked_payload(events: &[moira::domain::RuntimeEventEnvelope]) -> &Value {
    &events
        .iter()
        .find(|event| event.event_type == RuntimeEventType::CandidateRanked)
        .expect("a CandidateRanked event must be emitted for every execution that reaches routing")
        .payload
}

/// The single-candidate, no-hint case: rank 0, `candidate_score` unset (no scoring function
/// exists yet in this MVP-static slice), `selection_reason: "priority"` — both in the
/// `CandidateRanked` event, emitted once before the first attempt, and persisted onto the
/// `execution_attempts` row the attempt actually wrote.
#[tokio::test]
async fn single_candidate_success_is_ranked_zero_with_priority_reason() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "single-candidate-success".to_string(),
    }])
    .await;
    let provider_fixture = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let command = fixture.command(false);
    let execution_id = command.execution_id;
    let (outcome, events) = fixture
        .execution_service()
        .execute_with_events(command)
        .await
        .expect("single-candidate execution");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded, "{outcome:?}");

    let ranked = candidate_ranked_payload(&events);
    let candidates = ranked["candidates"]
        .as_array()
        .expect("CandidateRanked payload carries a `candidates` array");
    assert_eq!(candidates.len(), 1, "{ranked}");
    assert_eq!(
        candidates[0]["provider_id"],
        provider_fixture.provider_id.to_string()
    );
    assert_eq!(candidates[0]["candidate_rank"], 0);
    assert!(candidates[0]["candidate_score"].is_null());
    assert_eq!(candidates[0]["selection_reason"], "priority");

    let attempts = attempts_for(&fixture, execution_id).await;
    assert_eq!(attempts.len(), 1, "exactly one attempt for one candidate");
    assert_eq!(attempts[0].provider_id, provider_fixture.provider_id);
    assert_eq!(attempts[0].candidate_rank, Some(0));
    assert_eq!(attempts[0].candidate_score, None);
    assert_eq!(attempts[0].selection_reason.as_deref(), Some("priority"));
    assert_eq!(attempts[0].status, "succeeded");

    provider.shutdown().await;
}

/// The fallback case: the first candidate (rank 0) fails with a fallback-eligible provider
/// error, the second candidate (rank 1) is reached only because of that failure and is recorded
/// as `fallback_after_failure` — on both the `execution_attempts` rows and the extended
/// `FallbackSelected` payload's `to_provider_id`/`candidate_rank`.
#[tokio::test]
async fn fallback_candidate_is_ranked_one_with_fallback_after_failure_reason() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let primary = MockOpenAiServer::start([ProviderScript::HttpError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: "primary is down".to_string(),
    }])
    .await;
    let fallback = MockOpenAiServer::start([ProviderScript::Completion {
        text: "fallback-success".to_string(),
    }])
    .await;
    let primary_fixture = fixture
        .add_provider(primary.base_url(), 10, RuntimePolicy::default())
        .await;
    let fallback_fixture = fixture
        .add_provider(fallback.base_url(), 20, RuntimePolicy::default())
        .await;

    let command = fixture.command(false);
    let execution_id = command.execution_id;
    let (outcome, events) = fixture
        .execution_service()
        .execute_with_events(command)
        .await
        .expect("fallback execution");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded, "{outcome:?}");
    assert_eq!(outcome.output_text.as_deref(), Some("fallback-success"));

    let ranked = candidate_ranked_payload(&events);
    let candidates = ranked["candidates"]
        .as_array()
        .expect("CandidateRanked payload carries a `candidates` array");
    assert_eq!(candidates.len(), 2, "{ranked}");
    assert_eq!(
        candidates[0]["provider_id"],
        primary_fixture.provider_id.to_string()
    );
    assert_eq!(candidates[0]["candidate_rank"], 0);
    assert_eq!(candidates[0]["selection_reason"], "priority");
    assert_eq!(
        candidates[1]["provider_id"],
        fallback_fixture.provider_id.to_string()
    );
    assert_eq!(candidates[1]["candidate_rank"], 1);
    assert_eq!(candidates[1]["selection_reason"], "fallback_after_failure");

    let fallback_selected = events
        .iter()
        .find(|event| event.event_type == RuntimeEventType::FallbackSelected)
        .expect("a FallbackSelected event must be emitted when the primary candidate fails");
    assert_eq!(
        fallback_selected.payload["from_provider_id"],
        primary_fixture.provider_id.to_string()
    );
    assert_eq!(
        fallback_selected.payload["to_provider_id"],
        fallback_fixture.provider_id.to_string(),
        "the extended payload (issue #213) must name which candidate is tried next"
    );
    assert_eq!(
        fallback_selected.payload["candidate_rank"], 1,
        "the extended payload must carry the rank of the candidate being fallen back to"
    );

    let attempts = attempts_for(&fixture, execution_id).await;
    assert_eq!(attempts.len(), 2, "one attempt per candidate");
    assert_eq!(attempts[0].provider_id, primary_fixture.provider_id);
    assert_eq!(attempts[0].candidate_rank, Some(0));
    assert_eq!(attempts[0].selection_reason.as_deref(), Some("priority"));
    assert_eq!(attempts[0].status, "failed");
    assert_eq!(attempts[1].provider_id, fallback_fixture.provider_id);
    assert_eq!(attempts[1].candidate_rank, Some(1));
    assert_eq!(
        attempts[1].selection_reason.as_deref(),
        Some("fallback_after_failure")
    );
    assert_eq!(attempts[1].status, "succeeded");

    primary.shutdown().await;
    fallback.shutdown().await;
}

/// `model_hint` makes the hinted candidate `explicit_hint` regardless of its rank in the
/// underlying priority order — the routing filter (`DefaultModelRouter::select_candidates`)
/// retains only the hinted candidate, so it always lands at rank 0 here, but the *reason* must
/// say `explicit_hint`, not `priority`, because an unauthorized caller's hint is what
/// `moira:execution:override-model` gates.
#[tokio::test]
async fn explicit_model_hint_is_recorded_as_explicit_hint_not_priority() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "hinted".to_string(),
    }])
    .await;
    let provider_fixture = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let mut command = fixture.command(false);
    let execution_id = command.execution_id;
    command.model_hint = Some(provider_fixture.model_id);

    let (outcome, events) = fixture
        .execution_service()
        .execute_with_events(command)
        .await
        .expect("model-hinted execution");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded, "{outcome:?}");

    let ranked = candidate_ranked_payload(&events);
    let candidates = ranked["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates[0]["selection_reason"], "explicit_hint");

    let attempts = attempts_for(&fixture, execution_id).await;
    assert_eq!(
        attempts[0].selection_reason.as_deref(),
        Some("explicit_hint")
    );

    provider.shutdown().await;
}
