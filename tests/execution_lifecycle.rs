mod support;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use futures_util::StreamExt;
use moira::{
    application::{AdminService, ExecutionService},
    domain::{AttemptStatus, ExecutionFailureClass, ExecutionStatus, RuntimeEventType},
    error::AppError,
    orchestration::{RuntimeCacheKey, RuntimeModelHandle},
};
use serde_json::Value;
use sqlx::Row;
use support::mock_openai::{MockOpenAiServer, ProviderScript, ScriptGate};
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy, public_response_request, request_context,
};
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};
use uuid::Uuid;

/// Total execution budget the three deadline-enforcement tests give an execution.
///
/// Large enough that fixture setup (a handful of local round-trips) cannot plausibly
/// consume it — which would make the phase fail *before* entering the wrapped call site
/// and turn the test into a false pass — and small enough to keep each test around two
/// seconds.
const DEADLINE_TEST_BUDGET: Duration = Duration::from_millis(2_000);

/// Lower bound on the observed wall clock of a bounded-phase breach.
///
/// A phase that is genuinely blocked burns the whole budget. Anything materially faster
/// means the execution failed for some *other* reason and the test is not measuring the
/// deadline at all.
const DEADLINE_TEST_MINIMUM_ELAPSED: Duration = Duration::from_millis(1_200);

/// Outer guard so a regression that removes the bound fails the test instead of hanging
/// the CI job. Every gate this file installs is released before the guard's result is
/// unwrapped, so a breach of the guard cannot wedge the shared test database either.
const DEADLINE_TEST_GUARD: Duration = Duration::from_secs(20);

#[tokio::test]
async fn completion_uses_real_rig_protocol_and_encrypted_credential() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "complete".to_string(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let outcome = timeout(
        Duration::from_secs(5),
        fixture.execution_service().execute(fixture.command(false)),
    )
    .await
    .expect("completion timed out")
    .expect("completion execution failed");

    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    assert_eq!(outcome.output_text.as_deref(), Some("complete"));
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer sk-lifecycle-secret")
    );
    assert_eq!(requests[0].body["model"], "test-model");
    assert_ne!(requests[0].body["stream"], true);
    provider.shutdown().await;
}

