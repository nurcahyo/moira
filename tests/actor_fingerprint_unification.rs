//! Plan 06 / Module 16 (finding P2-15) — one `actor_fingerprint`, proven at the ledger.
//!
//! `idempotency_records` has a single unique index,
//! `(idempotency_key_hash, actor_fingerprint, operation)`
//! (`migrations/0003_security_foundation.sql:360-361`). Until this module landed, **three**
//! formulas wrote that one index:
//!
//! | Writer | Fields | Blind to |
//! |---|---|---|
//! | `application::admin::shared` (admin + conversation) | 10 | — |
//! | `application::runtime_admin` | 3 | issuer, tenant, application, external user, delegation |
//! | `application::public` | 4 | issuer, tenant, external user, delegation |
//!
//! The consequence was not cosmetic. Two callers differing only by trusted-JWT issuer, or
//! only by tenant, hashed to the *same* point in that index on the runtime-admin and public
//! routes, so one caller's `Idempotency-Key` addressed another caller's ledger row.
//!
//! ## Why these tests are here and not in `src/`
//!
//! The unit tests in `src/application/admin/shared.rs` assert the formula distinguishes the
//! fields. That is necessary and not sufficient: it says nothing about whether the routes
//! *call* it, whether the value reaches the index, or whether the write path and the read
//! path agree. Every one of those is a database-visible property, so it is asserted here
//! against a real PostgreSQL, over the real service entry points, by reading the ledger
//! column itself.
//!
//! ## What each test would have done before the fix
//!
//! * `runtime_admin_replay_is_isolated_across_trusted_jwt_issuers` /
//!   `..._across_tenants` — the second actor collided with the first's ledger row and was
//!   answered `409 idempotency_conflict`: denied service by a key it never saw, on the
//!   strength of an identity field the fingerprint could not see.
//! * `public_replay_is_isolated_across_trusted_jwt_issuers` / `..._across_tenants` — worse,
//!   and the reason the public pair is written with an identical request body: the second
//!   actor was handed the first actor's *stored response*, verbatim, with no execution of
//!   its own. The mock provider is scripted with exactly two completions and its call count
//!   asserted, so a silent replay shows up as `1` rather than as a plausible-looking body.
//! * `every_idempotent_command_path_writes_one_actor_fingerprint` — three distinct
//!   fingerprints for one actor and one key. It now reads the ledger's `actor_fingerprint`
//!   column across the admin, runtime-admin and public paths and requires exactly one
//!   distinct value.
//!
//! ## The migration half
//!
//! Unification changes the fingerprint of every in-flight row on those two paths. A naive
//! switch would stop matching them — a client retrying across the deploy would get a second
//! execution instead of a replay, turning a correctness fix into a correctness bug. The
//! read path therefore accepts the legacy value and the write path never emits it, the same
//! shape `tests/idempotency_hash_migration.rs` established for the hashing switch.
//! `a_legacy_runtime_admin_fingerprint_row_still_replays_after_unification` and
//! `a_legacy_public_fingerprint_row_still_replays_after_unification` rewrite a real ledger
//! row into the pre-deploy format and require it to still replay.
//!
//! Fail-closed rule: the skip decision lives in `tests/support/mod.rs` and is reused rather
//! than re-implemented — `MOIRA_TEST_DATABASE_URL` missing while `CI=true` panics,
//! otherwise the suite returns early.
//!
//! Cross-test isolation: every fixture-owned identifier carries a per-fixture
//! `Uuid::now_v7()` suffix, so two test binaries against one database cannot collide on a
//! ledger row or a unique index.

mod support;

use std::time::Duration;

use moira::{
    application::{AdminService, PublicExecutionService, RuntimeAdminService},
    domain::{
        AgentProfileCreateRequest, ApplicationCreateRequest, PublicResponse,
        RouteDefinitionCreateRequest, RouteSelectionStrategy,
    },
    security::{Actor, ActorType, IdempotencyHasher, secret_fingerprint},
};
use serde_json::json;
use tokio::time::timeout;
use uuid::Uuid;

use support::{
    LifecycleFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    public_response_request, request_context_with_idempotency_key,
};

