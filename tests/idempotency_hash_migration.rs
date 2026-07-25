//! Plan 03 / finding P1-1, Verification layer 2 — the idempotency hashing switch.
//!
//! `idempotency_records.request_hash` and `.idempotency_key_hash` moved from an unkeyed
//! SHA-256 to a versioned HMAC (`"v1:{base64url}"`). The riskiest part of that change is
//! **not** the hashing: it is that a row written before the switch must keep replaying
//! afterwards. That is untestable without a real database, because two *independent*
//! dimensions have to survive:
//!
//! 1. the **compared** value — `request_hash`, checked by
//!    `AdminIdempotencyClaim::matches_stored_request_hash`; and
//! 2. the **indexed** value — `idempotency_key_hash`, which is part of the unique index
//!    `(idempotency_key_hash, actor_fingerprint, operation)`
//!    (`migrations/0003_security_foundation.sql:360-361`) and therefore the *lookup* key.
//!
//! A test that only exercised (1) would pass while every replay lookup silently missed
//! and re-executed the command. Both dimensions are asserted here, over the real HTTP
//! surface, against a real PostgreSQL.
//!
//! Fail-closed rule: the skip decision lives in `tests/support/mod.rs` and is reused
//! rather than re-implemented — `MOIRA_TEST_DATABASE_URL` missing while `CI=true`
//! panics; otherwise the suite returns early.
//!
//! **All three ledgers are covered, not just the admin-command one.** Three independent
//! code paths read and write `idempotency_records`, and they do *not* share a read
//! implementation:
//!
//! * `src/application/admin_command.rs` compares via
//!   `AdminIdempotencyClaim::matches_stored_request_hash` and never calls
//!   `IdempotencyHasher::verify` at all;
//! * `src/application/public.rs` (`/api/v1/responses`, `/v1/responses`) calls `verify`;
//! * `src/application/runtime_admin.rs` calls `verify`.
//!
//! A review mutation that disabled the legacy branch of `verify` outright
//! (`None => false`) left this file 7/7 green, because every test here drove only the
//! admin-command path. The two `verify`-using paths — the ones carrying provider
//! credentials and runtime policy — had no legacy-downgrade coverage at any level. They
//! do now: `legacy_public_response_row_still_replays_after_the_hmac_switch` and
//! `legacy_runtime_admin_row_still_replays_after_the_hmac_switch`.
//!
//! Cross-test isolation: every fixture-owned identifier (idempotency keys, application
//! slugs, conversation titles) carries a per-fixture `Uuid::now_v7()` suffix, so two
//! test binaries running concurrently against the same database cannot collide on a
//! ledger row or a unique index.

mod support;

use std::{sync::Arc, time::Duration};

use moira::{
    application::{AdminCommandSpec, AdminService},
    domain::{
        ApplicationCreateRequest, ConsumerKeyCreateRequest, PublicResponseRequest,
        RouteDefinitionCreateRequest, RouteSelectionStrategy,
    },
    security::IdempotencyHasher,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tokio::{sync::Barrier, time::timeout};
use uuid::Uuid;

use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    public_response_request, request_context,
};

const WAIT: Duration = Duration::from_secs(20);
const OPERATION: &str = "application.create";
const RESPONSE_OPERATION: &str = "response.create";
const ROUTE_OPERATION: &str = "route.create";

struct HashFixture {
    fixture: LifecycleFixture,
    server: MoiraHttpServer,
    client: Client,
    hasher: IdempotencyHasher,
    suffix: String,
}

struct HttpResult {
    status: StatusCode,
    body: Value,
}

impl HttpResult {
    fn field(&self, name: &str) -> &Value {
        self.body
            .get(name)
            .unwrap_or_else(|| panic!("response has no `{name}` field: {}", self.body))
    }

    fn id(&self) -> String {
        self.field("id")
            .as_str()
            .unwrap_or_else(|| panic!("`id` is not a string: {}", self.body))
            .to_string()
    }

    fn error(&self) -> &Value {
        self.body
            .get("error")
            .unwrap_or_else(|| panic!("response is not an error envelope: {}", self.body))
    }
}

