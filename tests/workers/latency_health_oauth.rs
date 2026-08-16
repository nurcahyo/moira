//! End-to-end proof for the three DB-backed job handlers this change adds —
//! `latency-stats-aggregation`, `oauth-token-refresh`, `provider-health-check` — enqueued
//! through the real [`WorkerQueue`] and dispatched through the real `default_dispatcher`
//! against a real PostgreSQL and a scripted local HTTP server
//! ([`support::mock_control_plane`]). Never a real provider and never a real token — the
//! owner-approved testing policy for this workstream (plan 12 §1).
//!
//! Unit-level proofs (percentile arithmetic, refresh eligibility, health classification) live
//! next to the code they test, in each handler module's own `#[cfg(test)]` block
//! (`src/infra/workers/latency_stats.rs`, `oauth_refresh.rs`, `provider_health_check.rs`).
//! This file is deliberately the *only* new integration-test file for all three — see the
//! task's own "ONE new test file max" instruction — so it groups every end-to-end case rather
//! than spreading three thin files with three near-identical `TestDatabase` preambles.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use moira::{
    application::{AdminService, RequestContext},
    config::WorkerSettings,
    domain::{
        CredentialCreateRequest, CredentialScope, CredentialSecret, CredentialType,
        ProviderCreateRequest, ProviderModelCreateRequest, ProviderType,
    },
    infra::{
        metrics::MetricsRegistry,
        repositories::{AdminRepository, PgAdminRepository, PgWorkerJobRepository},
        workers::{dispatch::default_dispatcher, queue::WorkerQueue},
    },
    security::{Actor, ActorType, CredentialAadParts, SecretCipher, credential_aad},
};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use axum::http::StatusCode;

use crate::support::{
    MoiraHttpServer, TestDatabase,
    mock_control_plane::{MockControlPlane, TokenScript},
};

fn metrics() -> MetricsRegistry {
    MetricsRegistry::new("moira-test", None)
}

fn admin_actor() -> Actor {
    Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some("latency-health-oauth-test".to_string()),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    }
}