const WAIT: Duration = Duration::from_secs(20);
const ROUTE_OPERATION: &str = "route.create";
const AGENT_PROFILE_OPERATION: &str = "agent_profile.create";
const RESPONSE_OPERATION: &str = "response.create";
const APPLICATION_OPERATION: &str = "application.create";

struct FingerprintFixture {
    fixture: LifecycleFixture,
    hasher: IdempotencyHasher,
    suffix: String,
}

impl FingerprintFixture {
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
        Some(Self {
            fixture,
            hasher,
            suffix: Uuid::now_v7().simple().to_string(),
        })
    }

    fn key(&self, label: &str) -> String {
        format!("fingerprint-{label}-{}", self.suffix)
    }

    fn key_hash(&self, key: &str) -> String {
        self.hasher.hash(key.as_bytes())
    }

    /// A caller that can drive all three idempotent surfaces.
    ///
    /// `TrustedJwt` rather than `ConsumerKey` because the trusted-JWT path is where the
    /// issuer field exists at all, and `AuthorizationService::has_scope` grants
    /// `moira:admin` its full meaning for every actor type except `ConsumerKey`.
    /// `internal_application_id` is required by `public_access` for this actor type.
    fn actor(&self, issuer: Uuid) -> Actor {
        Actor {
            actor_type: ActorType::TrustedJwt,
            subject: Some(format!("shared-subject-{}", self.suffix)),
            trusted_jwt_issuer_id: Some(issuer),
            internal_application_id: Some(self.fixture.application_id),
            scopes: vec!["moira:admin".to_string()],
            ..Actor::default()
        }
    }

    fn route_request(&self, label: &str) -> RouteDefinitionCreateRequest {
        RouteDefinitionCreateRequest {
            route_key: format!("fp_{label}_{}", self.suffix),
            display_name: format!("Fingerprint {label} {}", self.suffix),
            description: None,
            selection_strategy: RouteSelectionStrategy::Default,
            agent_profile_id: None,
            metadata: json!({ "suite": "actor_fingerprint_unification" }),
        }
    }

    fn agent_profile_request(&self, label: &str) -> AgentProfileCreateRequest {
        AgentProfileCreateRequest {
            profile_key: format!("fp_{label}_{}", self.suffix),
            display_name: format!("Fingerprint {label} {}", self.suffix),
            preamble: None,
            temperature: None,
            max_tokens: None,
            tool_policy: json!({}),
            context_policy: json!({}),
            memory_policy: json!({}),
            metadata: json!({ "suite": "actor_fingerprint_unification" }),
        }
    }

    /// The ledger rows one `Idempotency-Key` produced for one operation, as
    /// `(actor_fingerprint, resource_id)` ordered so assertions are deterministic.
    async fn ledger_rows(&self, operation: &str, key: &str) -> Vec<(String, Option<String>)> {
        timeout(
            WAIT,
            sqlx::query_as::<_, (String, Option<String>)>(
                "select actor_fingerprint, resource_id
                 from idempotency_records
                 where idempotency_key_hash = $1 and operation = $2
                 order by actor_fingerprint",
            )
            .bind(self.key_hash(key))
            .bind(operation)
            .fetch_all(&self.fixture.pool),
        )
        .await
        .expect("ledger lookup timed out")
        .expect("ledger lookup")
    }

    /// Every distinct `actor_fingerprint` this key wrote, across **all** operations. The
    /// unification claim in one query.
    async fn distinct_fingerprints(&self, key: &str) -> Vec<String> {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select distinct actor_fingerprint
                 from idempotency_records
                 where idempotency_key_hash = $1
                 order by actor_fingerprint",
            )
            .bind(self.key_hash(key))
            .fetch_all(&self.fixture.pool),
        )
        .await
        .expect("fingerprint lookup timed out")
        .expect("fingerprint lookup")
    }

    /// Rewrites the ledger row a request produced into its **pre-unification** shape.
    ///
    /// Only `actor_fingerprint` moves; the key hash, the request hash and the stored
    /// response stay exactly as production wrote them, so a replay afterwards can only
    /// succeed by way of the legacy read path. Asserts first that the row really carries
    /// the unified value, so a mis-derived legacy formula cannot turn the replay assertion
    /// into a tautology.
    async fn downgrade_fingerprint(&self, operation: &str, key: &str, unified: &str, legacy: &str) {
        assert_ne!(
            unified, legacy,
            "the legacy fingerprint must actually differ from the unified one, or this test \
             proves nothing"
        );
        let rows = self.ledger_rows(operation, key).await;
        assert_eq!(
            rows.len(),
            1,
            "expected exactly one ledger row to downgrade for {operation}"
        );
        assert_eq!(
            rows[0].0, unified,
            "production did not write the unified fingerprint for {operation}, so downgrading \
             it would prove nothing"
        );

        let affected = timeout(
            WAIT,
            sqlx::query(
                "update idempotency_records
                 set actor_fingerprint = $1
                 where idempotency_key_hash = $2 and operation = $3 and actor_fingerprint = $4",
            )
            .bind(legacy)
            .bind(self.key_hash(key))
            .bind(operation)
            .bind(unified)
            .execute(&self.fixture.pool),
        )
        .await
        .expect("ledger downgrade timed out")
        .expect("ledger downgrade")
        .rows_affected();
        assert_eq!(affected, 1, "exactly one ledger row must be downgraded");
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

    async fn route_count(&self, route_key: &str) -> i64 {
        self.count(
            "select count(*) from route_definitions where route_key = $1",
            route_key,
        )
        .await
    }
}