impl HashFixture {
    async fn new() -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let hasher = IdempotencyHasher::new(
            fixture
                .state
                .settings
                .idempotency
                .pepper_bytes()
                .expect("the test profile resolves an idempotency pepper"),
            fixture.state.settings.idempotency.pepper_version.clone(),
        );
        let server = MoiraHttpServer::start(fixture.state.clone()).await;
        Some(Self {
            fixture,
            server,
            client: Client::new(),
            hasher,
            suffix: Uuid::now_v7().simple().to_string(),
        })
    }

    /// Every fixture-owned identifier is suffixed, so concurrent test binaries sharing
    /// one database never contend on the same ledger row or unique index.
    fn key(&self, label: &str) -> String {
        format!("idem-hash-{label}-{}", self.suffix)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.base_url)
    }

    /// The exact struct the route deserialises, so the canonical envelope this test
    /// hashes is byte-identical to the one the production command runner hashes.
    fn application_request(&self, label: &str) -> ApplicationCreateRequest {
        ApplicationCreateRequest {
            external_application_id: Some(format!("{label}-{}", self.suffix)),
            application_slug: Some(format!("{label}-{}", self.suffix)),
            display_name: format!("{label} {}", self.suffix),
            metadata: json!({ "suite": "idempotency_hash_migration" }),
        }
    }

    fn spec(&self, request: &ApplicationCreateRequest) -> AdminCommandSpec {
        AdminCommandSpec::new(OPERATION, json!({}), request).expect("admin command spec")
    }

    async fn post_application(
        &self,
        key: Option<&str>,
        request: &ApplicationCreateRequest,
    ) -> HttpResult {
        self.post(
            "/api/v1/admin/applications",
            key,
            serde_json::to_value(request).expect("serialise application request"),
        )
        .await
    }

    async fn post(&self, path: &str, key: Option<&str>, body: Value) -> HttpResult {
        let mut builder = self
            .client
            .post(self.url(path))
            .header("content-type", "application/json")
            .header("x-request-id", format!("idem-hash-{}", Uuid::now_v7()))
            .body(body.to_string());
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        let response = timeout(WAIT, builder.send())
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
        let status = response.status();
        let bytes = response.bytes().await.expect("response body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)))
        };
        HttpResult { status, body }
    }

    async fn ledger_hashes(&self, resource_id: &str) -> (String, String) {
        self.ledger_hashes_for(OPERATION, resource_id).await
    }

    async fn ledger_hashes_for(&self, operation: &str, resource_id: &str) -> (String, String) {
        timeout(
            WAIT,
            sqlx::query_as::<_, (String, String)>(
                "select idempotency_key_hash, request_hash
                 from idempotency_records
                 where resource_id = $1 and operation = $2",
            )
            .bind(resource_id)
            .bind(operation)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ledger lookup timed out")
        .expect("exactly one ledger row for this resource")
    }

    /// Rewrites one ledger row into the **pre-deploy** format on both dimensions at once:
    /// the indexed `idempotency_key_hash` and the compared `request_hash`.
    ///
    /// Unlike [`Self::downgrade_to_legacy`] this takes the canonical request bytes rather
    /// than an `AdminCommandSpec`, so it works for any of the three ledger users. It
    /// asserts first that the caller's bytes really are the ones production hashed — a
    /// mis-derived canonical envelope would otherwise silently turn the replay assertion
    /// into a tautology.
    async fn downgrade_row_to_legacy(
        &self,
        operation: &str,
        resource_id: &str,
        key: &str,
        request_bytes: &[u8],
    ) -> (String, String) {
        let (versioned_key_hash, versioned_request_hash) =
            self.ledger_hashes_for(operation, resource_id).await;
        assert_eq!(
            versioned_key_hash,
            self.hasher.hash(key.as_bytes()),
            "{operation}: production did not write the keyed HMAC of this Idempotency-Key"
        );
        assert_eq!(
            versioned_request_hash,
            self.hasher.hash(request_bytes),
            "{operation}: the canonical bytes this test derived are not the ones production \
             hashed, so downgrading them would prove nothing"
        );

        let legacy_key_hash = self.hasher.legacy_hash(key.as_bytes());
        let legacy_request_hash = self.hasher.legacy_hash(request_bytes);
        assert_ne!(legacy_key_hash, versioned_key_hash);
        assert_ne!(legacy_request_hash, versioned_request_hash);
        assert!(
            !legacy_key_hash.contains(':') && !legacy_request_hash.contains(':'),
            "pre-deploy values carry no version prefix"
        );

        let affected = timeout(
            WAIT,
            sqlx::query(
                "update idempotency_records
                 set idempotency_key_hash = $1, request_hash = $2
                 where resource_id = $3 and operation = $4",
            )
            .bind(&legacy_key_hash)
            .bind(&legacy_request_hash)
            .bind(resource_id)
            .bind(operation)
            .execute(&self.fixture.pool),
        )
        .await
        .expect("ledger downgrade timed out")
        .expect("ledger downgrade")
        .rows_affected();
        assert_eq!(affected, 1, "exactly one ledger row must be downgraded");

        (legacy_key_hash, legacy_request_hash)
    }

    async fn ledger_count_for(&self, operation: &str, key: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records
                 where idempotency_key_hash = any($1) and operation = $2",
            )
            .bind(vec![
                self.hasher.hash(key.as_bytes()),
                self.hasher.legacy_hash(key.as_bytes()),
            ])
            .bind(operation)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ledger count timed out")
        .expect("ledger count")
    }

    /// Rewrites the ledger row this request produced into the **pre-deploy** format:
    /// both the indexed key hash and the compared body hash become the legacy unkeyed
    /// SHA-256 values. Everything else on the row (actor fingerprint, operation, stored
    /// response) stays exactly as production wrote it.
    async fn downgrade_to_legacy(
        &self,
        resource_id: &str,
        key: &str,
        request: &ApplicationCreateRequest,
    ) -> (String, String) {
        let legacy_key_hash = self.hasher.legacy_hash(key.as_bytes());
        let legacy_request_hash = self
            .spec(request)
            .legacy_request_hash()
            .expect("legacy request hash");

        let (versioned_key_hash, versioned_request_hash) = self.ledger_hashes(resource_id).await;
        assert_ne!(
            legacy_key_hash, versioned_key_hash,
            "the legacy key hash must actually differ from the versioned one, or this \
             test proves nothing"
        );
        assert_ne!(
            legacy_request_hash, versioned_request_hash,
            "the legacy body hash must actually differ from the versioned one"
        );
        assert!(
            !legacy_key_hash.contains(':') && !legacy_request_hash.contains(':'),
            "pre-deploy values carry no version prefix"
        );

        let affected = timeout(
            WAIT,
            sqlx::query(
                "update idempotency_records
                 set idempotency_key_hash = $1, request_hash = $2
                 where resource_id = $3 and operation = $4",
            )
            .bind(&legacy_key_hash)
            .bind(&legacy_request_hash)
            .bind(resource_id)
            .bind(OPERATION)
            .execute(&self.fixture.pool),
        )
        .await
        .expect("ledger downgrade timed out")
        .expect("ledger downgrade")
        .rows_affected();
        assert_eq!(affected, 1, "exactly one ledger row must be downgraded");

        (legacy_key_hash, legacy_request_hash)
    }

    /// `POST /api/v1/responses` as a consumer key, carrying an `Idempotency-Key`.
    ///
    /// Nothing sent the header on this route before: `tests/support/mod.rs` hard-coded
    /// `idempotency_key: None`, so `/v1/responses` had zero idempotency coverage at any
    /// level even though it is the route whose bodies carry credential material.
    async fn post_public_response(
        &self,
        consumer_key: &str,
        key: &str,
        request: &PublicResponseRequest,
    ) -> HttpResult {
        let response = timeout(
            WAIT,
            self.client
                .post(self.url("/api/v1/responses"))
                .header("content-type", "application/json")
                .header("x-consumer-key", consumer_key)
                .header("idempotency-key", key)
                .header("x-request-id", format!("idem-public-{}", Uuid::now_v7()))
                .body(serde_json::to_string(request).expect("serialise public response request"))
                .send(),
        )
        .await
        .expect("public response request timed out")
        .expect("public response");
        let status = response.status();
        let bytes = response.bytes().await.expect("response body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)))
        };
        HttpResult { status, body }
    }

    async fn count(&self, query: &str, value: &str) -> i64 {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(query)
                .bind(value)
                .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("count query timed out")
        .expect("count query")
    }

    async fn application_count(&self, slug: &str) -> i64 {
        self.count(
            "select count(*) from applications where application_slug = $1",
            slug,
        )
        .await
    }

    async fn route_count(&self, route_key: &str) -> i64 {
        self.count(
            "select count(*) from route_definitions where route_key = $1",
            route_key,
        )
        .await
    }

    /// The exact struct `POST /api/v1/admin/routes` deserialises, so the canonical
    /// envelope this test hashes is byte-identical to the one `runtime_admin.rs` hashes.
    fn route_request(&self, label: &str) -> RouteDefinitionCreateRequest {
        RouteDefinitionCreateRequest {
            route_key: format!("hashmig_{label}_{}", self.suffix),
            display_name: format!("Hash migration {label} {}", self.suffix),
            description: None,
            selection_strategy: RouteSelectionStrategy::Default,
            agent_profile_id: None,
            metadata: json!({ "suite": "idempotency_hash_migration" }),
        }
    }

    async fn ledger_count(&self, key: &str) -> i64 {
        let versioned = self.hasher.hash(key.as_bytes());
        let legacy = self.hasher.legacy_hash(key.as_bytes());
        timeout(
            WAIT,
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_records
                 where idempotency_key_hash = any($1) and operation = $2",
            )
            .bind(vec![versioned, legacy])
            .bind(OPERATION)
            .fetch_one(&self.fixture.pool),
        )
        .await
        .expect("ledger count timed out")
        .expect("ledger count")
    }

    /// A consumer key carrying the conversation scopes the message route requires.
    /// `LifecycleFixture::enable_public_streaming` turns the policy on but issues a key
    /// without `moira:conversations:write`.
    async fn conversation_consumer_key(&self) -> String {
        self.fixture.enable_public_streaming().await;
        AdminService::new(&self.fixture.state)
            .expect("admin service")
            .create_consumer_key(
                &self.fixture.actor,
                &request_context(),
                ConsumerKeyCreateRequest {
                    application_id: self.fixture.application_id,
                    display_name: format!("hash-migration {}", self.suffix),
                    scopes: vec![
                        "moira:conversations:create".to_string(),
                        "moira:conversations:read".to_string(),
                        "moira:conversations:write".to_string(),
                    ],
                    expires_at: None,
                },
            )
            .await
            .expect("create conversation consumer key")
            .secret
            .expect("consumer key secret")
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

/// The load-bearing test of this plan's Definition of Done: "a DB row written with the
/// legacy unkeyed hash format before this change still replay-matches correctly after
/// this change ships (proven by an automated test, not manual inspection)".
///
/// Both dimensions are covered — the row is reachable only through the legacy **index
/// key**, and its stored body digest is only acceptable through the legacy **compared
/// value**.
#[tokio::test]
async fn legacy_unkeyed_row_still_replays_after_the_hmac_switch() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("legacy-replay");
    let request = fixture.application_request("legacy-replay");
    let slug = request.application_slug.clone().expect("slug");

    let first = fixture.post_application(Some(&key), &request).await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);
    let resource_id = first.id();

    let (legacy_key_hash, legacy_request_hash) = fixture
        .downgrade_to_legacy(&resource_id, &key, &request)
        .await;

    // The replay must find the pre-deploy row by its legacy index key and accept its
    // legacy body digest.
    let replay = fixture.post_application(Some(&key), &request).await;
    assert_eq!(
        replay.status,
        StatusCode::CREATED,
        "a pre-deploy row must still replay, got: {}",
        replay.body
    );
    assert_eq!(
        replay.id(),
        resource_id,
        "the replay must return the originally created application"
    );
    assert_eq!(
        replay.body, first.body,
        "the replayed body must be the stored one, verbatim"
    );
    assert_eq!(
        fixture.application_count(&slug).await,
        1,
        "a replay must not create a second application"
    );
    assert_eq!(
        fixture.ledger_count(&key).await,
        1,
        "a replay must not insert a second ledger row"
    );

    // Nothing rewrote the row: it is still in pre-deploy format, which is what proves
    // the lookup went through the legacy branch rather than through a fresh insert.
    let (stored_key_hash, stored_request_hash) = fixture.ledger_hashes(&resource_id).await;
    assert_eq!(stored_key_hash, legacy_key_hash);
    assert_eq!(stored_request_hash, legacy_request_hash);

    fixture.shutdown().await;
}