fn request_context() -> RequestContext {
    RequestContext {
        request_id: format!("test-{}", Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("moira-latency-health-oauth-test".to_string()),
        idempotency_key: None,
    }
}

fn queue(pool: &PgPool, settings: WorkerSettings) -> WorkerQueue {
    WorkerQueue::new(
        Arc::new(PgWorkerJobRepository::new(pool.clone())),
        Arc::new(settings),
        Uuid::now_v7(),
    )
}

/// One poll through the real `default_dispatcher`, with a pool — the exact object
/// `run_supervisor` builds when a database is configured, driving the three handlers this
/// file proves rather than the always-no-op path `tests/workers/job_dispatch.rs` proves for
/// the case with no pool.
///
/// Takes `state` rather than a bare pool so the dispatcher's cipher is the *same instance*
/// `AdminService::create_credential` encrypted the fixture's oauth2 credential with — a
/// dispatcher built from a throwaway `LocalSecretCipher` would decrypt with the wrong key and
/// every AEAD verification would fail, which is indistinguishable from "the credential row is
/// wrong" until you have chased it once.
async fn dispatch_one(
    state: &moira::app::AppState,
    settings: WorkerSettings,
) -> moira::infra::workers::queue::QueueTickOutcome {
    let pool = state.pool.clone().expect("test state has a pool");
    let queue = queue(&pool, settings.clone());
    let dispatcher = default_dispatcher(
        Some(pool),
        state.cipher.clone(),
        state.http.clone(),
        metrics(),
        Arc::new(settings),
        // Derived from the fixture's own settings, exactly as `run_supervisor` derives it
        // from `AppState`. `test_state` opts into both `provider_security` escape hatches, so
        // the mock control plane on `http://127.0.0.1:<port>` is reachable; `strict_state`
        // opts into neither, which is what the deny test below turns on.
        moira::infra::workers::oauth_refresh::TokenEndpointPolicy::from_provider_security(
            &state.settings.provider_security,
        ),
    );
    queue
        .run_once(&dispatcher, &metrics())
        .await
        .expect("poll the queue")
}

// ---------------------------------------------------------------------------------------
// latency-stats-aggregation
// ---------------------------------------------------------------------------------------

/// Inserts one `execution_attempts` row with a `succeeded` status and the given latency, tied
/// to `provider_id`/`provider_model_id`. Raw SQL rather than going through the execution
/// pipeline: this suite is about the aggregation job reading real durations, not about
/// producing them, and `execution_attempts` has no repository write path this test would
/// otherwise reuse (`src/infra/repositories/runtime.rs::insert_attempt_started` needs a whole
/// `ExecutionAttemptInsert` this test has no execution to build one from).
async fn insert_succeeded_attempt(
    pool: &PgPool,
    provider_id: Uuid,
    provider_model_id: Uuid,
    latency_ms: i64,
) {
    sqlx::query(
        r#"
        insert into execution_attempts
            (id, request_id, execution_id, attempt_number, provider_id, provider_model_id,
             status, started_at, completed_at, latency_ms)
        values ($1, $2, $3, 1, $4, $5, 'succeeded', now(), now(), $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(format!("req-{}", Uuid::now_v7()))
    .bind(Uuid::now_v7())
    .bind(provider_id)
    .bind(provider_model_id)
    .bind(latency_ms)
    .execute(pool)
    .await
    .expect("insert a succeeded execution attempt");
}

#[tokio::test]
async fn latency_stats_aggregation_writes_measured_percentiles_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let admin = AdminService::new(&state).expect("admin service");
    let actor = admin_actor();

    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Latency test provider".to_string(),
                base_url: None,
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");
    let model = admin
        .create_provider_model(
            &actor,
            &request_context(),
            provider.id,
            ProviderModelCreateRequest {
                model_key: "latency-test-model".to_string(),
                display_name: None,
                capabilities: json!({}),
            },
        )
        .await
        .expect("create provider model");

    // Ten evenly spaced latencies, nearest-rank: p50 = 500ms, p95 = 1000ms (the same fixture
    // `percentiles_use_the_nearest_rank_method` in `src/infra/workers/latency_stats.rs`
    // proves at the pure-function level).
    for n in 1..=10 {
        insert_succeeded_attempt(&pool, provider.id, model.id, n * 100).await;
    }

    let settings = WorkerSettings::default();
    let job_id = queue(&pool, settings.clone())
        .enqueue("latency-stats-aggregation", json!({}), None, &metrics())
        .await
        .expect("enqueue latency-stats-aggregation");

    let outcome = dispatch_one(&state, settings).await;
    assert_eq!(outcome.claimed, 1);
    assert_eq!(
        outcome.completed, 1,
        "the aggregation handler must complete, not fail"
    );
    assert_eq!(status_of(&pool, job_id).await, "completed");

    let row = sqlx::query(
        "select p50_latency_ms, p95_latency_ms, sample_count from provider_model_latency_stats \
         where provider_id = $1 and provider_model_id = $2",
    )
    .bind(provider.id)
    .bind(model.id)
    .fetch_one(&pool)
    .await
    .expect("provider_model_latency_stats row must exist after aggregation");
    let p50: i32 = row.get("p50_latency_ms");
    let p95: i32 = row.get("p95_latency_ms");
    let sample_count: i64 = row.get("sample_count");
    assert_eq!(p50, 500);
    assert_eq!(p95, 1000);
    assert_eq!(sample_count, 10);
}

// ---------------------------------------------------------------------------------------
// provider-health-check
// ---------------------------------------------------------------------------------------

async fn snapshot_row(pool: &PgPool, provider_id: Uuid) -> (String, Option<i32>) {
    let row = sqlx::query(
        "select status, latency_ms from provider_health_snapshots \
         where provider_id = $1 and provider_model_id is null \
         order by observed_at desc limit 1",
    )
    .bind(provider_id)
    .fetch_one(pool)
    .await
    .expect("a provider_health_snapshots row must exist after a health-check run");
    (row.get("status"), row.get("latency_ms"))
}

#[tokio::test]
async fn provider_health_check_records_a_healthy_snapshot_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;

    let provider = AdminService::new(&state)
        .expect("admin service")
        .create_provider(
            &admin_actor(),
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Healthy test provider".to_string(),
                base_url: Some(control_plane.health_url()),
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("provider-health-check", json!({}), None, &metrics())
        .await
        .expect("enqueue provider-health-check");
    let outcome = dispatch_one(&state, settings).await;
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);

    let (status, latency_ms) = snapshot_row(&pool, provider.id).await;
    assert_eq!(status, "healthy");
    assert!(
        latency_ms.is_some(),
        "a reachable probe must record a measured latency"
    );
    assert_eq!(control_plane.health_call_count(), 1);

    control_plane.shutdown().await;
}

#[tokio::test]
async fn provider_health_check_records_a_degraded_snapshot_for_a_slow_provider() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;
    // Above `DEGRADED_LATENCY_THRESHOLD_MS` (2s) but comfortably under the default probe
    // timeout (3s), so the probe still succeeds — just slowly.
    control_plane
        .set_health_delay(Duration::from_millis(2_200))
        .await;

    let provider = AdminService::new(&state)
        .expect("admin service")
        .create_provider(
            &admin_actor(),
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Slow test provider".to_string(),
                base_url: Some(control_plane.health_url()),
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("provider-health-check", json!({}), None, &metrics())
        .await
        .expect("enqueue provider-health-check");
    let outcome = dispatch_one(&state, settings).await;
    assert_eq!(outcome.completed, 1);

    let (status, _latency_ms) = snapshot_row(&pool, provider.id).await;
    assert_eq!(status, "degraded");

    control_plane.shutdown().await;
}

#[tokio::test]
async fn provider_health_check_records_an_unhealthy_snapshot_for_an_unreachable_provider() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let unreachable = moira::config::WorkerSettings::default();
    let dead_url = crate::support::mock_control_plane::unreachable_url().await;

    let provider = AdminService::new(&state)
        .expect("admin service")
        .create_provider(
            &admin_actor(),
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Unreachable test provider".to_string(),
                base_url: Some(dead_url),
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    queue(&pool, unreachable.clone())
        .enqueue("provider-health-check", json!({}), None, &metrics())
        .await
        .expect("enqueue provider-health-check");
    let outcome = dispatch_one(&state, unreachable).await;
    assert_eq!(
        outcome.completed, 1,
        "an unreachable provider must not fail the job — fail-soft per provider"
    );

    let (status, latency_ms) = snapshot_row(&pool, provider.id).await;
    assert_eq!(status, "unhealthy");
    assert!(latency_ms.is_none());
}

/// The read half of issue #83, end to end over the real router: probe a reachable provider,
/// then `GET /api/v1/admin/providers/health` and read the aggregate back.
///
/// This is the only test in the tree that decodes `average_latency_ms`, and decoding it is the
/// whole point. `provider_health_snapshots.latency_ms` is `integer`, so `avg(latency_ms)` is
/// Postgres `numeric`; this crate builds sqlx with neither `bigdecimal` nor `rust_decimal`, so
/// there is no `NUMERIC` decoder at all and `try_get::<Option<f64>>` fails with `ColumnDecode`
/// on any non-NULL value — a 500 for every caller, from the first reachable probe onwards.
/// The writers' own tests could never see it: they read `provider_health_snapshots` directly
/// and never touch the aggregate. Hence the deliberate shape here — a real snapshot with a
/// non-null `latency_ms` first, then the route, then an assertion that the number arrived.
///
/// Going through `MoiraHttpServer` rather than calling `AdminService::provider_health`
/// directly buys the status code: a `ColumnDecode` becomes `AppError::Sqlx` becomes 500, and
/// "the endpoint answers 200" is the claim the issue disputes.
#[tokio::test]
async fn provider_health_summary_reports_the_average_latency_over_http() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;

    let provider = AdminService::new(&state)
        .expect("admin service")
        .create_provider(
            &admin_actor(),
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Health summary provider".to_string(),
                base_url: Some(control_plane.health_url()),
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("provider-health-check", json!({}), None, &metrics())
        .await
        .expect("enqueue provider-health-check");
    let outcome = dispatch_one(&state, settings).await;
    assert_eq!(outcome.completed, 1);

    // The row the aggregate is about to average over: `healthy`, so `latency_ms` is non-null,
    // which is precisely the state that makes the summary's decode run at all.
    let (status, latency_ms) = snapshot_row(&pool, provider.id).await;
    assert_eq!(status, "healthy");
    let measured = latency_ms.expect("a reachable probe must record a measured latency");

    let server = MoiraHttpServer::start(state.clone()).await;
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/admin/providers/health", server.base_url))
        .send()
        .await
        .expect("call GET /api/v1/admin/providers/health");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "the health read surface must not 500 once a reachable probe is in the window"
    );
    let body: serde_json::Value = response.json().await.expect("parse the health response");

    let wanted = json!(provider.id);
    let entry = body["providers"]
        .as_array()
        .expect("providers must be an array")
        .iter()
        .find(|entry| entry["provider_id"] == wanted)
        .expect("the probed provider must appear in the summary");
    assert_eq!(entry["status"], "healthy");
    assert_eq!(entry["probes_total"], json!(1));
    assert_eq!(entry["probes_successful"], json!(1));
    let average = entry["average_latency_ms"]
        .as_f64()
        .expect("average_latency_ms must be a number, not null");
    assert!(
        (average - f64::from(measured)).abs() < 0.5,
        "the average over one snapshot must be that snapshot's latency: got {average}, \
         snapshot recorded {measured}"
    );

    server.shutdown().await;
    control_plane.shutdown().await;
}