/// The pre-plan-06 runtime-admin formula, restated here rather than imported.
///
/// It is `fn legacy_actor_fingerprint` in `src/application/runtime_admin.rs` and private,
/// which is correct — nothing outside that module may compute it. Restating it is what
/// makes the migration test load-bearing: if the production copy is edited, the two stop
/// agreeing and `downgrade_fingerprint`'s equality assertion fails rather than the suite
/// silently proving nothing.
fn legacy_runtime_admin_fingerprint(actor: &Actor) -> String {
    secret_fingerprint(
        format!(
            "{:?}:{}:{}",
            actor.actor_type,
            actor.subject.as_deref().unwrap_or(""),
            actor
                .api_key_id
                .map(|id| id.to_string())
                .unwrap_or_default()
        )
        .as_bytes(),
    )
}

/// The pre-plan-06 public formula — the same, plus the resolved application id. See
/// [`legacy_runtime_admin_fingerprint`] for why it is restated rather than imported.
fn legacy_public_fingerprint(actor: &Actor, application_id: Option<Uuid>) -> String {
    secret_fingerprint(
        format!(
            "{:?}:{}:{}:{}",
            actor.actor_type,
            actor.subject.as_deref().unwrap_or(""),
            actor
                .api_key_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            application_id.map(|id| id.to_string()).unwrap_or_default()
        )
        .as_bytes(),
    )
}

/// Two runtime-admin callers differing **only** in trusted-JWT issuer must not share a
/// replay ledger slot.
///
/// Different `route_key`s are deliberate: `route_definitions_route_key_active_unique`
/// forbids two live rows sharing a key, so identical requests would make the isolated case
/// fail on a database constraint rather than on the property under test. With distinct
/// bodies the collision shows up as the second actor being answered
/// `409 idempotency_conflict` — denied service on the strength of a key issued by someone
/// it cannot see.
#[tokio::test]
async fn runtime_admin_replay_is_isolated_across_trusted_jwt_issuers() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let service = RuntimeAdminService::new(&fixture.fixture.state).expect("runtime admin service");
    let key = fixture.key("runtime-issuer");

    let first_actor = fixture.actor(Uuid::now_v7());
    let second_actor = fixture.actor(Uuid::now_v7());
    assert_ne!(
        first_actor.trusted_jwt_issuer_id, second_actor.trusted_jwt_issuer_id,
        "the two actors must differ in issuer"
    );
    assert_eq!(
        first_actor.subject, second_actor.subject,
        "and in nothing else"
    );

    let first_request = fixture.route_request("issuer_a");
    let second_request = fixture.route_request("issuer_b");

    let first = service
        .create_route_definition(
            &first_actor,
            &request_context_with_idempotency_key(Some(&key)),
            first_request.clone(),
        )
        .await
        .expect("first issuer's route");
    let second = service
        .create_route_definition(
            &second_actor,
            &request_context_with_idempotency_key(Some(&key)),
            second_request.clone(),
        )
        .await
        .expect(
            "the second issuer must not be blocked by the first issuer's Idempotency-Key — a \
             409 here is the P2-15 collision",
        );

    assert_ne!(first.id, second.id, "each issuer gets its own resource");
    assert_eq!(fixture.route_count(&first_request.route_key).await, 1);
    assert_eq!(fixture.route_count(&second_request.route_key).await, 1);

    let rows = fixture.ledger_rows(ROUTE_OPERATION, &key).await;
    assert_eq!(rows.len(), 2, "each issuer gets its own ledger row");
    assert_ne!(
        rows[0].0, rows[1].0,
        "the two rows must be distinguished by the fingerprint column itself, not by luck"
    );
}

