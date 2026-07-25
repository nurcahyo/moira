mod support;

use std::{future::Future, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    app::AppState,
    application::{AdminService, RequestContext},
    config::Settings,
    domain::{ApplicationCreateRequest, ProviderCreateRequest, ProviderType},
    security::{Actor, ActorType},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::{sync::Barrier, task::JoinHandle, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

use support::TestDatabase;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    pool: PgPool,
    state: AppState,
    router: Router,
    suffix: String,
    /// Declared last so the database is dropped after everything still connected to
    /// it. See [`support::TestDatabase`] for why this suite no longer needs the
    /// `SERIAL` mutex it used to hold.
    _database: TestDatabase,
}

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

impl Fixture {
    /// A fixture on its own PostgreSQL database (plan 06 module 13, P2-13).
    ///
    /// This suite used to carry its own `SERIAL` mutex, its own pool against the shared
    /// `moira` database, and its own `db::migrate` call — a second, independent copy of
    /// the harness in `tests/support`. The mutex bought nothing across binaries, and it
    /// could not isolate this suite's most dangerous fixtures at all: one test installs
    /// a trigger that makes **every** `audit_logs` insert fail, and another blocks the
    /// whole table with `access exclusive`, both of which were visible to any other
    /// suite running at the same time.
    ///
    /// A private database also scopes what these tests actually assert on:
    /// `idempotency_records` counts are now exact rather than "at least", and the
    /// advisory locks under `advisory_lock_key` can no longer collide with another
    /// suite's, because PostgreSQL advisory locks are per database.
    ///
    /// The pool stays at 16 connections — the barrier-released concurrency tests below
    /// fire batches of simultaneous requests and will exhaust a smaller one.
    async fn new() -> Option<Self> {
        let database = TestDatabase::create_with_max_connections(16).await?;
        let pool = database.pool.clone();
        let mut settings = Settings::default();
        settings.provider_security.allow_http_provider_urls = true;
        settings.provider_security.allow_private_provider_urls = true;
        let state = AppState::new(settings, Some(pool.clone())).expect("test app state");
        let router = moira::build_router(state.clone()).expect("test router");
        Some(Self {
            pool,
            state,
            router,
            suffix: Uuid::now_v7().simple().to_string(),
            _database: database,
        })
    }

    fn key(&self, label: &str) -> String {
        format!("idem-{label}-{}", self.suffix)
    }

    async fn post(
        &self,
        path: &str,
        key: Option<&str>,
        if_match: Option<i64>,
        body: Value,
    ) -> HttpResult {
        post(self.router.clone(), path, key, if_match, body).await
    }

    async fn create_application(&self, label: &str, key: Option<&str>) -> HttpResult {
        self.post(
            "/api/v1/admin/applications",
            key,
            None,
            json!({
                "application_slug": format!("{label}-{}", self.suffix),
                "display_name": format!("{label} {}", self.suffix),
                "metadata": {"suite": "admin_idempotency"}
            }),
        )
        .await
    }

    async fn create_provider(&self, label: &str, key: Option<&str>) -> HttpResult {
        self.post(
            "/api/v1/admin/providers",
            key,
            None,
            json!({
                "provider_type": "open_ai_compatible",
                "display_name": format!("{label} {}", self.suffix),
                "base_url": null,
                "metadata": {"suite": "admin_idempotency"}
            }),
        )
        .await
    }

    async fn count(&self, query: &str, value: &str) -> i64 {
        if query.contains("$1") {
            timeout(
                WAIT,
                sqlx::query_scalar::<_, i64>(query)
                    .bind(value)
                    .fetch_one(&self.pool),
            )
            .await
            .expect("count query timed out")
            .expect("count query")
        } else {
            timeout(
                WAIT,
                sqlx::query_scalar::<_, i64>(query).fetch_one(&self.pool),
            )
            .await
            .expect("count query timed out")
            .expect("count query")
        }
    }

    /// The stored `idempotency_key_hash` for a raw Idempotency-Key.
    ///
    /// Keyed by the deployment pepper since plan 03 (P1-1), so the ledger can no longer be
    /// probed with a bare SHA-256 of the key.
    fn key_hash(&self, key: &str) -> String {
        self.state.idempotency_hasher.hash(key.as_bytes())
    }

    async fn ledger_count(&self, operation: &str, key: &str) -> i64 {
        let key_hash = self.key_hash(key);
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records where operation = $1 and idempotency_key_hash = $2 and resource_id is not null",
            )
            .bind(operation)
            .bind(key_hash)
            .fetch_one(&self.pool),
        )
        .await
        .expect("ledger query timed out")
        .expect("ledger query")
    }
}