#[tokio::test]
async fn first_stream_delta_arrives_before_provider_completion() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([ProviderScript::HeldStream {
        first_delta: "first".to_string(),
        remaining_deltas: vec!["second".to_string()],
        gate: gate.clone(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let mut handle = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start stream");

    let delta = timeout(Duration::from_secs(5), async {
        loop {
            let event = handle
                .events
                .recv()
                .await
                .expect("stream ended before delta")
                .expect("stream event failure");
            if event.event_type == RuntimeEventType::OutputTextDelta {
                return event.payload["text"].as_str().unwrap().to_string();
            }
        }
    })
    .await
    .expect("first stream delta timed out");

    assert_eq!(delta, "first");
    gate.wait_arrived().await;
    assert!(!gate.is_completed());
    gate.release();
    gate.wait_completed().await;
    while handle.events.recv().await.is_some() {}
    let outcome = (&mut handle.outcome)
        .await
        .expect("stream outcome sender dropped")
        .expect("stream execution failed");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    assert_eq!(outcome.output_text.as_deref(), Some("firstsecond"));
    drop(handle);
    provider.shutdown().await;
}

#[tokio::test]
async fn request_capacity_is_held_for_the_upstream_call_and_reused_after_completion() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::HeldCompletion {
            text: "first".to_string(),
            gate: gate.clone(),
        },
        ProviderScript::Completion {
            text: "after-release".to_string(),
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                max_concurrent_requests: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let first_service = fixture.execution_service();
    let first_command = fixture.command(false);
    let first = tokio::spawn(async move { first_service.execute(first_command).await });
    gate.wait_arrived().await;

    let rejected = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("capacity result");
    assert_eq!(rejected.status, ExecutionStatus::Failed);
    assert_eq!(
        rejected.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::CapacityExhausted)
    );
    assert_eq!(provider.call_count().await, 1);

    gate.release();
    assert_eq!(
        first
            .await
            .expect("first execution task")
            .expect("first execution result")
            .status,
        ExecutionStatus::Succeeded
    );
    let after_release = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("execution after permit release");
    assert_eq!(after_release.status, ExecutionStatus::Succeeded);
    assert_eq!(provider.call_count().await, 2);
    provider.shutdown().await;
}

#[tokio::test]
async fn stream_capacity_is_independent_from_request_capacity() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::HeldStream {
            first_delta: "held".to_string(),
            remaining_deltas: vec!["done".to_string()],
            gate: gate.clone(),
        },
        ProviderScript::Completion {
            text: "non-stream".to_string(),
        },
        ProviderScript::Stream {
            deltas: vec!["reused".to_string()],
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                max_concurrent_requests: 2,
                max_concurrent_streams: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let mut held = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start held stream");
    wait_for_delta(&mut held, "held").await;
    gate.wait_arrived().await;

    let non_stream = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("non-stream execution");
    assert_eq!(non_stream.status, ExecutionStatus::Succeeded);
    assert_eq!(provider.call_count().await, 2);

    let mut rejected_stream = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start capacity-rejected stream");
    let rejected_outcome = (&mut rejected_stream.outcome)
        .await
        .expect("capacity outcome sender")
        .expect("capacity outcome");
    assert_eq!(rejected_outcome.status, ExecutionStatus::Failed);
    assert_eq!(
        rejected_outcome
            .failure
            .as_ref()
            .map(|failure| failure.class),
        Some(ExecutionFailureClass::CapacityExhausted)
    );
    assert_eq!(provider.call_count().await, 2);
    drop(rejected_stream);

    gate.release();
    drain_stream(&mut held).await;
    let held_outcome = (&mut held.outcome)
        .await
        .expect("held outcome sender")
        .expect("held outcome");
    assert_eq!(held_outcome.status, ExecutionStatus::Succeeded);
    drop(held);

    let mut reused = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start reused stream");
    drain_stream(&mut reused).await;
    let reused_outcome = (&mut reused.outcome)
        .await
        .expect("reused outcome sender")
        .expect("reused outcome");
    assert_eq!(reused_outcome.status, ExecutionStatus::Succeeded);
    assert_eq!(provider.call_count().await, 3);
    drop(reused);
    provider.shutdown().await;
}

#[tokio::test]
async fn retry_backoff_releases_and_later_reacquires_request_capacity() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([
        ProviderScript::HttpError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: "first attempt unavailable".to_string(),
        },
        ProviderScript::Completion {
            text: "probe".to_string(),
        },
        ProviderScript::Completion {
            text: "retried".to_string(),
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                max_concurrent_requests: 1,
                retry_limit: 1,
                retry_base_delay_ms: 3_000,
                retry_max_delay_ms: 3_000,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let retry_service = fixture.execution_service();
    let mut retry_command = fixture.command(false);
    retry_command.options.timeout_ms = Some(10_000);
    let retry_execution_id = retry_command.execution_id;
    let retry = tokio::spawn(async move { retry_service.execute(retry_command).await });
    provider.wait_for_call_count(1).await;
    wait_for_attempt_status(&fixture, retry_execution_id, "failed").await;

    let probe = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("probe during retry backoff");
    assert_eq!(probe.status, ExecutionStatus::Succeeded);
    assert_eq!(probe.output_text.as_deref(), Some("probe"));

    let retried = retry
        .await
        .expect("retry task")
        .expect("retry execution result");
    assert_eq!(retried.status, ExecutionStatus::Succeeded);
    assert_eq!(retried.output_text.as_deref(), Some("retried"));
    assert_eq!(retried.attempts.len(), 2);
    assert_eq!(provider.call_count().await, 3);
    provider.shutdown().await;
}

#[tokio::test]
async fn timeout_terminalizes_attempt_and_releases_capacity() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::HeldCompletion {
            text: "too-late".to_string(),
            gate: gate.clone(),
        },
        ProviderScript::Completion {
            text: "after-timeout".to_string(),
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                request_timeout_ms: 100,
                max_concurrent_requests: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let timeout_command = fixture.command(false);
    let timeout_execution_id = timeout_command.execution_id;
    let timed_out = fixture
        .execution_service()
        .execute(timeout_command)
        .await
        .expect("timeout execution");
    assert_eq!(timed_out.status, ExecutionStatus::Failed);
    assert_eq!(
        timed_out.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::ProviderTimeout)
    );
    assert_eq!(timed_out.attempts.len(), 1);
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'failed' and failure_class = 'provider_timeout'",
                timeout_execution_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'started'",
                timeout_execution_id,
            )
            .await,
        0
    );

    let after_timeout = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("execution after timeout");
    assert_eq!(after_timeout.status, ExecutionStatus::Succeeded);
    assert_eq!(after_timeout.output_text.as_deref(), Some("after-timeout"));
    gate.release();
    provider.shutdown().await;
}

#[tokio::test]
async fn cancellation_terminalizes_attempt_and_releases_stream_capacity() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::StalledStream {
            first_delta: Some("partial".to_string()),
            gate: gate.clone(),
        },
        ProviderScript::Stream {
            deltas: vec!["after-cancel".to_string()],
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                max_concurrent_requests: 1,
                max_concurrent_streams: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let cancellation_command = fixture.command(true);
    let cancellation_execution_id = cancellation_command.execution_id;
    let mut cancelled = fixture
        .execution_service()
        .execute_stream(cancellation_command)
        .await
        .expect("start cancellable stream");
    wait_for_delta(&mut cancelled, "partial").await;
    gate.wait_arrived().await;
    cancelled.cancel();
    drain_stream(&mut cancelled).await;
    let cancelled_outcome = (&mut cancelled.outcome)
        .await
        .expect("cancelled outcome sender")
        .expect("cancelled outcome");
    assert_eq!(cancelled_outcome.status, ExecutionStatus::Cancelled);
    assert_eq!(
        cancelled_outcome
            .failure
            .as_ref()
            .map(|failure| failure.class),
        Some(ExecutionFailureClass::RequestCancelled)
    );
    assert!(
        cancelled_outcome
            .attempts
            .iter()
            .all(|attempt| attempt.status != moira::domain::AttemptStatus::Started)
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'cancelled' and failure_class = 'request_cancelled'",
                cancellation_execution_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'started'",
                cancellation_execution_id,
            )
            .await,
        0
    );
    drop(cancelled);

    let mut after_cancel = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("stream after cancellation");
    drain_stream(&mut after_cancel).await;
    let after_cancel_outcome = (&mut after_cancel.outcome)
        .await
        .expect("post-cancel outcome sender")
        .expect("post-cancel outcome");
    assert_eq!(after_cancel_outcome.status, ExecutionStatus::Succeeded);
    drop(after_cancel);
    gate.release();
    provider.shutdown().await;
}