/// Issue #251 finding 3. `current_status` came from the newest snapshot *ever* recorded,
/// with no window bound — so once probing stopped (the operator disabled the provider, or
/// turned workers off, which also stops `prune_health_snapshots`, since that only ever runs
/// from inside `provider-health-check` itself) the surface reported the last status forever.
///
/// An operator reading "healthy, last probed: null" for a provider nobody has touched in
/// days is worse than reading nothing, and three separate doc comments — on
/// `ProviderHealthSummaryRow::current_status`, on the `provider_health_summaries` trait
/// method, and on `ProviderHealthStatus` — all promised `unknown` here.
#[tokio::test]
async fn provider_health_summary_reports_unknown_once_its_last_probe_ages_out_of_the_window() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;

    let provider = AdminService::new(&state)
        .expect("admin service")
        .create_provider(
            &admin_actor(),
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: "Stale health provider".to_string(),
                base_url: Some(control_plane.health_url()),
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("provider-health-check", json!({}), None, &metrics())
        .await
        .expect("enqueue provider-health-check");
    assert_eq!(dispatch_one(&state, settings.clone()).await.completed, 1);
    assert_eq!(snapshot_row(&pool, provider.id).await.0, "healthy");

    // Age the one snapshot past `provider_health_snapshot_retention_hours` rather than
    // waiting a day for it — the same technique `a_job_orphaned_by_a_dead_replica_is_…`
    // uses on `claimed_at`. The prune sweep would have deleted it, but the prune sweep
    // only runs from inside the probe job, which in this scenario has stopped running.
    let aged = sqlx::query(
        "update provider_health_snapshots
         set observed_at = now() - make_interval(hours => $2::int)
         where provider_id = $1",
    )
    .bind(provider.id)
    .bind(i32::try_from(settings.provider_health_snapshot_retention_hours + 1).unwrap_or(25))
    .execute(&pool)
    .await
    .expect("age the snapshot out of the window");
    assert_eq!(aged.rows_affected(), 1);

    let server = MoiraHttpServer::start(state.clone()).await;
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/admin/providers/health", server.base_url))
        .send()
        .await
        .expect("call GET /api/v1/admin/providers/health");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.expect("parse the health response");

    let wanted = json!(provider.id);
    let entry = body["providers"]
        .as_array()
        .expect("providers must be an array")
        .iter()
        .find(|entry| entry["provider_id"] == wanted)
        .expect("a provider with no snapshot in the window must still appear");
    assert_eq!(
        entry["status"],
        json!("unknown"),
        "a status from outside the rolling window was reported as the current one: {entry}"
    );
    assert_eq!(entry["probes_total"], json!(0));
    assert_eq!(entry["last_probe_at"], json!(null));

    server.shutdown().await;
    control_plane.shutdown().await;
}