async fn post(
    router: Router,
    path: &str,
    key: Option<&str>,
    if_match: Option<i64>,
    body: Value,
) -> HttpResult {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-request-id", format!("admin-idem-{}", Uuid::now_v7()));
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    if let Some(version) = if_match {
        builder = builder.header("if-match", version.to_string());
    }
    let response = timeout(
        WAIT,
        router.oneshot(
            builder
                .body(Body::from(body.to_string()))
                .expect("HTTP request"),
        ),
    )
    .await
    .expect("HTTP request timed out")
    .expect("HTTP response");
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response")
    };
    HttpResult { status, body, etag }
}

fn id(value: &Value) -> Uuid {
    Uuid::parse_str(value["id"].as_str().expect("resource id")).expect("UUID resource id")
}

fn key_resource_id(value: &Value) -> Uuid {
    Uuid::parse_str(value["resource"]["id"].as_str().expect("key resource id"))
        .expect("UUID key resource id")
}

fn assert_error(result: &HttpResult, status: StatusCode, code: &str) {
    assert_eq!(result.status, status, "unexpected body: {}", result.body);
    assert_eq!(result.body["error"]["code"], code);
}

fn assert_replayed_error(first: &HttpResult, replay: &HttpResult) {
    assert_eq!(first.status, replay.status);
    let mut first_error = first.body["error"].clone();
    let mut replay_error = replay.body["error"].clone();
    let first_request_id = first_error
        .as_object_mut()
        .and_then(|error| error.remove("request_id"))
        .expect("first error request ID");
    let replay_request_id = replay_error
        .as_object_mut()
        .and_then(|error| error.remove("request_id"))
        .expect("replayed error request ID");
    assert_ne!(first_request_id, replay_request_id);
    assert_eq!(first_error, replay_error);
}