#[tokio::test]
async fn pre_output_failure_falls_back_to_the_next_provider() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let primary = MockOpenAiServer::start([ProviderScript::HttpError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: "private primary diagnostics".to_string(),
    }])
    .await;
    let fallback = MockOpenAiServer::start([ProviderScript::Completion {
        text: "fallback-success".to_string(),
    }])
    .await;
    fixture
        .add_provider(primary.base_url(), 10, RuntimePolicy::default())
        .await;
    fixture
        .add_provider(fallback.base_url(), 20, RuntimePolicy::default())
        .await;

    let (outcome, events) = fixture
        .execution_service()
        .execute_with_events(fixture.command(false))
        .await
        .expect("fallback execution");

    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    assert_eq!(outcome.output_text.as_deref(), Some("fallback-success"));
    assert_eq!(outcome.attempts.len(), 2);
    assert!(
        events
            .iter()
            .any(|event| event.event_type == RuntimeEventType::FallbackSelected)
    );
    assert_eq!(primary.call_count().await, 1);
    assert_eq!(fallback.call_count().await, 1);
    primary.shutdown().await;
    fallback.shutdown().await;
}

#[tokio::test]
async fn post_delta_stream_failure_cannot_retry_or_fallback() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let primary = MockOpenAiServer::start([ProviderScript::StreamErrorAfterDelta {
        delta: "committed".to_string(),
    }])
    .await;
    let fallback = MockOpenAiServer::start([ProviderScript::Stream {
        deltas: vec!["must-not-run".to_string()],
    }])
    .await;
    fixture
        .add_provider(
            primary.base_url(),
            10,
            RuntimePolicy {
                retry_limit: 2,
                ..RuntimePolicy::default()
            },
        )
        .await;
    fixture
        .add_provider(fallback.base_url(), 20, RuntimePolicy::default())
        .await;

    let mut handle = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start post-delta failure stream");
    wait_for_delta(&mut handle, "committed").await;
    drain_stream(&mut handle).await;
    let outcome = (&mut handle.outcome)
        .await
        .expect("post-delta outcome sender")
        .expect("post-delta outcome");

    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(primary.call_count().await, 1);
    assert_eq!(fallback.call_count().await, 0);
    assert!(
        !outcome
            .failure
            .as_ref()
            .expect("post-delta failure")
            .fallback_eligible
    );
    drop(handle);
    primary.shutdown().await;
    fallback.shutdown().await;
}

#[tokio::test]
async fn post_tool_output_stream_failure_cannot_retry_or_fallback() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let primary = MockOpenAiServer::start([ProviderScript::StreamErrorAfterToolCall {
        name: "lookup".to_string(),
    }])
    .await;
    let fallback = MockOpenAiServer::start([ProviderScript::Stream {
        deltas: vec!["must-not-run".to_string()],
    }])
    .await;
    fixture
        .add_provider(
            primary.base_url(),
            10,
            RuntimePolicy {
                retry_limit: 2,
                ..RuntimePolicy::default()
            },
        )
        .await;
    fixture
        .add_provider(fallback.base_url(), 20, RuntimePolicy::default())
        .await;

    let mut handle = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start post-tool failure stream");
    timeout(Duration::from_secs(5), async {
        loop {
            let event = handle
                .events
                .recv()
                .await
                .expect("stream ended before tool output")
                .expect("stream event failure");
            if event.event_type == RuntimeEventType::ToolCallStarted {
                assert_eq!(event.payload["name"], "lookup");
                return;
            }
        }
    })
    .await
    .expect("tool output timed out");
    drain_stream(&mut handle).await;
    let outcome = (&mut handle.outcome)
        .await
        .expect("post-tool outcome sender")
        .expect("post-tool outcome");

    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert_eq!(outcome.attempts.len(), 1);
    assert_eq!(primary.call_count().await, 1);
    assert_eq!(fallback.call_count().await, 0);
    assert!(
        !outcome
            .failure
            .as_ref()
            .expect("post-tool failure")
            .fallback_eligible
    );
    drop(handle);
    primary.shutdown().await;
    fallback.shutdown().await;
}

#[tokio::test]
async fn malformed_response_is_sanitized_and_opens_the_provider_circuit() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::MalformedResponse]).await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                circuit_failure_threshold: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;

    let malformed = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("malformed response execution");
    assert_eq!(malformed.status, ExecutionStatus::Failed);
    let malformed_failure = malformed.failure.expect("malformed response failure");
    assert_eq!(
        malformed_failure.class,
        ExecutionFailureClass::ProviderInvalidResponse
    );
    assert!(!malformed_failure.message.contains("{not valid JSON"));

    let circuit_open = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("circuit-open execution");
    assert_eq!(circuit_open.status, ExecutionStatus::Failed);
    assert_eq!(
        circuit_open.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::CircuitOpen)
    );
    assert_eq!(provider.call_count().await, 1);
    provider.shutdown().await;
}