// ---------------------------------------------------------------------------------------
// oauth-token-refresh
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn oauth_token_refresh_rotates_the_credential_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;
    let actor = admin_actor();
    let admin = AdminService::new(&state).expect("admin service");

    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::Anthropic,
                display_name: "OAuth test provider".to_string(),
                base_url: None,
                metadata: json!({ "oauth_token_endpoint": control_plane.token_endpoint() }),
            },
        )
        .await
        .expect("create provider");

    // Due now: `expires_at` is inside the default 900s lead window.
    let expires_soon = Utc::now() + chrono::Duration::seconds(60);
    let credential = admin
        .create_credential(
            &actor,
            &request_context(),
            CredentialCreateRequest {
                provider_id: provider.id,
                credential_type: CredentialType::Oauth2,
                scope: CredentialScope::Global,
                secret: CredentialSecret::OAuth2 {
                    access_token: "old-access-token".to_string(),
                    refresh_token: Some("old-refresh-token".to_string()),
                    token_type: Some("Bearer".to_string()),
                    expires_at: Some(expires_soon),
                },
                display_name: Some("OAuth test credential".to_string()),
                priority: 100,
                expires_at: Some(expires_soon),
                metadata: json!({}),
            },
        )
        .await
        .expect("create oauth2 credential");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("oauth-token-refresh", json!({}), None, &metrics())
        .await
        .expect("enqueue oauth-token-refresh");
    let outcome = dispatch_one(&state, settings).await;
    assert_eq!(
        outcome.completed, 1,
        "the refresh handler must complete, not fail"
    );
    assert_eq!(
        control_plane.token_call_count(),
        1,
        "the mock token endpoint must have been called exactly once"
    );

    // Decrypt the row exactly as `AdminRepository::load_credential_secret` +
    // `CredentialAadParts` do for every other reader — the same path
    // `CredentialAdminService::validate_credential` uses in
    // `src/application/admin/credentials.rs`.
    let admin_repo = PgAdminRepository::new(pool.clone());
    let stored = admin_repo
        .load_credential_secret(credential.id)
        .await
        .expect("load the refreshed credential");
    assert!(
        stored.record.expires_at.unwrap() > expires_soon,
        "a successful refresh must push expires_at out"
    );
    let aad = credential_aad(CredentialAadParts {
        credential_id: stored.record.id,
        provider_id: stored.record.provider_id,
        credential_type: "oauth2",
        scope_type: "global",
        external_tenant_id: None,
        application_id: None,
        external_user_id: None,
        encryption_version: stored.record.encryption_version,
    });
    let plaintext = state
        .cipher
        .decrypt(&stored.encrypted, aad.as_bytes())
        .expect("decrypt the refreshed credential");
    let secret: CredentialSecret =
        serde_json::from_slice(&plaintext).expect("parse the refreshed secret");
    let CredentialSecret::OAuth2 {
        access_token,
        refresh_token,
        ..
    } = secret
    else {
        panic!("refreshed credential must still be an oauth2 secret");
    };
    assert_eq!(access_token, "mock-access-token");
    assert_eq!(refresh_token.as_deref(), Some("mock-refresh-token-2"));

    control_plane.shutdown().await;
}