#[tokio::test]
async fn all_ten_admin_operation_identities_replay_the_same_resource() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let application_key = fixture.key("inventory-application");
    let application = fixture
        .create_application("inventory-app", Some(&application_key))
        .await;
    assert_eq!(application.status, StatusCode::CREATED);
    let application_replay = fixture
        .create_application("inventory-app", Some(&application_key))
        .await;
    assert_eq!(application_replay.status, StatusCode::CREATED);
    assert_eq!(id(&application.body), id(&application_replay.body));
    let application_id = id(&application.body);

    let provider_key = fixture.key("inventory-provider");
    let provider = fixture
        .create_provider("inventory-provider", Some(&provider_key))
        .await;
    assert_eq!(provider.status, StatusCode::CREATED);
    let provider_replay = fixture
        .create_provider("inventory-provider", Some(&provider_key))
        .await;
    assert_eq!(id(&provider.body), id(&provider_replay.body));
    let provider_id = id(&provider.body);

    let model_key = fixture.key("inventory-model");
    let model_path = format!("/api/v1/admin/providers/{provider_id}/models");
    let model_body = json!({
        "model_key": format!("model-{}", fixture.suffix),
        "display_name": "Idempotency model",
        "capabilities": {"streaming": true}
    });
    let model = fixture
        .post(&model_path, Some(&model_key), None, model_body.clone())
        .await;
    let model_replay = fixture
        .post(&model_path, Some(&model_key), None, model_body)
        .await;
    assert_eq!(model.status, StatusCode::CREATED);
    assert_eq!(id(&model.body), id(&model_replay.body));

    let credential_key = fixture.key("inventory-credential");
    let credential_body = json!({
        "provider_id": provider_id,
        "credential_type": "api_key",
        "scope": {"type": "global"},
        "secret": {"api_key": format!("secret-{}", fixture.suffix)},
        "display_name": "Idempotency credential",
        "priority": 10,
        "expires_at": null,
        "metadata": {"suite": "admin_idempotency"}
    });
    let credential = fixture
        .post(
            "/api/v1/admin/provider-credentials",
            Some(&credential_key),
            None,
            credential_body.clone(),
        )
        .await;
    let credential_replay = fixture
        .post(
            "/api/v1/admin/provider-credentials",
            Some(&credential_key),
            None,
            credential_body,
        )
        .await;
    assert_eq!(credential.status, StatusCode::CREATED);
    assert_eq!(id(&credential.body), id(&credential_replay.body));
    let credential_id = id(&credential.body);

    let credential_rotate_key = fixture.key("inventory-credential-rotate");
    let credential_rotate_path =
        format!("/api/v1/admin/provider-credentials/{credential_id}/rotate");
    let rotate_body = json!({"secret": {"api_key": format!("rotated-{}", fixture.suffix)}});
    let credential_rotated = fixture
        .post(
            &credential_rotate_path,
            Some(&credential_rotate_key),
            Some(
                credential.body["version"]
                    .as_i64()
                    .expect("credential version"),
            ),
            rotate_body.clone(),
        )
        .await;
    let credential_rotated_replay = fixture
        .post(
            &credential_rotate_path,
            Some(&credential_rotate_key),
            Some(
                credential.body["version"]
                    .as_i64()
                    .expect("credential version"),
            ),
            rotate_body,
        )
        .await;
    assert_eq!(credential_rotated.status, StatusCode::OK);
    assert_eq!(
        credential_rotated.body["version"],
        credential_rotated_replay.body["version"]
    );

    let system_key = fixture.key("inventory-system-key");
    let system_body = json!({
        "display_name": format!("Inventory system {}", fixture.suffix),
        "scopes": ["moira:applications:read"],
        "expires_at": null
    });
    let system = fixture
        .post(
            "/api/v1/admin/system-keys",
            Some(&system_key),
            None,
            system_body.clone(),
        )
        .await;
    let system_replay = fixture
        .post(
            "/api/v1/admin/system-keys",
            Some(&system_key),
            None,
            system_body,
        )
        .await;
    assert_eq!(system.status, StatusCode::CREATED);
    assert!(system.body["secret"].is_string());
    assert!(system_replay.body["secret"].is_null());
    let system_id = key_resource_id(&system.body);
    let system_rotate_key = fixture.key("inventory-system-rotate");
    let system_rotate_path = format!("/api/v1/admin/system-keys/{system_id}/rotate");
    let system_rotated = fixture
        .post(
            &system_rotate_path,
            Some(&system_rotate_key),
            None,
            json!({}),
        )
        .await;
    let system_rotated_replay = fixture
        .post(
            &system_rotate_path,
            Some(&system_rotate_key),
            None,
            json!({}),
        )
        .await;
    assert!(system_rotated.body["secret"].is_string());
    assert!(system_rotated_replay.body["secret"].is_null());

    let consumer_key = fixture.key("inventory-consumer-key");
    let consumer_body = json!({
        "application_id": application_id,
        "display_name": format!("Inventory consumer {}", fixture.suffix),
        "scopes": ["moira:responses:create"],
        "expires_at": null
    });
    let consumer = fixture
        .post(
            "/api/v1/admin/consumer-keys",
            Some(&consumer_key),
            None,
            consumer_body.clone(),
        )
        .await;
    let consumer_replay = fixture
        .post(
            "/api/v1/admin/consumer-keys",
            Some(&consumer_key),
            None,
            consumer_body,
        )
        .await;
    assert_eq!(consumer.status, StatusCode::CREATED);
    assert!(consumer.body["secret"].is_string());
    assert!(consumer_replay.body["secret"].is_null());
    let consumer_id = key_resource_id(&consumer.body);
    let consumer_rotate_key = fixture.key("inventory-consumer-rotate");
    let consumer_rotate_path = format!("/api/v1/admin/consumer-keys/{consumer_id}/rotate");
    let consumer_rotated = fixture
        .post(
            &consumer_rotate_path,
            Some(&consumer_rotate_key),
            None,
            json!({}),
        )
        .await;
    let consumer_rotated_replay = fixture
        .post(
            &consumer_rotate_path,
            Some(&consumer_rotate_key),
            None,
            json!({}),
        )
        .await;
    assert!(consumer_rotated.body["secret"].is_string());
    assert!(consumer_rotated_replay.body["secret"].is_null());

    let issuer_key = fixture.key("inventory-issuer");
    let issuer_body = json!({
        "issuer": format!("https://issuer-{}.example", fixture.suffix),
        "jwks_url": "https://issuer.example/jwks",
        "expected_audiences": [],
        "allowed_algorithms": ["RS256"],
        "subject_claim": "sub",
        "user_id_claim": null,
        "tenant_id_claim": null,
        "application_id_claim": "application_id",
        "roles_claim": null,
        "scopes_claim": "scope",
        "delegated_user_claim": null,
        "delegated_tenant_claim": null,
        "clock_skew_seconds": 30,
        "allow_delegation": false,
        "claim_mapping": null
    });
    let issuer = fixture
        .post(
            "/api/v1/admin/jwt-issuers",
            Some(&issuer_key),
            None,
            issuer_body.clone(),
        )
        .await;
    let issuer_replay = fixture
        .post(
            "/api/v1/admin/jwt-issuers",
            Some(&issuer_key),
            None,
            issuer_body,
        )
        .await;
    assert_eq!(issuer.status, StatusCode::CREATED);
    assert_eq!(id(&issuer.body), id(&issuer_replay.body));

    for (operation, key) in [
        ("application.create", &application_key),
        ("provider.create", &provider_key),
        ("provider_model.create", &model_key),
        ("credential.create", &credential_key),
        ("credential.rotate", &credential_rotate_key),
        ("system_key.create", &system_key),
        ("system_key.rotate", &system_rotate_key),
        ("consumer_key.create", &consumer_key),
        ("consumer_key.rotate", &consumer_rotate_key),
        ("jwt_issuer.create", &issuer_key),
    ] {
        assert_eq!(
            fixture.ledger_count(operation, key).await,
            1,
            "missing completed ledger for {operation}"
        );
    }
}