#[tokio::test]
async fn disabled_credential_fails_without_calling_provider_or_leaking_secret() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "must-not-run".to_string(),
    }])
    .await;
    let configured = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    AdminService::new(&fixture.state)
        .expect("admin service")
        .set_credential_enabled(
            &fixture.actor,
            &request_context(),
            configured.credential_id,
            false,
        )
        .await
        .expect("disable credential");

    let outcome = fixture
        .execution_service()
        .execute(fixture.command(false))
        .await
        .expect("disabled credential execution");
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    let failure = outcome.failure.expect("disabled credential failure");
    assert_eq!(failure.class, ExecutionFailureClass::CredentialNotFound);
    assert!(!failure.message.contains("sk-lifecycle-secret"));
    assert_eq!(provider.call_count().await, 0);
    provider.shutdown().await;
}

#[tokio::test]
async fn public_sse_disconnect_persists_cancellation_and_reuses_capacity() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::StalledStream {
            first_delta: Some("public-partial".to_string()),
            gate: gate.clone(),
        },
        ProviderScript::Stream {
            deltas: vec!["after-disconnect".to_string()],
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                max_concurrent_requests: 1,
                max_concurrent_streams: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;
    let client = reqwest::Client::new();
    let mut public_request = public_response_request(&fixture.route_key);
    public_request.conversation = Some(moira::domain::ResponseConversationInput {
        id: None,
        create: true,
        title: Some("Disconnect lifecycle".to_string()),
        metadata: serde_json::json!({ "test_fixture": true }),
    });
    let response = client
        .post(format!("{}/api/v1/responses/stream", moira.base_url))
        .header("x-consumer-key", consumer_key)
        .header(
            "x-request-id",
            format!("public-disconnect-{}", Uuid::now_v7()),
        )
        .json(&public_request)
        .send()
        .await
        .expect("open public SSE response");
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.expect("public SSE error body");
        panic!("public SSE returned {status}: {body}");
    }
    let mut body = response.bytes_stream();
    let envelope = timeout(Duration::from_secs(5), async {
        let mut buffer = String::new();
        loop {
            let chunk = body
                .next()
                .await
                .expect("public SSE ended before delta")
                .expect("read public SSE chunk");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(envelope) = find_sse_envelope(&buffer, "response.output_text.delta") {
                return envelope;
            }
        }
    })
    .await
    .expect("public first delta timed out");
    assert_eq!(envelope["payload"]["text"], "public-partial");
    let execution_id = parse_prefixed_uuid(
        envelope["execution_id"]
            .as_str()
            .expect("public execution id"),
        "exec_",
    );
    drop(body);

    gate.wait_connection_closed().await;
    wait_for_public_cancellation(&fixture, execution_id).await;
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from responses where execution_id = $1 and status = 'in_progress'",
                execution_id,
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from conversation_messages where execution_id is null and role = 'user' and conversation_id = (select conversation_id from responses where execution_id = $1)",
                execution_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'started'",
                execution_id,
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'cancelled'",
                execution_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from conversation_messages where execution_id = $1 and role = 'assistant'",
                execution_id,
            )
            .await,
        0
    );

    let mut reused = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("stream after public disconnect");
    wait_for_delta(&mut reused, "after-disconnect").await;
    drain_stream(&mut reused).await;
    let outcome = (&mut reused.outcome)
        .await
        .expect("reused public-disconnect outcome sender")
        .expect("reused public-disconnect outcome");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    drop(reused);
    moira.shutdown().await;
    provider.shutdown().await;
}