/// Issue #251 finding 6. `expires_in` is optional in RFC 6749 §5.1, and
/// `apply_oauth_refresh` wrote `expires_at = coalesce($10, expires_at)` — so a provider that
/// omitted it left the *old* token's expiry on the row. The credential therefore stayed
/// permanently inside `list_oauth_credentials_due_for_refresh`'s `expires_at < threshold`,
/// and was re-exchanged every `maintenance_enqueue_interval_seconds` (60s) forever, rotating
/// the refresh-token chain each time against any provider that rotates it, while recording a
/// `moira_oauth_refresh_total` success on every pass so the metric read as healthy.
///
/// Two polls, not one, because one poll cannot tell "the expiry did not move" from "the
/// expiry moved and the second poll would not pick it up anyway".
#[tokio::test]
async fn a_refresh_with_no_expires_in_clears_the_expiry_instead_of_staying_permanently_due() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let control_plane = MockControlPlane::start().await;
    control_plane
        .set_token_script(TokenScript::Success {
            access_token: "mock-access-token".to_string(),
            refresh_token: Some("mock-refresh-token-2".to_string()),
            expires_in: None,
        })
        .await;
    let actor = admin_actor();
    let admin = AdminService::new(&state).expect("admin service");

    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::Anthropic,
                display_name: "OAuth no-expiry provider".to_string(),
                base_url: None,
                metadata: json!({ "oauth_token_endpoint": control_plane.token_endpoint() }),
            },
        )
        .await
        .expect("create provider");

    let expires_soon = Utc::now() + chrono::Duration::seconds(60);
    let credential = admin
        .create_credential(
            &actor,
            &request_context(),
            CredentialCreateRequest {
                provider_id: provider.id,
                credential_type: CredentialType::Oauth2,
                scope: CredentialScope::Global,
                secret: CredentialSecret::OAuth2 {
                    access_token: "old-access-token".to_string(),
                    refresh_token: Some("old-refresh-token".to_string()),
                    token_type: Some("Bearer".to_string()),
                    expires_at: Some(expires_soon),
                },
                display_name: Some("OAuth no-expiry credential".to_string()),
                priority: 100,
                expires_at: Some(expires_soon),
                metadata: json!({}),
            },
        )
        .await
        .expect("create oauth2 credential");

    let settings = WorkerSettings::default();
    let enqueue_and_run = || async {
        queue(&pool, settings.clone())
            .enqueue("oauth-token-refresh", json!({}), None, &metrics())
            .await
            .expect("enqueue oauth-token-refresh");
        assert_eq!(dispatch_one(&state, settings.clone()).await.completed, 1);
    };

    enqueue_and_run().await;
    assert_eq!(control_plane.token_call_count(), 1);

    let admin_repo = PgAdminRepository::new(pool.clone());
    let after_first = admin_repo
        .load_credential_secret(credential.id)
        .await
        .expect("load the refreshed credential");
    assert_eq!(
        after_first.record.expires_at, None,
        "a token endpoint that did not state a lifetime must leave no lifetime on the row — \
         keeping the replaced token's expiry attributes it to its replacement, and keeps the \
         credential permanently matching the due query"
    );

    // The behavioural consequence, not just the column: the next maintenance cycle must not
    // exchange the same credential again.
    enqueue_and_run().await;
    assert_eq!(
        control_plane.token_call_count(),
        1,
        "the credential was re-exchanged on the next cycle, so it is still permanently due"
    );

    control_plane.shutdown().await;
}