#[tokio::test]
async fn concurrent_same_key_create_has_one_resource_audit_and_ledger() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let key = fixture.key("concurrent-app");
    let path = "/api/v1/admin/applications";
    let body = json!({
        "application_slug": format!("race-{}", fixture.suffix),
        "display_name": format!("Race {}", fixture.suffix),
        "metadata": {}
    });
    let barrier = std::sync::Arc::new(Barrier::new(3));
    let first = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        path,
        key.clone(),
        None,
        body.clone(),
    );
    let second = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        path,
        key,
        None,
        body,
    );
    barrier.wait().await;
    let first = first.await.expect("first task");
    let second = second.await.expect("second task");
    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(id(&first.body), id(&second.body));
    let resource_id = id(&first.body).to_string();
    assert_eq!(
        fixture
            .count(
                "select count(*) from applications where id::text = $1",
                &resource_id
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(
                "select count(*) from audit_logs where action = 'application.create' and resource_id = $1 and result = 'success'",
                &resource_id,
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(
                "select count(*) from idempotency_records where operation = 'application.create' and resource_id = $1 and response_status = 201",
                &resource_id,
            )
            .await,
        1
    );
}

#[tokio::test]
async fn command_hash_covers_request_path_and_version_while_scope_covers_actor_and_operation() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let key = fixture.key("hash-conflict");
    let first = fixture.create_application("hash-a", Some(&key)).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let changed = fixture.create_application("hash-b", Some(&key)).await;
    assert_error(&changed, StatusCode::CONFLICT, "idempotency_conflict");

    let system_a = fixture
        .post(
            "/api/v1/admin/system-keys",
            None,
            None,
            json!({"display_name": format!("Path A {}", fixture.suffix), "scopes": ["moira:applications:read"], "expires_at": null}),
        )
        .await;
    let system_b = fixture
        .post(
            "/api/v1/admin/system-keys",
            None,
            None,
            json!({"display_name": format!("Path B {}", fixture.suffix), "scopes": ["moira:applications:read"], "expires_at": null}),
        )
        .await;
    let path_key = fixture.key("path-conflict");
    let rotate_a = format!(
        "/api/v1/admin/system-keys/{}/rotate",
        key_resource_id(&system_a.body)
    );
    let rotate_b = format!(
        "/api/v1/admin/system-keys/{}/rotate",
        key_resource_id(&system_b.body)
    );
    assert_eq!(
        fixture
            .post(&rotate_a, Some(&path_key), None, json!({}))
            .await
            .status,
        StatusCode::OK
    );
    let wrong_path = fixture
        .post(&rotate_b, Some(&path_key), None, json!({}))
        .await;
    assert_error(&wrong_path, StatusCode::CONFLICT, "idempotency_conflict");

    let actor_key = fixture.key("actor-operation-reuse");
    let actor_one = admin_actor("actor-one");
    let actor_two = admin_actor("actor-two");
    let request_one = application_request(&fixture, "actor-one");
    let request_two = application_request(&fixture, "actor-two");
    let context = context(&actor_key);
    AdminService::new(&fixture.state)
        .expect("admin service")
        .create_application(&actor_one, &context, request_one)
        .await
        .expect("first actor application");
    AdminService::new(&fixture.state)
        .expect("admin service")
        .create_application(&actor_two, &context, request_two)
        .await
        .expect("second actor application");
    AdminService::new(&fixture.state)
        .expect("admin service")
        .create_provider(
            &actor_one,
            &context,
            ProviderCreateRequest {
                provider_type: ProviderType::OpenAiCompatible,
                display_name: format!("Actor provider {}", fixture.suffix),
                base_url: None,
                metadata: json!({}),
            },
        )
        .await
        .expect("same key different operation");
    assert_eq!(
        fixture.ledger_count("application.create", &actor_key).await,
        2
    );
    assert_eq!(fixture.ledger_count("provider.create", &actor_key).await, 1);
}