/// The same Definition-of-Done box as the test above, but on the path that actually
/// calls `IdempotencyHasher::verify` — and the one the plan calls the headline P1-1
/// exposure, because `/v1/responses` bodies are the ones that can carry credential
/// material.
///
/// The admin-command test above cannot stand in for this: `admin_command.rs` compares
/// through `AdminIdempotencyClaim::matches_stored_request_hash` and never reaches
/// `verify` at all. A review mutation that disabled `verify`'s legacy branch
/// (`None => false`) left every pre-existing test in this file green.
///
/// The provider is scripted with exactly **one** completion, so a replay that silently
/// re-executed would get a 502 from the exhausted mock rather than quietly producing a
/// second, plausible-looking response.
#[tokio::test]
async fn legacy_public_response_row_still_replays_after_the_hmac_switch() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "legacy replay payload".to_string(),
    }])
    .await;
    fixture
        .fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.fixture.enable_public_streaming().await;

    let key = fixture.key("public-legacy-replay");
    let request = public_response_request(&fixture.fixture.route_key);
    // The canonical envelope `claim_idempotency` hashes: `(application_id, request)`.
    // `downgrade_row_to_legacy` asserts this really is what production stored before it
    // rewrites anything.
    let request_bytes = serde_json::to_vec(&(Some(fixture.fixture.application_id), &request))
        .expect("canonical /v1/responses envelope");

    let first = fixture
        .post_public_response(&consumer_key, &key, &request)
        .await;
    assert_eq!(first.status, StatusCode::OK, "body: {}", first.body);
    let response_id = first.id();
    assert_eq!(
        provider.call_count().await,
        1,
        "the first request must genuinely execute"
    );

    let (legacy_key_hash, legacy_request_hash) = fixture
        .downgrade_row_to_legacy(RESPONSE_OPERATION, &response_id, &key, &request_bytes)
        .await;

    let replay = fixture
        .post_public_response(&consumer_key, &key, &request)
        .await;
    assert_eq!(
        replay.status,
        StatusCode::OK,
        "a pre-deploy /v1/responses row must still replay, got: {}",
        replay.body
    );
    assert_eq!(
        replay.id(),
        response_id,
        "the replay must return the originally created response"
    );
    assert_eq!(
        replay.body, first.body,
        "the replayed body must be the stored one, verbatim"
    );
    assert_eq!(
        provider.call_count().await,
        1,
        "a replay must not re-execute against the provider"
    );
    assert_eq!(
        fixture.ledger_count_for(RESPONSE_OPERATION, &key).await,
        1,
        "a replay must not insert a second ledger row"
    );

    // Still pre-deploy format: the replay went through the legacy branch of both the
    // lookup and the comparison, not through a fresh claim.
    let (stored_key_hash, stored_request_hash) = fixture
        .ledger_hashes_for(RESPONSE_OPERATION, &response_id)
        .await;
    assert_eq!(stored_key_hash, legacy_key_hash);
    assert_eq!(stored_request_hash, legacy_request_hash);

    fixture.shutdown().await;
    provider.shutdown().await;
}