/// The same property for the tenant dimension, across both tenant channels.
///
/// `tenant_id` (the claim as presented) and `external_tenant_id` (the resolved one) are
/// populated independently, so a formula covering one and not the other still leaks. Both
/// are varied here, in one test, against one baseline actor.
#[tokio::test]
async fn runtime_admin_replay_is_isolated_across_tenants() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let service = RuntimeAdminService::new(&fixture.fixture.state).expect("runtime admin service");
    let key = fixture.key("runtime-tenant");
    let issuer = Uuid::now_v7();

    let base_actor = fixture.actor(issuer);
    let tenant_claim_actor = Actor {
        tenant_id: Some(format!("tenant-a-{}", fixture.suffix)),
        ..fixture.actor(issuer)
    };
    let external_tenant_actor = Actor {
        external_tenant_id: Some(format!("tenant-b-{}", fixture.suffix)),
        ..fixture.actor(issuer)
    };

    let requests = [
        fixture.agent_profile_request("tenant_none"),
        fixture.agent_profile_request("tenant_claim"),
        fixture.agent_profile_request("tenant_external"),
    ];
    let actors = [&base_actor, &tenant_claim_actor, &external_tenant_actor];

    let mut ids = Vec::new();
    for (actor, request) in actors.iter().zip(requests.iter()) {
        let record = service
            .create_agent_profile(
                actor,
                &request_context_with_idempotency_key(Some(&key)),
                request.clone(),
            )
            .await
            .expect(
                "each tenant must be able to use its own Idempotency-Key — a 409 here is the \
                 P2-15 collision",
            );
        ids.push(record.id);
    }

    assert_eq!(ids[0], ids[0]);
    assert_ne!(ids[0], ids[1], "the tenant claim must partition the ledger");
    assert_ne!(
        ids[0], ids[2],
        "the resolved tenant must partition the ledger"
    );
    assert_ne!(ids[1], ids[2]);

    let rows = fixture.ledger_rows(AGENT_PROFILE_OPERATION, &key).await;
    assert_eq!(rows.len(), 3, "each tenant gets its own ledger row");
    let mut fingerprints: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
    fingerprints.sort_unstable();
    fingerprints.dedup();
    assert_eq!(
        fingerprints.len(),
        3,
        "three actors, three fingerprints — a shorter list means two tenants share an index \
         slot"
    );
}