#[tokio::test]
async fn trusted_actor_fingerprint_isolates_issuer_and_application_identity() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let service = AdminService::new(&fixture.state).expect("admin service");
    let key = fixture.key("trusted-identity");
    let first_application = fixture.create_application("trusted-first", None).await;
    let second_application = fixture.create_application("trusted-second", None).await;
    let request = ProviderCreateRequest {
        provider_type: ProviderType::OpenAiCompatible,
        display_name: format!("Trusted identity {}", fixture.suffix),
        base_url: None,
        metadata: json!({}),
    };
    let issuer_id = Uuid::now_v7();
    let first = Actor {
        actor_type: ActorType::TrustedJwt,
        subject: Some("shared-subject".to_string()),
        trusted_jwt_issuer_id: Some(issuer_id),
        internal_application_id: Some(id(&first_application.body)),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    };
    let second = Actor {
        trusted_jwt_issuer_id: Some(Uuid::now_v7()),
        ..first.clone()
    };
    let third = Actor {
        internal_application_id: Some(id(&second_application.body)),
        ..first.clone()
    };

    let first_record = service
        .create_provider(&first, &context(&key), request.clone())
        .await
        .expect("first trusted actor command");
    let second_record = service
        .create_provider(&second, &context(&key), request.clone())
        .await
        .expect("different issuer command");
    let third_record = service
        .create_provider(&third, &context(&key), request)
        .await
        .expect("different application command");

    assert_ne!(first_record.id, second_record.id);
    assert_ne!(first_record.id, third_record.id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from idempotency_records where operation = 'provider.create' and idempotency_key_hash = $1",
        )
        .bind(fixture.key_hash(&key))
        .fetch_one(&fixture.pool)
        .await
        .expect("trusted actor ledger count"),
        3
    );
}

#[tokio::test]
async fn expired_record_is_reclaimed_and_deterministic_failure_is_replayed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let key = fixture.key("expired");
    let first = fixture.create_application("expired-a", Some(&key)).await;
    assert_eq!(first.status, StatusCode::CREATED);
    sqlx::query(
        "update idempotency_records set expires_at = now() - interval '1 second' where resource_id = $1",
    )
    .bind(id(&first.body).to_string())
    .execute(&fixture.pool)
    .await
    .expect("expire idempotency record");
    let reused = fixture.create_application("expired-b", Some(&key)).await;
    assert_eq!(reused.status, StatusCode::CREATED);
    assert_ne!(id(&first.body), id(&reused.body));

    let failure_key = fixture.key("failure-replay");
    let invalid = json!({
        "application_slug": format!("invalid-{}", fixture.suffix),
        "display_name": "",
        "metadata": {}
    });
    let first_failure = fixture
        .post(
            "/api/v1/admin/applications",
            Some(&failure_key),
            None,
            invalid.clone(),
        )
        .await;
    let replayed_failure = fixture
        .post(
            "/api/v1/admin/applications",
            Some(&failure_key),
            None,
            invalid,
        )
        .await;
    assert_error(&first_failure, StatusCode::BAD_REQUEST, "bad_request");
    assert_error(&replayed_failure, StatusCode::BAD_REQUEST, "bad_request");
    assert_replayed_error(&first_failure, &replayed_failure);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from idempotency_records where operation = 'application.create' and idempotency_key_hash = $1 and response_status = 400 and resource_id is null",
        )
        .bind(fixture.key_hash(&failure_key))
        .fetch_one(&fixture.pool)
        .await
        .expect("cached deterministic failure"),
        1
    );

    let invalid_scope_key = fixture.key("invalid-scope");
    let invalid_scope = json!({
        "display_name": "Invalid scope",
        "scopes": ["moira:not-a-real-scope"],
        "expires_at": null
    });
    let first_scope_failure = fixture
        .post(
            "/api/v1/admin/system-keys",
            Some(&invalid_scope_key),
            None,
            invalid_scope.clone(),
        )
        .await;
    let replayed_scope_failure = fixture
        .post(
            "/api/v1/admin/system-keys",
            Some(&invalid_scope_key),
            None,
            invalid_scope,
        )
        .await;
    assert_error(
        &first_scope_failure,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scope_invalid",
    );
    assert_replayed_error(&first_scope_failure, &replayed_scope_failure);
}

#[tokio::test]
async fn advisory_lock_timeout_is_transient_and_does_not_mutate_or_cache() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let seed_key = fixture.key("lock-seed");
    let seed = fixture
        .create_application("lock-seed", Some(&seed_key))
        .await;
    assert_eq!(seed.status, StatusCode::CREATED);
    let actor_fingerprint = sqlx::query_scalar::<_, String>(
        "select actor_fingerprint from idempotency_records where operation = 'application.create' and idempotency_key_hash = $1",
    )
    .bind(fixture.key_hash(&seed_key))
    .fetch_one(&fixture.pool)
    .await
    .expect("seed actor fingerprint");

    let key = fixture.key("lock-timeout");
    let lock_key = advisory_lock_key(
        &fixture.key_hash(&key),
        &actor_fingerprint,
        "application.create",
    );
    let mut lock_connection = fixture.pool.acquire().await.expect("lock connection");
    sqlx::query("select pg_advisory_lock($1)")
        .bind(lock_key)
        .execute(&mut *lock_connection)
        .await
        .expect("hold idempotency advisory lock");

    let slug = format!("lock-timeout-{}", fixture.suffix);
    let result = timeout(
        Duration::from_secs(7),
        fixture.post(
            "/api/v1/admin/applications",
            Some(&key),
            None,
            json!({
                "application_slug": slug,
                "display_name": format!("Lock timeout {}", fixture.suffix),
                "metadata": {}
            }),
        ),
    )
    .await
    .expect("idempotency lock timeout response");
    sqlx::query("select pg_advisory_unlock($1)")
        .bind(lock_key)
        .execute(&mut *lock_connection)
        .await
        .expect("release idempotency advisory lock");

    assert_error(&result, StatusCode::CONFLICT, "idempotency_in_progress");
    assert_eq!(
        fixture
            .count(
                "select count(*) from applications where application_slug = $1",
                &format!("lock-timeout-{}", fixture.suffix),
            )
            .await,
        0
    );
    assert_eq!(fixture.ledger_count("application.create", &key).await, 0);
}