/// The third ledger user — `src/application/runtime_admin.rs` — which has its own
/// `idempotency_replay`/`record_idempotency` pair and its own private canonical-bytes
/// helper, and which also reads through `IdempotencyHasher::verify`.
#[tokio::test]
async fn legacy_runtime_admin_row_still_replays_after_the_hmac_switch() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("route-legacy-replay");
    let request = fixture.route_request("legacy");
    let request_bytes = serde_json::to_vec(&request).expect("canonical runtime-admin envelope");
    let body = serde_json::to_value(&request).expect("serialise route request");

    let first = fixture
        .post("/api/v1/admin/routes", Some(&key), body.clone())
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);
    let route_id = first.id();

    let (legacy_key_hash, legacy_request_hash) = fixture
        .downgrade_row_to_legacy(ROUTE_OPERATION, &route_id, &key, &request_bytes)
        .await;

    let replay = fixture.post("/api/v1/admin/routes", Some(&key), body).await;
    assert_eq!(
        replay.status,
        StatusCode::CREATED,
        "a pre-deploy runtime-admin row must still replay, got: {}",
        replay.body
    );
    assert_eq!(replay.id(), route_id);
    assert_eq!(
        replay.body, first.body,
        "the replayed body must be the stored one, verbatim"
    );
    assert_eq!(
        fixture.route_count(&request.route_key).await,
        1,
        "a replay must not create a second route definition"
    );
    assert_eq!(fixture.ledger_count_for(ROUTE_OPERATION, &key).await, 1);

    let (stored_key_hash, stored_request_hash) =
        fixture.ledger_hashes_for(ROUTE_OPERATION, &route_id).await;
    assert_eq!(stored_key_hash, legacy_key_hash);
    assert_eq!(stored_request_hash, legacy_request_hash);

    fixture.shutdown().await;
}