#[tokio::test]
async fn oauth_token_refresh_skips_a_credential_with_no_configured_token_endpoint() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    let actor = admin_actor();
    let admin = AdminService::new(&state).expect("admin service");

    // No `oauth_token_endpoint` in metadata — the module's documented refusal to guess one.
    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::Anthropic,
                display_name: "Unconfigured oauth provider".to_string(),
                base_url: None,
                metadata: json!({}),
            },
        )
        .await
        .expect("create provider");

    let expires_soon = Utc::now() + chrono::Duration::seconds(60);
    let credential = admin
        .create_credential(
            &actor,
            &request_context(),
            CredentialCreateRequest {
                provider_id: provider.id,
                credential_type: CredentialType::Oauth2,
                scope: CredentialScope::Global,
                secret: CredentialSecret::OAuth2 {
                    access_token: "old-access-token".to_string(),
                    refresh_token: Some("old-refresh-token".to_string()),
                    token_type: Some("Bearer".to_string()),
                    expires_at: Some(expires_soon),
                },
                display_name: None,
                priority: 100,
                expires_at: Some(expires_soon),
                metadata: json!({}),
            },
        )
        .await
        .expect("create oauth2 credential");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("oauth-token-refresh", json!({}), None, &metrics())
        .await
        .expect("enqueue oauth-token-refresh");
    let outcome = dispatch_one(&state, settings).await;
    // Fail-soft per credential: the job itself still completes even though this one
    // credential's refresh attempt failed for lack of a configured endpoint.
    assert_eq!(outcome.completed, 1);

    let admin_repo = PgAdminRepository::new(pool.clone());
    let unchanged = admin_repo
        .get_credential(credential.id)
        .await
        .expect("load the credential");
    assert_eq!(
        unchanged.expires_at, credential.expires_at,
        "a credential with no configured token endpoint must not be modified"
    );
}