/// The public route, and the one place the collision's real consequence is directly
/// observable: an **identical** request body, so a shared ledger slot returns the other
/// actor's stored response rather than a `409`.
///
/// The provider is scripted with exactly two completions and its call count asserted. A
/// silent replay therefore shows up as `1`, not as a second plausible-looking body.
#[tokio::test]
async fn public_replay_is_isolated_across_trusted_jwt_issuers() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([
        ProviderScript::Completion {
            text: "first issuer payload".to_string(),
        },
        ProviderScript::Completion {
            text: "second issuer payload".to_string(),
        },
    ])
    .await;
    fixture
        .fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    fixture.fixture.enable_public_streaming().await;

    let service = PublicExecutionService::new(&fixture.fixture.state).expect("public service");
    let key = fixture.key("public-issuer");
    let request = public_response_request(&fixture.fixture.route_key);

    let first_actor = fixture.actor(Uuid::now_v7());
    let second_actor = fixture.actor(Uuid::now_v7());

    let first: PublicResponse = service
        .create_response(
            &first_actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("first issuer's response");
    let second: PublicResponse = service
        .create_response(
            &second_actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("second issuer's response");

    assert_ne!(
        first.id, second.id,
        "the second issuer was handed the first issuer's stored response — this is the P2-15 \
         cross-actor replay"
    );
    assert_eq!(
        provider.call_count().await,
        2,
        "both issuers must genuinely execute; 1 means the second call replayed the first"
    );

    let rows = fixture.ledger_rows(RESPONSE_OPERATION, &key).await;
    assert_eq!(rows.len(), 2, "each issuer gets its own ledger row");
    assert_ne!(rows[0].0, rows[1].0);

    provider.shutdown().await;
}

/// The public route's tenant dimension. Same shape, same identical-body construction, and
/// the same call-count control.
#[tokio::test]
async fn public_replay_is_isolated_across_tenants() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([
        ProviderScript::Completion {
            text: "tenant a payload".to_string(),
        },
        ProviderScript::Completion {
            text: "tenant b payload".to_string(),
        },
    ])
    .await;
    fixture
        .fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    fixture.fixture.enable_public_streaming().await;

    let service = PublicExecutionService::new(&fixture.fixture.state).expect("public service");
    let key = fixture.key("public-tenant");
    let request = public_response_request(&fixture.fixture.route_key);
    let issuer = Uuid::now_v7();

    let first_actor = Actor {
        external_tenant_id: Some(format!("tenant-a-{}", fixture.suffix)),
        ..fixture.actor(issuer)
    };
    let second_actor = Actor {
        external_tenant_id: Some(format!("tenant-b-{}", fixture.suffix)),
        ..fixture.actor(issuer)
    };

    let first: PublicResponse = service
        .create_response(
            &first_actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("first tenant's response");
    let second: PublicResponse = service
        .create_response(
            &second_actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("second tenant's response");

    assert_ne!(
        first.id, second.id,
        "the second tenant was handed the first tenant's stored response"
    );
    assert_eq!(
        provider.call_count().await,
        2,
        "both tenants must genuinely execute"
    );
    assert_eq!(fixture.ledger_rows(RESPONSE_OPERATION, &key).await.len(), 2);

    provider.shutdown().await;
}

/// Unification stated as a single database fact.
///
/// One actor, one `Idempotency-Key`, driven through all three writers of
/// `idempotency_records` — the admin-command envelope, the runtime-admin two-phase scheme,
/// and the public claim path. Before Module 16 this produced three different values in the
/// `actor_fingerprint` column. It must now produce exactly one.
///
/// This is the assertion the unit tests cannot make: it reads the stored column rather than
/// calling a function, so it fails if any path stops calling the shared formula, whatever
/// the formula itself does.
#[tokio::test]
async fn every_idempotent_command_path_writes_one_actor_fingerprint() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "unified fingerprint payload".to_string(),
    }])
    .await;
    fixture
        .fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    fixture.fixture.enable_public_streaming().await;

    let key = fixture.key("unified");
    let actor = fixture.actor(Uuid::now_v7());

    AdminService::new(&fixture.fixture.state)
        .expect("admin service")
        .create_application(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            ApplicationCreateRequest {
                external_application_id: Some(format!("fp-unified-{}", fixture.suffix)),
                application_slug: Some(format!("fp-unified-{}", fixture.suffix)),
                display_name: format!("Fingerprint unified {}", fixture.suffix),
                metadata: json!({ "suite": "actor_fingerprint_unification" }),
            },
        )
        .await
        .expect("admin-command path");

    RuntimeAdminService::new(&fixture.fixture.state)
        .expect("runtime admin service")
        .create_route_definition(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            fixture.route_request("unified"),
        )
        .await
        .expect("runtime-admin path");

    PublicExecutionService::new(&fixture.fixture.state)
        .expect("public service")
        .create_response(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            public_response_request(&fixture.fixture.route_key),
        )
        .await
        .expect("public path");

    let fingerprints = fixture.distinct_fingerprints(&key).await;
    assert_eq!(
        fingerprints.len(),
        1,
        "one actor and one key must address one point of the unique index across all three \
         ledger writers; got {fingerprints:?}"
    );
    // And all three rows really were written, so the single value is not an artefact of two
    // paths having written nothing.
    assert_eq!(
        fixture.ledger_rows(APPLICATION_OPERATION, &key).await.len(),
        1
    );
    assert_eq!(fixture.ledger_rows(ROUTE_OPERATION, &key).await.len(), 1);
    assert_eq!(fixture.ledger_rows(RESPONSE_OPERATION, &key).await.len(), 1);

    provider.shutdown().await;
}