/// The negative half on the `verify`-using runtime-admin path: the legacy row must be
/// *found* and its stored digest must actually be *compared*, so a different body under
/// the same key conflicts rather than replaying someone else's response.
#[tokio::test]
async fn legacy_runtime_admin_row_with_a_different_body_still_conflicts() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("route-legacy-conflict");
    let request = fixture.route_request("conflict");
    let request_bytes = serde_json::to_vec(&request).expect("canonical runtime-admin envelope");

    let first = fixture
        .post(
            "/api/v1/admin/routes",
            Some(&key),
            serde_json::to_value(&request).expect("serialise route request"),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);
    fixture
        .downgrade_row_to_legacy(ROUTE_OPERATION, &first.id(), &key, &request_bytes)
        .await;

    // Same label, so the route key (and therefore the resource identity) is unchanged and
    // only the hashed body differs — exactly the case the compared digest must catch.
    let mut different = fixture.route_request("conflict");
    different.display_name = format!("{} (changed)", different.display_name);
    let conflict = fixture
        .post(
            "/api/v1/admin/routes",
            Some(&key),
            serde_json::to_value(&different).expect("serialise route request"),
        )
        .await;

    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    assert_eq!(conflict.error()["code"], "idempotency_conflict");
    assert_eq!(
        conflict.error()["message_key"],
        "moira.error.idempotency_conflict"
    );

    fixture.shutdown().await;
}