#[tokio::test]
async fn audit_failure_and_request_cancellation_roll_back_mutation_and_ledger() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let marker = format!("audit_fail_{}", fixture.suffix);
    let function_name = format!("fail_audit_{}", fixture.suffix);
    let trigger_name = format!("fail_audit_trigger_{}", fixture.suffix);
    let ddl = format!(
        "create function {function_name}() returns trigger language plpgsql as $$ begin if new.request_id = '{marker}' then raise exception 'scripted audit failure'; end if; return new; end $$; create trigger {trigger_name} before insert on audit_logs for each row execute function {function_name}()"
    );
    sqlx::raw_sql(&ddl)
        .execute(&fixture.pool)
        .await
        .expect("install audit failure trigger");
    let failed_key = fixture.key("audit-failure");
    let failed_body = json!({
        "application_slug": format!("audit-failure-{}", fixture.suffix),
        "display_name": format!("Audit failure {}", fixture.suffix),
        "metadata": {}
    });
    let failed = request_with_id(
        fixture.router.clone(),
        "/api/v1/admin/applications",
        Some(&failed_key),
        &marker,
        failed_body,
    )
    .await;
    let no_key_slug = format!("audit-failure-no-key-{}", fixture.suffix);
    let no_key_failed = request_with_id(
        fixture.router.clone(),
        "/api/v1/admin/applications",
        None,
        &marker,
        json!({
            "application_slug": no_key_slug,
            "display_name": format!("Audit failure no key {}", fixture.suffix),
            "metadata": {}
        }),
    )
    .await;
    let cleanup =
        format!("drop trigger {trigger_name} on audit_logs; drop function {function_name}()");
    sqlx::raw_sql(&cleanup)
        .execute(&fixture.pool)
        .await
        .expect("remove audit failure trigger");
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(no_key_failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        fixture
            .count(
                "select count(*) from applications where application_slug = $1",
                &format!("audit-failure-{}", fixture.suffix),
            )
            .await,
        0
    );
    assert_eq!(
        fixture
            .count(
                "select count(*) from applications where application_slug = $1",
                &format!("audit-failure-no-key-{}", fixture.suffix),
            )
            .await,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from idempotency_records where operation = 'application.create' and idempotency_key_hash = $1",
        )
        .bind(fixture.key_hash(&failed_key))
        .fetch_one(&fixture.pool)
        .await
        .expect("audit-failure ledger count"),
        0
    );

    let mut blocker = fixture
        .pool
        .begin()
        .await
        .expect("audit blocker transaction");
    sqlx::query("lock table audit_logs in access exclusive mode")
        .execute(&mut *blocker)
        .await
        .expect("lock audit table");
    let cancelled_key = fixture.key("cancelled");
    let cancelled_slug = format!("cancelled-{}", fixture.suffix);
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/applications")
        .header("content-type", "application/json")
        .header("idempotency-key", &cancelled_key)
        .header("x-request-id", format!("cancelled-{}", fixture.suffix))
        .body(Body::from(
            json!({
                "application_slug": cancelled_slug,
                "display_name": format!("Cancelled {}", fixture.suffix),
                "metadata": {}
            })
            .to_string(),
        ))
        .expect("cancelled request");
    let task = tokio::spawn(fixture.router.clone().oneshot(request));
    wait_for_audit_lock(&fixture.pool).await;
    task.abort();
    let _ = task.await;
    blocker.rollback().await.expect("release audit lock");

    // P2-12. Cancellation itself is already observed above — `task.abort()` plus
    // `task.await`, then the blocker's `rollback()` — but the aborted request's own
    // transaction unwinds on a pooled connection afterwards, so its rollback is not
    // guaranteed visible the instant `rollback()` returns. This used to be a
    // `sleep(50ms)`: a guess that passed by being generous and would have gone quietly
    // green had the rollback stopped happening at all, because the counts it guarded
    // are asserted to be *zero*. Polling the two counters instead is fail-loud — a row
    // that is committed rather than rolled back keeps the condition false until the
    // budget expires, and the assertion below reports the counts it actually saw.
    let cancelled_slug_value = format!("cancelled-{}", fixture.suffix);
    let cancelled_key_hash = fixture.key_hash(&cancelled_key);
    let application_count = "select count(*) from applications where application_slug = $1";
    let ledger_count = "select count(*) from idempotency_records \
         where operation = 'application.create' and idempotency_key_hash = $1";
    let rolled_back = poll_until(WAIT, || async {
        fixture
            .count(application_count, &cancelled_slug_value)
            .await
            == 0
            && fixture.count(ledger_count, &cancelled_key_hash).await == 0
    })
    .await;
    assert!(
        rolled_back,
        "a cancelled command must leave neither an application row nor a ledger row; after \
         {WAIT:?} there were {} application rows and {} ledger rows",
        fixture
            .count(application_count, &cancelled_slug_value)
            .await,
        fixture.count(ledger_count, &cancelled_key_hash).await,
    );
}