/// The migration half, runtime-admin side.
///
/// A row written by the previous release carries the 3-field fingerprint. If the new read
/// path only looked for the unified value it would miss, and a client retrying across the
/// deploy would get a **second execution** where it asked for a replay. The row is rewritten
/// into the pre-deploy shape and the same request replayed through the same service.
#[tokio::test]
async fn a_legacy_runtime_admin_fingerprint_row_still_replays_after_unification() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let service = RuntimeAdminService::new(&fixture.fixture.state).expect("runtime admin service");
    let key = fixture.key("runtime-legacy");
    let actor = fixture.actor(Uuid::now_v7());
    let request = fixture.route_request("legacy");

    let first = service
        .create_route_definition(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("pre-downgrade route");

    let rows = fixture.ledger_rows(ROUTE_OPERATION, &key).await;
    assert_eq!(rows.len(), 1);
    fixture
        .downgrade_fingerprint(
            ROUTE_OPERATION,
            &key,
            &rows[0].0,
            &legacy_runtime_admin_fingerprint(&actor),
        )
        .await;

    let replay = service
        .create_route_definition(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect(
            "a pre-deploy ledger row must still replay — an error here means an in-flight key \
             stopped matching across the deploy",
        );

    assert_eq!(
        replay.id, first.id,
        "the replay must return the originally created route"
    );
    assert_eq!(
        fixture.route_count(&request.route_key).await,
        1,
        "a replay must not create a second route definition"
    );
    let after = fixture.ledger_rows(ROUTE_OPERATION, &key).await;
    assert_eq!(after.len(), 1, "a replay must not insert a second row");
    assert_eq!(
        after[0].0,
        legacy_runtime_admin_fingerprint(&actor),
        "the replay must go through the legacy read path, not silently re-claim under the \
         unified fingerprint"
    );
}

/// The migration half, public side — the path whose bodies carry caller content and whose
/// duplicate execution costs a real provider call.
///
/// The provider is scripted with exactly one completion, so a replay that quietly
/// re-executed would fail against the exhausted mock rather than return a second
/// plausible-looking response.
#[tokio::test]
async fn a_legacy_public_fingerprint_row_still_replays_after_unification() {
    let Some(fixture) = FingerprintFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "legacy fingerprint payload".to_string(),
    }])
    .await;
    fixture
        .fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    fixture.fixture.enable_public_streaming().await;

    let service = PublicExecutionService::new(&fixture.fixture.state).expect("public service");
    let key = fixture.key("public-legacy");
    let actor = fixture.actor(Uuid::now_v7());
    let request = public_response_request(&fixture.fixture.route_key);

    let first: PublicResponse = service
        .create_response(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            request.clone(),
        )
        .await
        .expect("pre-downgrade response");
    assert_eq!(
        provider.call_count().await,
        1,
        "the first request must genuinely execute"
    );

    let rows = fixture.ledger_rows(RESPONSE_OPERATION, &key).await;
    assert_eq!(rows.len(), 1);
    let legacy = legacy_public_fingerprint(&actor, actor.internal_application_id);
    fixture
        .downgrade_fingerprint(RESPONSE_OPERATION, &key, &rows[0].0, &legacy)
        .await;

    let replay: PublicResponse = service
        .create_response(
            &actor,
            &request_context_with_idempotency_key(Some(&key)),
            request,
        )
        .await
        .expect("a pre-deploy /v1/responses ledger row must still replay");

    assert_eq!(
        replay.id, first.id,
        "the replay must return the originally created response"
    );
    assert_eq!(
        provider.call_count().await,
        1,
        "a replay must not re-execute against the provider"
    );
    let after = fixture.ledger_rows(RESPONSE_OPERATION, &key).await;
    assert_eq!(
        after.len(),
        1,
        "a replay must not insert a second ledger row alongside the legacy one"
    );
    assert_eq!(
        after[0].0, legacy,
        "the replay must go through the legacy read path"
    );

    provider.shutdown().await;
}