/// The negative half of the dual-lookup contract. If the legacy row were found but its
/// stored digest were never actually compared, a *different* body under the same key
/// would replay the wrong response instead of conflicting.
#[tokio::test]
async fn legacy_row_with_a_different_body_still_returns_idempotency_conflict() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("legacy-conflict");
    let request = fixture.application_request("legacy-conflict");

    let first = fixture.post_application(Some(&key), &request).await;
    assert_eq!(first.status, StatusCode::CREATED, "body: {}", first.body);
    fixture
        .downgrade_to_legacy(&first.id(), &key, &request)
        .await;

    let mut different = fixture.application_request("legacy-conflict");
    different.display_name = format!("{} (changed)", different.display_name);

    let conflict = fixture.post_application(Some(&key), &different).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    assert_eq!(conflict.error()["code"], "idempotency_conflict");
    assert_eq!(
        conflict.error()["message_key"],
        "moira.error.idempotency_conflict"
    );

    fixture.shutdown().await;
}

/// Every *new* ledger value is written in the versioned HMAC format — the actual P1-1
/// correction — and still fits the `varchar(128)` columns, which is what makes the
/// "no migration needed" claim true.
#[tokio::test]
async fn new_records_persist_the_versioned_hmac_prefix() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("versioned");
    let request = fixture.application_request("versioned");

    let created = fixture.post_application(Some(&key), &request).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (key_hash, request_hash) = fixture.ledger_hashes(&created.id()).await;
    let prefix = format!("{}:", fixture.hasher.pepper_version());
    for (label, value) in [("key hash", &key_hash), ("request hash", &request_hash)] {
        assert!(
            value.starts_with(&prefix),
            "{label} {value} is not written in the versioned format"
        );
        assert!(value.len() <= 128, "{label} must fit varchar(128)");
    }
    assert_eq!(
        key_hash,
        fixture.hasher.hash(key.as_bytes()),
        "the stored key hash must be the keyed HMAC of the Idempotency-Key"
    );
    assert_eq!(
        request_hash,
        fixture
            .spec(&request)
            .request_hash(&fixture.hasher)
            .expect("versioned request hash"),
        "the stored body hash must be the keyed HMAC of the canonical command envelope"
    );
    assert_ne!(
        request_hash,
        fixture
            .spec(&request)
            .legacy_request_hash()
            .expect("legacy request hash"),
        "the new value must not coincide with the unkeyed digest"
    );

    fixture.shutdown().await;
}