#[tokio::test]
async fn credential_rotation_enforces_if_match_atomically() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let provider = fixture.create_provider("rotation-provider", None).await;
    let provider_id = id(&provider.body);
    let credential = fixture
        .post(
            "/api/v1/admin/provider-credentials",
            None,
            None,
            json!({
                "provider_id": provider_id,
                "credential_type": "api_key",
                "scope": {"type": "global"},
                "secret": {"api_key": format!("initial-{}", fixture.suffix)},
                "display_name": "Rotation credential",
                "priority": 1,
                "expires_at": null,
                "metadata": {}
            }),
        )
        .await;
    let credential_id = id(&credential.body);
    let version = credential.body["version"].as_i64().expect("version");
    let path = format!("/api/v1/admin/provider-credentials/{credential_id}/rotate");
    let key = fixture.key("credential-version");
    let body = json!({"secret": {"api_key": format!("next-{}", fixture.suffix)}});
    let barrier = std::sync::Arc::new(Barrier::new(3));
    let first = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        path.clone(),
        key.clone(),
        Some(version),
        body.clone(),
    );
    let second = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        path.clone(),
        key.clone(),
        Some(version),
        body.clone(),
    );
    barrier.wait().await;
    let rotated = first.await.expect("first credential rotation");
    let replay = second.await.expect("second credential rotation");
    assert_eq!(rotated.status, StatusCode::OK);
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(rotated.body["version"], version + 1);
    assert_eq!(replay.body["version"], version + 1);
    assert_eq!(rotated.etag, Some(format!("\"{}\"", version + 1)));
    assert_eq!(replay.etag, rotated.etag);
    let changed_precondition = fixture
        .post(&path, Some(&key), Some(version + 1), body)
        .await;
    assert_error(
        &changed_precondition,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );

    let stale = fixture
        .post(
            &path,
            Some(&fixture.key("credential-stale")),
            Some(version),
            json!({"secret": {"api_key": "stale-must-not-apply"}}),
        )
        .await;
    assert_error(&stale, StatusCode::CONFLICT, "resource_version_conflict");
    let stale_replay = fixture
        .post(
            &path,
            Some(&fixture.key("credential-stale")),
            Some(version),
            json!({"secret": {"api_key": "stale-must-not-apply"}}),
        )
        .await;
    assert_error(
        &stale_replay,
        StatusCode::CONFLICT,
        "resource_version_conflict",
    );
    assert_replayed_error(&stale, &stale_replay);

    let missing_path = format!(
        "/api/v1/admin/provider-credentials/{}/rotate",
        Uuid::now_v7()
    );
    let missing_key = fixture.key("credential-missing");
    let missing_body = json!({"secret": {"api_key": "missing-credential"}});
    let missing = fixture
        .post(
            &missing_path,
            Some(&missing_key),
            Some(1),
            missing_body.clone(),
        )
        .await;
    let missing_replay = fixture
        .post(&missing_path, Some(&missing_key), Some(1), missing_body)
        .await;
    assert_error(&missing, StatusCode::NOT_FOUND, "not_found");
    assert_replayed_error(&missing, &missing_replay);
    assert_eq!(
        fixture
            .count(
                "select count(*) from audit_logs where action = 'credential.rotate' and resource_id = $1 and result = 'success'",
                &credential_id.to_string(),
            )
            .await,
        1
    );
}