/// P1-7: a client that stays **connected** but stops reading.
///
/// The sibling test above (`public_sse_disconnect_persists_cancellation_and_reuses_capacity`)
/// `drop`s the body, which closes the TCP socket and drives
/// `supervise_public_stream`'s `public_tx.closed()` branch. That branch is easy: the
/// receiver is gone, so the send fails immediately.
///
/// This test drives the *other* branch — the one that had no DB-backed proof. The client
/// holds the `reqwest::Response` body open and simply stops polling it, so:
///
///   * the socket is never closed, therefore `public_rx` is never dropped, therefore
///     `public_tx.closed()` can never resolve;
///   * hyper stops accepting body frames once its write buffer and the kernel socket
///     buffers fill, so `public_rx` stops being drained;
///   * the bounded `public_tx` fills, and `send_public_event`'s
///     `tokio::time::timeout(send_timeout, tx.send(event))` arm is the *only* thing that
///     can terminate the stream.
///
/// The assertions then have to prove the cancellation actually released resources:
/// row states alone would miss a leaked concurrency permit, so the last step re-runs an
/// execution against `max_concurrent_streams: 1` capacity.
#[tokio::test]
async fn public_sse_stalled_reader_without_disconnect_releases_permit_and_terminates_response() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Every fixture-owned identifier is suffixed so concurrently running test binaries
    // cannot collide, and so an assertion can never be satisfied by a row left behind by
    // a previous run.
    let test_id = Uuid::now_v7().simple().to_string();
    let gate = ScriptGate::new();
    let first_delta = format!("public-stalled-{test_id}");
    let after_stall_delta = format!("after-stall-{test_id}");
    let provider = MockOpenAiServer::start([
        ProviderScript::FloodingStream {
            first_delta: first_delta.clone(),
            // Large enough that a few hundred frames saturate every buffer between the
            // supervisor and the stalled client; the flood is unbounded, so the exact
            // size is a throughput choice, not a correctness dependency.
            flood_delta: "s".repeat(4096),
            gate: gate.clone(),
        },
        ProviderScript::Stream {
            deltas: vec![after_stall_delta.clone()],
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                // Deliberately far above the 1 s send timeout configured below, so the
                // only clock that can fire is `send_public_event`'s. If the provider
                // request timeout or the stream idle timeout could win, the test would
                // pass for the wrong reason.
                request_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                max_concurrent_requests: 1,
                max_concurrent_streams: 1,
                ..RuntimePolicy::default()
            },
        )
        .await;
    let consumer_key = fixture.enable_public_streaming().await;

    // `send_public_event`'s bounded-send arm takes its `send_timeout` from
    // `public_api.heartbeat_seconds.max(1)` (`src/application/public.rs`). The fixture
    // leaves that at the 15 s production default, which would make this test wait 15 s of
    // real time; 1 s is the smallest value the production code accepts. Only `settings`
    // is replaced: the cloned `AppState` keeps the fixture's `ConcurrencyController`,
    // `ProviderRuntimeCache` and `CircuitBreakerRegistry` (all `Arc`-backed), so the
    // permit released here is the same permit `fixture.execution_service()` competes for
    // at the end of the test.
    let mut tuned_state = fixture.state.clone();
    let mut tuned_settings = (*fixture.state.settings).clone();
    tuned_settings.public_api.heartbeat_seconds = 1;
    tuned_state.settings = Arc::new(tuned_settings);
    let moira = MoiraHttpServer::start(tuned_state).await;

    let client = reqwest::Client::new();
    let mut public_request = public_response_request(&fixture.route_key);
    public_request.conversation = Some(moira::domain::ResponseConversationInput {
        id: None,
        create: true,
        title: Some(format!("Stalled reader {test_id}")),
        metadata: serde_json::json!({ "test_fixture": true }),
    });
    let response = client
        .post(format!("{}/api/v1/responses/stream", moira.base_url))
        .header("x-consumer-key", consumer_key)
        .header("x-request-id", format!("public-stalled-{test_id}"))
        .json(&public_request)
        .send()
        .await
        .expect("open public SSE response");
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.expect("public SSE error body");
        panic!("public SSE returned {status}: {body}");
    }

    // Read exactly one delta: proof the stream really started and the permit is held.
    let mut body = response.bytes_stream();
    let envelope = timeout(Duration::from_secs(10), async {
        let mut buffer = String::new();
        loop {
            let chunk = body
                .next()
                .await
                .expect("public SSE ended before delta")
                .expect("read public SSE chunk");
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(envelope) = find_sse_envelope(&buffer, "response.output_text.delta") {
                return envelope;
            }
        }
    })
    .await
    .expect("public first delta timed out");
    assert_eq!(envelope["payload"]["text"], first_delta);
    let execution_id = parse_prefixed_uuid(
        envelope["execution_id"]
            .as_str()
            .expect("public execution id"),
        "exec_",
    );

    // From here the client stalls: `body` stays alive (socket open, `public_rx` alive)
    // and is never polled again until the cancellation has already been observed.
    // `drop(body)` — the disconnect test's move — is deliberately NOT performed.
    gate.wait_arrived().await;
    gate.release();

    wait_for_public_cancellation(&fixture, execution_id).await;
    // Moira let go of the upstream connection as part of the same cancellation.
    gate.wait_connection_closed().await;

    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from responses where execution_id = $1 and status = 'in_progress'",
                execution_id,
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'started'",
                execution_id,
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from execution_attempts where execution_id = $1 and status = 'cancelled'",
                execution_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .scalar_i64(
                "select count(*) from conversation_messages where execution_id = $1 and role = 'assistant'",
                execution_id,
            )
            .await,
        0
    );

    // The client socket was never closed by us: the bytes the server pushed while we
    // stalled are still queued for a reader. This is what separates this test from the
    // disconnect test — had the socket closed, there would be nothing left to read.
    let queued = timeout(Duration::from_secs(5), body.next())
        .await
        .expect("stalled body read timed out")
        .expect("stalled body ended with no queued bytes")
        .expect("read queued public SSE chunk");
    assert!(!queued.is_empty());

    // The actual failure mode being hunted: a leaked permit. `max_concurrent_streams` is
    // 1, so this execution can only start if the stalled stream's permit came back.
    let mut reused = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("stream after stalled reader");
    wait_for_delta(&mut reused, &after_stall_delta).await;
    drain_stream(&mut reused).await;
    let outcome = (&mut reused.outcome)
        .await
        .expect("reused stalled-reader outcome sender")
        .expect("reused stalled-reader outcome");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    drop(reused);

    drop(body);
    moira.shutdown().await;
    provider.shutdown().await;
}

#[tokio::test]
async fn public_provider_failure_retains_keyed_i18n_error_contract() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::MalformedResponse]).await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;
    let request_id = format!("public-failure-{}", Uuid::now_v7());
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/responses", moira.base_url))
        .header("x-consumer-key", consumer_key)
        .header("x-request-id", &request_id)
        .json(&public_response_request(&fixture.route_key))
        .send()
        .await
        .expect("send failing public response");
    if response.status() != StatusCode::BAD_GATEWAY {
        let status = response.status();
        let body = response.text().await.expect("public failure error body");
        panic!("public failure returned {status}: {body}");
    }
    let serialized = response.text().await.expect("public failure body");
    assert!(!serialized.contains("private primary diagnostics"));
    assert!(!serialized.contains("{not valid JSON"));
    assert!(!serialized.contains("sk-lifecycle-secret"));
    let body: Value = serde_json::from_str(&serialized).expect("public failure JSON");
    let error = &body["error"];
    assert_eq!(error["code"], "provider_invalid_response");
    assert_eq!(
        error["message_key"],
        "moira.error.provider_invalid_response"
    );
    assert_eq!(
        error["message"],
        "provider request failed (ProviderInvalidResponse)"
    );
    assert_eq!(error["message_args"], serde_json::json!({}));
    assert_eq!(error["request_id"], request_id);
    assert!(error["details"].is_null());
    moira.shutdown().await;
    provider.shutdown().await;
}