/// The ordinary replay contract, unchanged by the hashing switch.
#[tokio::test]
async fn admin_command_replay_matches_under_the_new_hasher() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("replay");
    let request = fixture.application_request("replay");
    let slug = request.application_slug.clone().expect("slug");

    let first = fixture.post_application(Some(&key), &request).await;
    let second = fixture.post_application(Some(&key), &request).await;

    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    assert_eq!(first.body, second.body, "replay must be verbatim");
    assert_eq!(fixture.application_count(&slug).await, 1);
    assert_eq!(fixture.ledger_count(&key).await, 1);

    // A request without a key is not idempotent and must still be able to fail on the
    // slug's own uniqueness rather than silently replaying.
    let unkeyed = fixture.post_application(None, &request).await;
    assert_ne!(
        unkeyed.status,
        StatusCode::CREATED,
        "an unkeyed duplicate must not create a second application: {}",
        unkeyed.body
    );
    assert_eq!(fixture.application_count(&slug).await, 1);

    fixture.shutdown().await;
}

/// The conflict contract must not have been weakened by the hardening.
#[tokio::test]
async fn body_mismatch_under_the_new_hasher_still_returns_idempotency_conflict() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("conflict");
    let request = fixture.application_request("conflict");

    let first = fixture.post_application(Some(&key), &request).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let mut different = fixture.application_request("conflict-other");
    different.display_name = format!("{} (changed)", different.display_name);
    let conflict = fixture.post_application(Some(&key), &different).await;

    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    let error = conflict.error();
    assert_eq!(error["code"], "idempotency_conflict");
    assert_eq!(error["message_key"], "moira.error.idempotency_conflict");
    assert!(
        moira::i18n::is_known_key(
            error["message_key"]
                .as_str()
                .expect("message_key is a string")
        ),
        "the conflict key must be catalogued"
    );
    assert!(
        !error["message"]
            .as_str()
            .expect("message is a string")
            .is_empty()
    );
    assert!(
        !error["request_id"]
            .as_str()
            .expect("request_id is a string")
            .is_empty()
    );

    fixture.shutdown().await;
}