/// Issue #251 finding 2: `providers.metadata` is a free-form JSON blob nothing validates, so
/// an operator (or anyone who compromises `moira:providers:write`) could point
/// `oauth_token_endpoint` at `http://127.0.0.1:…` or `http://169.254.169.254/…` and Moira
/// would POST a **decrypted refresh token** there.
///
/// The mock control plane here is the exfiltration target, and the assertion that matters is
/// `token_call_count() == 0`: not "the refresh failed" but "the refresh token never left the
/// process". Everything else about the fixture is identical to
/// `oauth_token_refresh_rotates_the_credential_end_to_end` — same mock, same reachable
/// loopback address, same due credential — so the *only* difference between a rotation and a
/// refusal is `strict_state`'s `provider_security`, which is what a real deployment has.
#[tokio::test]
async fn oauth_token_refresh_never_posts_a_refresh_token_to_an_ssrf_blocked_endpoint() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = strict_state(&pool).await;
    let control_plane = MockControlPlane::start().await;
    let actor = admin_actor();
    let admin = AdminService::new(&state).expect("admin service");

    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::Anthropic,
                display_name: "Exfiltration target provider".to_string(),
                base_url: None,
                // Written unvalidated — `create_provider` checks `base_url`, never
                // `metadata`. That is exactly the finding: the write path is not the guard.
                metadata: json!({ "oauth_token_endpoint": control_plane.token_endpoint() }),
            },
        )
        .await
        .expect("create provider");

    let expires_soon = Utc::now() + chrono::Duration::seconds(60);
    let credential = admin
        .create_credential(
            &actor,
            &request_context(),
            CredentialCreateRequest {
                provider_id: provider.id,
                credential_type: CredentialType::Oauth2,
                scope: CredentialScope::Global,
                secret: CredentialSecret::OAuth2 {
                    access_token: "old-access-token".to_string(),
                    refresh_token: Some("old-refresh-token".to_string()),
                    token_type: Some("Bearer".to_string()),
                    expires_at: Some(expires_soon),
                },
                display_name: Some("Exfiltration test credential".to_string()),
                priority: 100,
                expires_at: Some(expires_soon),
                metadata: json!({}),
            },
        )
        .await
        .expect("create oauth2 credential");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("oauth-token-refresh", json!({}), None, &metrics())
        .await
        .expect("enqueue oauth-token-refresh");
    let outcome = dispatch_one(&state, settings).await;

    assert_eq!(
        control_plane.token_call_count(),
        0,
        "the refresh token must never be POSTed to an endpoint the outbound SSRF policy \
         refuses"
    );
    // Fail-soft per credential, exactly as an unreachable identity provider is: one
    // misconfigured provider must not dead-letter the whole job.
    assert_eq!(
        outcome.completed, 1,
        "the job itself still completes; only this credential's refresh is refused"
    );

    let admin_repo = PgAdminRepository::new(pool.clone());
    let unchanged = admin_repo
        .get_credential(credential.id)
        .await
        .expect("load the credential");
    assert_eq!(
        unchanged.expires_at, credential.expires_at,
        "a refused refresh must leave the credential row untouched"
    );
    assert_eq!(
        unchanged.version, credential.version,
        "a refused refresh must not bump the credential's version"
    );

    control_plane.shutdown().await;
}