/// P1-6 enforcement point 1: `bounded_phase(execution_deadline, self.resolve_credential(..))`.
///
/// The gate is a Postgres `ACCESS EXCLUSIVE` lock on `provider_credentials`. Credential
/// resolution issues a plain `SELECT`, and `ACCESS EXCLUSIVE` is the only lock level that
/// blocks a plain `SELECT` — a row lock would not. Acquiring the lock *is* the
/// acknowledgement: the execution is only started once the lock statement has returned, so
/// there is no sleep and no timing race.
///
/// Nothing earlier in `execute_inner` reads `provider_credentials` — command validation
/// reads `applications`, routing reads `route_definitions`/`routing_policies`/`providers`/
/// `provider_models`/`provider_runtime_policies`, and audit writes go to `audit_logs` — so
/// the lock can only bite inside the wrapped phase. If the wrapper stops bounding the
/// phase, the `SELECT` waits for the lock indefinitely and this test trips
/// `DEADLINE_TEST_GUARD` instead of returning a failure.
#[tokio::test]
async fn slow_credential_resolution_is_bounded_by_the_total_execution_deadline() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "must-not-be-reached".to_string(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let mut lock_tx = fixture
        .pool
        .begin()
        .await
        .expect("begin credential-lock transaction");
    sqlx::query("set local lock_timeout = '5s'")
        .execute(&mut *lock_tx)
        .await
        .expect("bound the credential-lock acquisition itself");
    sqlx::query("lock table provider_credentials in access exclusive mode")
        .execute(&mut *lock_tx)
        .await
        .expect("acquire the credential-lock gate");

    let mut command = fixture.command(false);
    command.options.timeout_ms = Some(DEADLINE_TEST_BUDGET.as_millis() as u64);
    let execution_id = command.execution_id;
    let started = Instant::now();
    let guarded = timeout(
        DEADLINE_TEST_GUARD,
        fixture.execution_service().execute(command),
    )
    .await;
    let elapsed = started.elapsed();
    lock_tx
        .rollback()
        .await
        .expect("release the credential-lock gate");

    let outcome = guarded
        .expect("credential resolution was not bounded by the total execution deadline")
        .expect("execution returned a transport error instead of a bounded failure");

    assert_eq!(outcome.status, ExecutionStatus::Failed);
    let failure = outcome
        .failure
        .expect("a bounded credential phase must report a failure");
    assert_eq!(failure.class, ExecutionFailureClass::DeadlineExceeded);
    assert!(
        elapsed >= DEADLINE_TEST_MINIMUM_ELAPSED,
        "the phase returned in {elapsed:?}, which is too fast to have been blocked on the credential lock"
    );
    assert!(
        elapsed < DEADLINE_TEST_GUARD / 2,
        "the phase took {elapsed:?}; the total deadline, not the guard, must be what ended it"
    );
    assert_eq!(
        provider.call_count().await,
        0,
        "the breach happens before any provider call"
    );
    assert!(
        outcome.attempts.is_empty(),
        "the breach happens before any attempt row exists"
    );
    let persisted_attempts: i64 =
        sqlx::query_scalar("select count(*) from execution_attempts where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("count execution attempts");
    assert_eq!(
        persisted_attempts, 0,
        "a pre-attempt phase breach must leave no attempt row behind"
    );
    provider.shutdown().await;
}

/// P1-6 enforcement point 2: `bounded_phase(execution_deadline, self.runtime_handle(..))`.
///
/// The gate is the per-key build lock inside `ProviderRuntimeCache::get_or_insert_with`.
/// A helper task claims that lock for the exact `RuntimeCacheKey` the execution will
/// compute and parks inside the builder; the execution then blocks on `build_lock.lock()`
/// inside the wrapped phase. The helper signalling that it entered the builder is the
/// acknowledgement, so again no sleep is involved.
///
/// `provider_credentials` is *not* locked here, so phase 1 completes in a single fast
/// round-trip; the only thing in the path that can consume the whole budget is the build
/// lock. If the cache key were reconstructed wrongly the execution would sail past the
/// contended entry, call the provider and succeed — the assertions below fail loudly
/// rather than passing vacuously.
#[tokio::test]
async fn slow_runtime_handle_construction_is_bounded_by_the_total_execution_deadline() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "must-not-be-reached".to_string(),
    }])
    .await;
    let provider_fixture = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let versions = sqlx::query(
        r#"
        select p.version as provider_version,
               pm.version as model_version,
               c.version as credential_version,
               coalesce(prp.version, 1::bigint) as runtime_policy_version
        from providers p
        join provider_models pm on pm.id = $2
        join provider_credentials c on c.id = $3
        left join provider_runtime_policies prp on prp.provider_id = p.id
        where p.id = $1
        limit 1
        "#,
    )
    .bind(provider_fixture.provider_id)
    .bind(provider_fixture.model_id)
    .bind(provider_fixture.credential_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("read the runtime cache key versions");
    let key = RuntimeCacheKey {
        provider_id: provider_fixture.provider_id,
        provider_version: versions
            .try_get("provider_version")
            .expect("provider version"),
        model_id: provider_fixture.model_id,
        model_version: versions.try_get("model_version").expect("model version"),
        credential_id: provider_fixture.credential_id,
        credential_version: versions
            .try_get("credential_version")
            .expect("credential version"),
        runtime_policy_version: versions
            .try_get("runtime_policy_version")
            .expect("runtime policy version"),
    };

    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let cache = fixture.state.runtime_handles.clone();
    let holder = tokio::spawn(async move {
        let _ = cache
            .get_or_insert_with(key, || async move {
                let _ = entered_tx.send(());
                let _ = release_rx.await;
                Err::<RuntimeModelHandle, AppError>(AppError::Internal(
                    "test-owned runtime build-lock holder".to_string(),
                ))
            })
            .await;
    });
    timeout(DEADLINE_TEST_GUARD, entered_rx)
        .await
        .expect("the build-lock holder never entered the builder")
        .expect("the build-lock holder was dropped before it signalled");

    let mut command = fixture.command(false);
    command.options.timeout_ms = Some(DEADLINE_TEST_BUDGET.as_millis() as u64);
    let execution_id = command.execution_id;
    let started = Instant::now();
    let guarded = timeout(
        DEADLINE_TEST_GUARD,
        fixture.execution_service().execute(command),
    )
    .await;
    let elapsed = started.elapsed();
    let _ = release_tx.send(());
    holder.await.expect("build-lock holder task panicked");

    let outcome = guarded
        .expect("runtime handle construction was not bounded by the total execution deadline")
        .expect("execution returned a transport error instead of a bounded failure");

    assert_eq!(outcome.status, ExecutionStatus::Failed);
    let failure = outcome
        .failure
        .expect("a bounded runtime-handle phase must report a failure");
    assert_eq!(failure.class, ExecutionFailureClass::DeadlineExceeded);
    assert!(
        elapsed >= DEADLINE_TEST_MINIMUM_ELAPSED,
        "the phase returned in {elapsed:?}, which is too fast to have been blocked on the build lock"
    );
    assert!(
        elapsed < DEADLINE_TEST_GUARD / 2,
        "the phase took {elapsed:?}; the total deadline, not the guard, must be what ended it"
    );
    assert_eq!(
        provider.call_count().await,
        0,
        "the breach happens before any provider call"
    );
    assert!(
        outcome.attempts.is_empty(),
        "the breach happens before any attempt row exists"
    );
    let persisted_attempts: i64 =
        sqlx::query_scalar("select count(*) from execution_attempts where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("count execution attempts");
    assert_eq!(
        persisted_attempts, 0,
        "a pre-attempt phase breach must leave no attempt row behind"
    );
    provider.shutdown().await;
}

/// P1-6 enforcement point 3: the bound around the three terminal writes.
///
/// This one is not a `bounded_phase` call. By the time it fires the provider call has
/// **already succeeded** and the output is committed, so the breach must not be flattened
/// into a plain deadline failure — it must carry the existing
/// `attempt_timeout_failure(bounded_by_total_deadline, output_committed)` clamp so nothing
/// downstream retries or falls back onto a second provider and bills the caller twice.
///
/// The gate is a `SELECT ... FOR UPDATE` row lock on the attempt row, taken after the mock
/// provider reports the request arrived (which is itself proof the attempt row exists,
/// because `insert_attempt_started` runs before the provider call) and before the provider
/// response is released. That blocks `update_attempt` — the first of the three terminal
/// writes — and nothing else in the database.
#[tokio::test]
async fn terminal_persistence_timeout_is_recorded_as_output_committed_not_as_a_plain_failure() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([ProviderScript::HeldCompletion {
        text: "committed-output".to_string(),
        gate: gate.clone(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;

    let mut command = fixture.command(false);
    command.options.timeout_ms = Some(DEADLINE_TEST_BUDGET.as_millis() as u64);
    let execution_id = command.execution_id;
    let request_id = command.request_id.clone();
    let service = fixture.execution_service();
    let execution = tokio::spawn(async move { service.execute_with_events(command).await });

    gate.wait_arrived().await;
    let attempt_id: Uuid =
        sqlx::query_scalar("select id from execution_attempts where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("the attempt row must exist before the provider call");

    let mut lock_tx = fixture
        .pool
        .begin()
        .await
        .expect("begin terminal-persistence lock transaction");
    sqlx::query("set local lock_timeout = '5s'")
        .execute(&mut *lock_tx)
        .await
        .expect("bound the attempt-row lock acquisition itself");
    sqlx::query("select id from execution_attempts where id = $1 for update")
        .bind(attempt_id)
        .fetch_one(&mut *lock_tx)
        .await
        .expect("acquire the attempt-row gate");

    let started = Instant::now();
    gate.release();
    gate.wait_completed().await;
    let guarded = timeout(DEADLINE_TEST_GUARD, execution).await;
    let elapsed = started.elapsed();
    lock_tx
        .rollback()
        .await
        .expect("release the attempt-row gate");

    let (outcome, events) = guarded
        .expect("terminal persistence was not bounded by the total execution deadline")
        .expect("execution task panicked")
        .expect("execution returned a transport error instead of a bounded failure");

    assert_eq!(outcome.status, ExecutionStatus::Failed);
    let failure = outcome
        .failure
        .expect("a bounded terminal-persistence phase must report a failure");

    // The point of the test: this is the output-committed failure class, not a plain
    // deadline failure. Both use `DeadlineExceeded`, so the class alone proves nothing —
    // the clamp and the distinguishing message are what matter.
    assert_eq!(failure.class, ExecutionFailureClass::DeadlineExceeded);
    assert!(
        !failure.retryable,
        "committed output must never be re-executed"
    );
    assert!(
        !failure.fallback_eligible,
        "committed output must never be sent to a fallback provider"
    );
    assert_eq!(
        failure.message, "execution exceeded its total deadline while persisting terminal state",
        "a terminal-persistence breach must stay distinguishable from a plain deadline failure"
    );
    assert!(
        elapsed >= DEADLINE_TEST_MINIMUM_ELAPSED,
        "terminal persistence returned in {elapsed:?}, too fast to have been blocked on the attempt row"
    );
    assert!(
        elapsed < DEADLINE_TEST_GUARD / 2,
        "terminal persistence took {elapsed:?}; the bound, not the guard, must be what ended it"
    );

    let attempt = outcome
        .attempts
        .last()
        .expect("the successful provider attempt must still be reported");
    assert_eq!(attempt.attempt_id, attempt_id);
    assert_eq!(attempt.status, AttemptStatus::Failed);
    assert_eq!(
        attempt.failure_class,
        Some(ExecutionFailureClass::DeadlineExceeded)
    );

    let failed_event = events
        .iter()
        .find(|event| event.event_type == RuntimeEventType::ExecutionFailed)
        .expect("a terminal-persistence breach must emit ExecutionFailed");
    assert_eq!(failed_event.payload["phase"], "terminal_persistence");
    assert_eq!(failed_event.payload["output_committed"], true);

    // The provider really did answer, and the terminal write group really was cut off.
    //
    // Deliberately *not* asserted: the final state of `execution_attempts.status`. Dropping
    // the timed-out future cancels the await, but the `update attempt` statement is already
    // in flight on the server and Postgres runs it to completion once the row lock is
    // released. Asserting on that would be asserting sqlx's cancellation semantics rather
    // than Moira's bound. The second write of the group is the honest evidence: it is only
    // issued after the first one returns, so its absence proves the group was cut off.
    assert_eq!(provider.call_count().await, 1);
    let usage_rows: i64 =
        sqlx::query_scalar("select count(*) from usage_records where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("count usage records");
    assert_eq!(
        usage_rows, 0,
        "the usage write is part of the same bounded group and must not have landed"
    );

    // The condition is audited under its own action, on the bounded best-effort path.
    let audit: Value = sqlx::query_scalar(
        "select metadata from audit_logs
         where resource_id = $1
           and request_id = $2
           and action = 'execution.terminal_persistence_deadline_exceeded'",
    )
    .bind(execution_id.to_string())
    .bind(&request_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("the terminal-persistence breach must be audited under its own action");
    assert_eq!(audit["output_committed"], true);
    assert_eq!(audit["attempt_id"], attempt_id.to_string());

    provider.shutdown().await;
}

async fn wait_for_delta(handle: &mut moira::domain::ExecutionStreamHandle, expected: &str) {
    timeout(Duration::from_secs(5), async {
        loop {
            let event = handle
                .events
                .recv()
                .await
                .expect("stream ended before expected delta")
                .expect("stream event failure");
            if event.event_type == RuntimeEventType::OutputTextDelta {
                assert_eq!(event.payload["text"], expected);
                return;
            }
        }
    })
    .await
    .expect("stream delta timed out");
}

async fn drain_stream(handle: &mut moira::domain::ExecutionStreamHandle) {
    timeout(Duration::from_secs(5), async {
        while handle.events.recv().await.is_some() {}
    })
    .await
    .expect("stream drain timed out");
}

fn find_sse_envelope(buffer: &str, event_type: &str) -> Option<Value> {
    buffer
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .find(|value| value["type"] == event_type)
}

fn parse_prefixed_uuid(value: &str, prefix: &str) -> Uuid {
    Uuid::parse_str(
        value
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("{value} is missing {prefix}")),
    )
    .expect("prefixed UUID")
}

async fn wait_for_public_cancellation(fixture: &LifecycleFixture, execution_id: Uuid) {
    timeout(Duration::from_secs(5), async {
        loop {
            let status = sqlx::query("select status from responses where execution_id = $1")
                .bind(execution_id)
                .fetch_optional(&fixture.pool)
                .await
                .expect("query public response status")
                .and_then(|row| row.try_get::<String, _>("status").ok());
            if status.as_deref() == Some("cancelled") {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("public response was not cancelled");
}

async fn wait_for_attempt_status(
    fixture: &LifecycleFixture,
    execution_id: Uuid,
    expected_status: &str,
) {
    timeout(Duration::from_secs(5), async {
        loop {
            let status = sqlx::query("select status from execution_attempts where execution_id = $1 order by attempt_number desc limit 1")
                .bind(execution_id)
                .fetch_optional(&fixture.pool)
                .await
                .expect("query execution attempt status")
                .and_then(|row| row.try_get::<String, _>("status").ok());
            if status.as_deref() == Some(expected_status) {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("execution {execution_id} did not reach attempt status {expected_status}")
    });
}