/// Concurrency under the new hasher. The interleaving gate is a `tokio::sync::Barrier`
/// — no `sleep()`, per `plans/CONVENTIONS.md` §3.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_key_claims_still_produce_one_ledger_row() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let key = fixture.key("concurrent");
    let request = fixture.application_request("concurrent");
    let slug = request.application_slug.clone().expect("slug");
    let body = serde_json::to_value(&request).expect("serialise application request");

    const RACERS: usize = 4;
    // `RACERS + 1`: every racer plus the test body, so no request is issued until all of
    // them are parked on the barrier.
    let barrier = Arc::new(Barrier::new(RACERS + 1));
    let mut tasks = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let client = fixture.client.clone();
        let url = fixture.url("/api/v1/admin/applications");
        let key = key.clone();
        let payload = body.to_string();
        let gate = barrier.clone();
        tasks.push(tokio::spawn(async move {
            gate.wait().await;
            let response = client
                .post(url)
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .header("x-request-id", format!("idem-race-{}", Uuid::now_v7()))
                .body(payload)
                .send()
                .await
                .expect("concurrent HTTP response");
            let status = response.status();
            let bytes = response.bytes().await.expect("concurrent body");
            (status, bytes)
        }));
    }
    barrier.wait().await;

    let mut created = 0_usize;
    for task in tasks {
        let (status, bytes) = timeout(WAIT, task)
            .await
            .expect("concurrent request timed out")
            .expect("concurrent task panicked");
        if status == StatusCode::CREATED {
            created += 1;
        } else {
            // The only tolerated loser is the in-progress conflict the ledger raises
            // while the winner holds the claim.
            let body: Value = serde_json::from_slice(&bytes).expect("JSON error envelope");
            assert_eq!(
                status,
                StatusCode::CONFLICT,
                "unexpected concurrent outcome: {body}"
            );
            assert_eq!(body["error"]["code"], "idempotency_in_progress");
        }
    }

    assert!(created >= 1, "at least one racer must win the claim");
    assert_eq!(
        fixture.application_count(&slug).await,
        1,
        "the ledger must admit exactly one mutation"
    );
    assert_eq!(
        fixture.ledger_count(&key).await,
        1,
        "exactly one ledger row may exist for the key"
    );

    fixture.shutdown().await;
}

/// The conversation-layer content hashes are write-only fingerprints (no read-side
/// comparison), but they cover the same secret-bearing exposure P1-1 is about, so they
/// must be written keyed and versioned too — and still fit `varchar(128)`.
#[tokio::test]
async fn conversation_content_hashes_are_written_in_the_versioned_format() {
    let Some(fixture) = HashFixture::new().await else {
        return;
    };
    let consumer_key = fixture.conversation_consumer_key().await;

    let conversation = timeout(
        WAIT,
        fixture
            .client
            .post(fixture.url("/api/v1/conversations"))
            .header("content-type", "application/json")
            .header("x-consumer-key", &consumer_key)
            .body(json!({ "title": format!("hash {}", fixture.suffix) }).to_string())
            .send(),
    )
    .await
    .expect("conversation create timed out")
    .expect("conversation create");
    assert_eq!(conversation.status(), StatusCode::CREATED);
    let conversation: Value = conversation.json().await.expect("conversation body");
    let conversation_id = conversation["id"].as_str().expect("conversation id");

    let content = format!("sensitive personal note {}", fixture.suffix);
    let message = timeout(
        WAIT,
        fixture
            .client
            .post(fixture.url(&format!("/api/v1/conversations/{conversation_id}/messages")))
            .header("content-type", "application/json")
            .header("x-consumer-key", &consumer_key)
            .body(json!({ "role": "user", "content": content }).to_string())
            .send(),
    )
    .await
    .expect("message create timed out")
    .expect("message create");
    assert_eq!(message.status(), StatusCode::CREATED);

    let hashes: Vec<String> = timeout(
        WAIT,
        sqlx::query_scalar::<_, String>(
            "select m.content_hash
             from conversation_messages m
             join conversations c on c.id = m.conversation_id
             where c.public_id = $1",
        )
        .bind(conversation_id)
        .fetch_all(&fixture.fixture.pool),
    )
    .await
    .expect("content hash query timed out")
    .expect("content hash query");

    assert!(
        !hashes.is_empty(),
        "the message route must persist at least one content hash"
    );
    let prefix = format!("{}:", fixture.hasher.pepper_version());
    for hash in &hashes {
        assert!(
            hash.starts_with(&prefix),
            "content hash {hash} is not in the versioned format"
        );
        assert!(hash.len() <= 128, "content hash must fit varchar(128)");
    }
    assert!(
        hashes.contains(&fixture.hasher.hash(content.as_bytes())),
        "the stored hash must be the keyed HMAC of the message content"
    );
    assert!(
        !hashes.contains(&fixture.hasher.legacy_hash(content.as_bytes())),
        "no unkeyed digest may remain on this path"
    );

    fixture.shutdown().await;
}