/// The **fourth** mechanism this fix deliberately added, and the one round one shipped with no
/// test at all: `OAuthTokenRefreshHandler::new` builds its own client with
/// `redirect::Policy::none()` instead of taking `AppState::http`, which keeps reqwest's
/// default `redirect::Policy::limited(10)`.
///
/// The other three that change behaviour are covered: the SSRF guard on the configured
/// endpoint (`oauth_token_refresh_never_posts_a_refresh_token_to_an_ssrf_blocked_endpoint`)
/// and the two halves of the skill-credential entitlement rule (`tests/skill_import.rs`).
/// This one had nothing, and an untested security mechanism is a claim rather than a control
/// — the case it exists for is one none of the other three can reach: a token endpoint that
/// **passes** validation and then answers `307 Location: <somewhere else>`. Validation is a
/// statement about the request Moira issues, never about wherever the response points next.
///
/// Two further constructions in that handler stay untested and are *not* claimed as covered:
/// the `Url`-typed `exchange_refresh_token` signature, which is a compile-time obligation
/// with no runtime behaviour to observe, and the no-fallback `http: Option<Client>`, whose
/// `None` arm is only reachable if reqwest cannot initialise a TLS backend at all — a
/// condition no test can force without a test-only constructor.
///
/// Both planes are on loopback, so the fixture is the permissive `test_state`: the whole
/// point is that the *first* address is allowed. The assertion is on the second plane —
/// `token_call_count() == 0` and, stronger, that the refresh token's plaintext never appeared
/// in any body it received.
#[tokio::test]
async fn oauth_token_refresh_never_follows_a_redirect_away_from_the_validated_endpoint() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = test_state(&pool).await;
    // `entry` is the configured, validated endpoint; `exfiltration` is where its redirect
    // points. Two separate servers rather than two routes on one, so "did the token reach the
    // redirect target" is answered by a counter that the first hop cannot touch.
    let entry = MockControlPlane::start().await;
    let exfiltration = MockControlPlane::start().await;
    entry
        .set_token_script(TokenScript::Redirect {
            status: StatusCode::TEMPORARY_REDIRECT,
            location: exfiltration.token_endpoint(),
        })
        .await;
    let actor = admin_actor();
    let admin = AdminService::new(&state).expect("admin service");

    let provider = admin
        .create_provider(
            &actor,
            &request_context(),
            ProviderCreateRequest {
                provider_type: ProviderType::Anthropic,
                display_name: "Redirecting IdP".to_string(),
                base_url: None,
                metadata: json!({ "oauth_token_endpoint": entry.token_endpoint() }),
            },
        )
        .await
        .expect("create provider");

    let expires_soon = Utc::now() + chrono::Duration::seconds(60);
    let credential = admin
        .create_credential(
            &actor,
            &request_context(),
            CredentialCreateRequest {
                provider_id: provider.id,
                credential_type: CredentialType::Oauth2,
                scope: CredentialScope::Global,
                secret: CredentialSecret::OAuth2 {
                    access_token: "old-access-token".to_string(),
                    refresh_token: Some(REDIRECT_TEST_REFRESH_TOKEN.to_string()),
                    token_type: Some("Bearer".to_string()),
                    expires_at: Some(expires_soon),
                },
                display_name: Some("Redirect test credential".to_string()),
                priority: 100,
                expires_at: Some(expires_soon),
                metadata: json!({}),
            },
        )
        .await
        .expect("create oauth2 credential");

    let settings = WorkerSettings::default();
    queue(&pool, settings.clone())
        .enqueue("oauth-token-refresh", json!({}), None, &metrics())
        .await
        .expect("enqueue oauth-token-refresh");
    let outcome = dispatch_one(&state, settings).await;

    assert_eq!(
        entry.token_call_count(),
        1,
        "the validated endpoint is allowed and must be called exactly once — otherwise this \
         test would pass for the wrong reason"
    );
    assert!(
        entry.observed_secret(REDIRECT_TEST_REFRESH_TOKEN).await,
        "sanity: the refresh token really is in the body of the request that was allowed, so \
         a followed redirect would carry it"
    );
    assert_eq!(
        exfiltration.token_call_count(),
        0,
        "a 307 from a validated token endpoint must not re-issue the exchange against the \
         Location target"
    );
    assert!(
        !exfiltration
            .observed_secret(REDIRECT_TEST_REFRESH_TOKEN)
            .await,
        "the decrypted refresh token must never reach the redirect target"
    );
    assert_eq!(
        outcome.completed, 1,
        "the job itself still completes; only this credential's refresh fails"
    );

    let admin_repo = PgAdminRepository::new(pool.clone());
    let unchanged = admin_repo
        .get_credential(credential.id)
        .await
        .expect("load the credential");
    assert_eq!(
        unchanged.version, credential.version,
        "a refresh that ended at a 3xx must not rewrite the credential"
    );

    entry.shutdown().await;
    exfiltration.shutdown().await;
}

/// A distinctive plaintext so `observed_secret` cannot match on something incidental.
const REDIRECT_TEST_REFRESH_TOKEN: &str = "redirect-probe-refresh-token-8f2a1c";

async fn status_of(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select status from worker_jobs where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the job row")
}

/// A minimal [`moira::app::AppState`] over `pool` — this file needs `AdminService` for
/// provider/credential setup but none of `LifecycleFixture`'s heavier routing/model/streaming
/// scaffolding, so it builds state directly rather than paying for that.
async fn test_state(pool: &PgPool) -> moira::app::AppState {
    let mut settings = moira::config::Settings::default();
    settings.provider_security.allow_http_provider_urls = true;
    settings.provider_security.allow_private_provider_urls = true;
    moira::app::AppState::new(settings, Some(pool.clone()))
        .await
        .expect("build test app state")
}

/// [`test_state`] with **neither** `provider_security` escape hatch — the shape every real
/// deployment has, and the only shape production can have (`Settings::validate_production`
/// rejects `allow_http_provider_urls`). Used by the token-endpoint SSRF test below; every
/// other test in this file wants the permissive fixture so it can talk to the loopback mock.
async fn strict_state(pool: &PgPool) -> moira::app::AppState {
    let settings = moira::config::Settings::default();
    assert!(
        !settings.provider_security.allow_http_provider_urls
            && !settings.provider_security.allow_private_provider_urls,
        "the shipped defaults must not grant either provider_security escape hatch"
    );
    moira::app::AppState::new(settings, Some(pool.clone()))
        .await
        .expect("build strict test app state")
}
