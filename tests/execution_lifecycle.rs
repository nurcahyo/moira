mod support;

use std::time::Duration;

use axum::http::StatusCode;
use futures_util::StreamExt;
use moira::{
    application::{AdminService, ExecutionService},
    domain::{ExecutionFailureClass, ExecutionStatus, RuntimeEventType},
};
use serde_json::Value;
use sqlx::Row;
use support::mock_openai::{MockOpenAiServer, ProviderScript, ScriptGate};
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy, public_response_request, request_context,
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

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