#[tokio::test]
async fn concurrent_key_create_returns_plaintext_to_exactly_one_caller() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let key = fixture.key("one-winner-secret");
    let body = json!({
        "display_name": format!("One winner {}", fixture.suffix),
        "scopes": ["moira:applications:read"],
        "expires_at": null
    });
    let barrier = std::sync::Arc::new(Barrier::new(3));
    let first = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        "/api/v1/admin/system-keys",
        key.clone(),
        None,
        body.clone(),
    );
    let second = spawn_post(
        fixture.router.clone(),
        barrier.clone(),
        "/api/v1/admin/system-keys",
        key,
        None,
        body,
    );
    barrier.wait().await;
    let first = first.await.expect("first key task");
    let second = second.await.expect("second key task");
    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(
        usize::from(first.body["secret"].is_string())
            + usize::from(second.body["secret"].is_string()),
        1
    );
    let raw_secret = first.body["secret"]
        .as_str()
        .or_else(|| second.body["secret"].as_str())
        .expect("one winning response contains the one-time secret");
    assert_eq!(
        usize::from(first.body["secret_retrievable"] == true)
            + usize::from(second.body["secret_retrievable"] == true),
        1
    );
    assert_eq!(key_resource_id(&first.body), key_resource_id(&second.body));
    let resource_id = key_resource_id(&first.body).to_string();
    assert_eq!(
        fixture
            .count(
                "select count(*) from system_api_keys where id::text = $1",
                &resource_id
            )
            .await,
        1
    );
    assert_eq!(
        fixture
            .count(
                "select count(*) from audit_logs where action = 'system_key.create' and resource_id = $1",
                &resource_id,
            )
            .await,
        1
    );
    let replay_json: Value = sqlx::query_scalar(
        "select response_body from idempotency_records where operation = 'system_key.create' and resource_id = $1",
    )
    .bind(&resource_id)
    .fetch_one(&fixture.pool)
    .await
    .expect("stored key replay");
    assert_eq!(replay_json["kind"], "success");
    assert!(replay_json["body"]["secret"].is_null());
    assert_eq!(replay_json["body"]["secret_retrievable"], false);
    assert!(!replay_json.to_string().contains(raw_secret));
}

fn spawn_post(
    router: Router,
    barrier: std::sync::Arc<Barrier>,
    path: impl Into<String>,
    key: String,
    if_match: Option<i64>,
    body: Value,
) -> JoinHandle<HttpResult> {
    let path = path.into();
    tokio::spawn(async move {
        barrier.wait().await;
        post(router, &path, Some(&key), if_match, body).await
    })
}

async fn request_with_id(
    router: Router,
    path: &str,
    key: Option<&str>,
    request_id: &str,
    body: Value,
) -> HttpResult {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-request-id", request_id);
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let response = router
        .oneshot(builder.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    HttpResult {
        status,
        body: serde_json::from_slice(&bytes).expect("error JSON"),
        etag,
    }
}

/// Polls `condition` on a fixed tick until it holds or `budget` expires.
///
/// The house pattern for waiting on something that has no signal to subscribe to —
/// see `tests/retention_worker.rs::poll_until`, which this mirrors. It is *not* a
/// `sleep`-based interleave (P2-12, `plans/CONVENTIONS.md` §3): every wait is bounded,
/// so a condition that never holds fails the test loudly instead of a timing guess
/// silently passing.
async fn poll_until<F, Fut>(budget: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    timeout(budget, async move {
        loop {
            ticker.tick().await;
            if condition().await {
                return;
            }
        }
    })
    .await
    .is_ok()
}

/// Waits until the spawned request is parked on the `audit_logs` lock the test holds.
///
/// `datname = current_database()` matters now that every fixture owns its own database
/// (module 13): without it this would happily match a *different* suite's blocked audit
/// insert and return before this fixture's request had reached the lock at all.
async fn wait_for_audit_lock(pool: &PgPool) {
    let blocked = poll_until(WAIT, || async {
        sqlx::query_scalar::<_, bool>(
            "select exists(\
                 select 1 from pg_stat_activity \
                 where datname = current_database() \
                   and wait_event_type = 'Lock' \
                   and query ilike '%insert into audit_logs%')",
        )
        .fetch_one(pool)
        .await
        .expect("inspect blocked audit insert")
    })
    .await;
    assert!(blocked, "command never blocked on audit insert");
}

fn admin_actor(subject: &str) -> Actor {
    Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some(subject.to_string()),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    }
}

fn advisory_lock_key(key_hash: &str, actor_fingerprint: &str, operation: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(key_hash.as_bytes());
    hasher.update([0]);
    hasher.update(actor_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(operation.as_bytes());
    let digest = hasher.finalize();
    i64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"))
}

fn context(key: &str) -> RequestContext {
    RequestContext {
        request_id: format!("admin-idem-{}", Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("admin-idempotency-test".to_string()),
        idempotency_key: Some(key.to_string()),
    }
}

fn application_request(fixture: &Fixture, label: &str) -> ApplicationCreateRequest {
    ApplicationCreateRequest {
        external_application_id: None,
        application_slug: Some(format!("{label}-{}", fixture.suffix)),
        display_name: format!("{label} {}", fixture.suffix),
        metadata: json!({}),
    }
}
