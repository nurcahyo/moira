//! What a `moira_runtime_config` notification actually invalidates, driven end to end
//! through a real PostgreSQL `LISTEN`/`NOTIFY` round trip.
//!
//! # The finding this file exists for
//!
//! `listen_once` logged `notification.payload()` and then called
//! `circuits.reset_all()` regardless of what it said. Every trigger on the channel —
//! twenty tables, most of them nothing to do with provider health — therefore cleared
//! the breaker state of *every* provider in the process. Creating an application,
//! rotating a consumer API key or writing a conversation row was enough to re-close a
//! circuit that had been opened by real, still-ongoing provider failures, and the next
//! request went straight back at the failing provider. On a busy control plane that is
//! not a rare race: it is whatever the admin surface happens to be doing.
//!
//! The unit tests in `src/infra/db.rs` and `src/orchestration/controls.rs` cover the
//! two halves separately — payload to scope, and scope to entries. Neither can prove
//! the halves meet, because the thing between them is Postgres: the payload shape is
//! produced by `notify_moira_runtime_config_change` in `migrations/`, not by Rust, and
//! a mismatch there (a renamed key, a `tg_table_name` that is not what the mapping
//! expects) would leave both unit suites green while the running system silently fell
//! back to `reset_all()` on every notification. So these tests write real rows and let
//! the real trigger build the payload.
//!
//! # Why the assertions are ordering barriers rather than sleeps
//!
//! `NOTIFY` delivery is asynchronous, so "assert nothing was reset" needs a moment it
//! can be checked at. Sleeping picks that moment by guesswork and turns a real
//! regression into a flake in both directions. Instead each test opens a throwaway
//! *probe* circuit and drives it with a hand-built notification issued **after** the
//! event under test. Postgres delivers notifications to a listening connection in
//! commit order, so once the probe circuit is observed cleared, the earlier
//! notification has provably already been handled and the negative assertion is safe.
//!
//! Other test binaries run against the same database and emit their own notifications
//! on this channel. That is deliberate rather than tolerated: those notifications are
//! scoped to their own provider and model ids, so under the fix they cannot touch the
//! circuits these tests own — and under the bug they could, which is one more way for
//! a regression to show up here.
//!
//! Where a bounded poll is unavoidable it paces itself with a `tokio::time::interval`
//! rather than a `sleep`, matching `tests/retention_worker.rs::poll_until`: the first
//! tick completes immediately, so a working system satisfies the condition on the first
//! iteration and only a genuine failure pays the deadline.
//!
//! # The second path into the same registry
//!
//! `RuntimeAdminService::invalidate_runtime` clears the caches and the breakers directly,
//! in the process that served the admin request, alongside the notification the same write
//! emits. It called `reset_all()` too, so the fix above was only half of one — the
//! service-side half is covered by
//! [`every_runtime_admin_mutation_leaves_provider_circuits_intact`], which needs no
//! barrier at all because that reset happens inside the awaited call.

mod support;

use std::time::{Duration, Instant};

use chrono::Utc;
use moira::{
    application::RuntimeAdminService,
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AuthMethod, ExecutionFailureClass,
        ProviderConfig, ProviderKind, ProviderRuntimePolicyPutRequest, ProviderRuntimePolicyRecord,
        PublicAuthMethod, RouteDefinitionCreateRequest, RouteDefinitionPatchRequest,
        RouteSelectionStrategy, RoutingPolicyCreateRequest, RoutingPolicyPatchRequest,
        RuntimePolicyStatus,
    },
    orchestration::{
        CircuitBreakerRegistry, RuntimeCacheKey, RuntimeConfigCache, RuntimeModelHandle,
    },
};
use rig_core::{client::CompletionClient, providers::openai};
use serde_json::json;
use sqlx::PgPool;
use support::{LifecycleFixture, RuntimePolicy, request_context};
use uuid::Uuid;

/// How long a notification is given to travel from `COMMIT` to the listener task.
/// Generous on purpose: a barrier that has not arrived yet must never be read as a
/// reset that did not happen.
const BARRIER_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A breaker policy whose `version` is pinned, because `before_call` resets any entry
/// whose stored `policy_version` disagrees with the policy it is handed. Every read and
/// write below uses this same record, so a circuit that closes did so because something
/// cleared it and not because the policy appeared to change underneath it.
fn breaker_policy() -> ProviderRuntimePolicyRecord {
    ProviderRuntimePolicyRecord {
        id: Uuid::now_v7(),
        provider_id: Uuid::now_v7(),
        connect_timeout_ms: 1_000,
        request_timeout_ms: 1_000,
        stream_idle_timeout_ms: 1_000,
        max_concurrent_requests: 1,
        max_concurrent_streams: 1,
        retry_limit: 0,
        retry_base_delay_ms: 0,
        retry_max_delay_ms: 0,
        circuit_failure_threshold: 1,
        // Long enough that nothing below can half-open on a timer and be mistaken for a
        // notification-driven reset.
        circuit_open_duration_ms: 600_000,
        status: RuntimePolicyStatus::Active,
        updated_at: Utc::now(),
        version: 1,
    }
}

/// Drives one `(provider, model)` breaker to `Open` and asserts it got there, so a
/// later "still open" assertion cannot pass because the circuit was never opened.
async fn open_circuit(circuits: &CircuitBreakerRegistry, provider_id: Uuid, model_id: Uuid) {
    let policy = breaker_policy();
    circuits
        .on_failure(
            provider_id,
            model_id,
            &policy,
            ExecutionFailureClass::ProviderTimeout,
        )
        .await;
    assert!(
        is_open(circuits, provider_id, model_id).await,
        "circuit for ({provider_id}, {model_id}) should be open before the notification"
    );
}

/// Reads a breaker without disturbing the entries this test cares about.
///
/// `before_call` re-inserts a `Closed` entry on a miss, so calling it on a *cleared*
/// key is idempotent — it stays closed — while calling it on a surviving key returns
/// the `CircuitOpen` failure without mutating anything.
async fn is_open(circuits: &CircuitBreakerRegistry, provider_id: Uuid, model_id: Uuid) -> bool {
    let policy = breaker_policy();
    circuits
        .before_call(provider_id, model_id, &policy)
        .await
        .is_err()
}

async fn notify(pool: &PgPool, payload: &str) {
    sqlx::query("select pg_notify('moira_runtime_config', $1)")
        .bind(payload)
        .execute(pool)
        .await
        .expect("emit runtime config notification");
}

fn change_payload(resource_type: &str, resource_id: Uuid) -> String {
    json!({ "resource_type": resource_type, "resource_id": resource_id.to_string() }).to_string()
}

/// Blocks until the listener has demonstrably drained everything notified so far.
///
/// Opens a fresh probe circuit nobody else can name, then repeatedly emits a
/// `provider_models` notification for it until it clears. Re-emitting is safe as a
/// barrier: every repeat is still issued after the event under test, so observing the
/// probe clear still proves that event was handled first. It also doubles as the
/// "listener is attached" wait — until the `PgListener` has issued its `LISTEN`,
/// notifications are dropped on the floor and no single-shot signal would ever arrive.
async fn drain_listener(pool: &PgPool, circuits: &CircuitBreakerRegistry) {
    let probe_provider = Uuid::now_v7();
    let probe_model = Uuid::now_v7();
    open_circuit(circuits, probe_provider, probe_model).await;

    let payload = change_payload("provider_models", probe_model);
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    let deadline = Instant::now() + BARRIER_TIMEOUT;
    loop {
        notify(pool, &payload).await;
        let attempt_until = Instant::now() + Duration::from_millis(250);
        while Instant::now() < attempt_until {
            if !is_open(circuits, probe_provider, probe_model).await {
                return;
            }
            ticker.tick().await;
        }
        assert!(
            Instant::now() < deadline,
            "runtime config listener never handled a notification within {BARRIER_TIMEOUT:?}"
        );
    }
}

async fn wait_until<F>(mut condition: F, message: &str)
where
    F: AsyncFnMut() -> bool,
{
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    let deadline = Instant::now() + BARRIER_TIMEOUT;
    while Instant::now() < deadline {
        if condition().await {
            return;
        }
        ticker.tick().await;
    }
    panic!("{message}");
}

/// A cache entry that exists only to be observed disappearing. Its id is unique per
/// call, so a concurrent suite's notification clearing the cache early cannot be
/// mistaken for this one being invalidated by the write under test.
fn sentinel_provider() -> ProviderConfig {
    ProviderConfig {
        id: Uuid::now_v7(),
        tenant_id: None,
        application_id: None,
        name: "runtime-config-invalidation-sentinel".to_string(),
        kind: ProviderKind::OpenAiCompatible,
        base_url: "http://127.0.0.1:9/v1".to_string(),
        default_model: "sentinel".to_string(),
        enabled: true,
        timeout_ms: 1_000,
        metadata: json!({ "test_fixture": true }),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// A provider-handle cache key nothing else in the tree can name.
///
/// Every version field is pinned to a value no fixture writes, so the entry can only be
/// removed by something that clears the whole cache — which is the only thing this file
/// is asking about.
fn sentinel_cache_key() -> RuntimeCacheKey {
    RuntimeCacheKey {
        provider_id: Uuid::now_v7(),
        provider_version: 1,
        model_id: Uuid::now_v7(),
        model_version: 1,
        credential_id: Uuid::now_v7(),
        credential_version: 1,
        runtime_policy_version: 1,
    }
}

/// A real [`RuntimeModelHandle`], built the way `RigRuntimeFactory` builds one.
///
/// Client construction in `rig-core` is network-free — no request is made until a
/// completion is requested — so this costs nothing and needs no mock. It has to be a
/// genuine handle rather than a stand-in because the cache stores the enum itself.
fn sentinel_handle() -> RuntimeModelHandle {
    let client = openai::Client::builder()
        .api_key("f51-sentinel-not-a-real-key")
        .build()
        .expect("build an openai client")
        .completions_api();
    RuntimeModelHandle::OpenAi(client.completion_model("f51-sentinel"))
}

fn sentinel_auth_method() -> PublicAuthMethod {
    PublicAuthMethod {
        id: Uuid::now_v7(),
        method: AuthMethod::GoogleOauth,
        display_name: "F51 sentinel".to_string(),
        issuer: Some("https://accounts.google.com".to_string()),
        discovery_url: None,
        authorization_url: None,
        jwks_url: None,
        client_id: Some("f51-sentinel".to_string()),
        requested_scopes: vec!["openid".to_string()],
        allowed_email_domains: Vec::new(),
    }
}

/// How many invalidations this fixture's listener has applied, from its own metrics.
///
/// `record_runtime_invalidation` is the last thing `apply_invalidation` does and it runs
/// on **every** notification, including the ones that now clear no cache. That is exactly
/// what a barrier for this file needs and what `drain_listener` cannot be: that helper
/// drives its probe with a `provider_models` payload, which is configuration, so using it
/// to bracket a cache-survival assertion would clear the very caches under observation.
///
/// Each `LifecycleFixture` owns its own `MetricsRegistry` and its own database, so this
/// counter cannot be moved by another test.
fn invalidations_applied(fixture: &LifecycleFixture) -> u64 {
    let rendered = fixture
        .state
        .metrics
        .render_prometheus("moira-test", false, false);
    rendered
        .lines()
        .find(|line| {
            line.starts_with("moira_runtime_invalidations_total")
                && line.contains(r#"channel="postgres""#)
        })
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("the runtime invalidation counter is seeded at zero from process start")
}

/// Emits one notification and blocks until the listener has applied it, clearing nothing
/// on the way.
///
/// The counter above is the acknowledgement: it is incremented after the caches and
/// breakers have been dealt with, so observing it move proves the payload was fully
/// handled and the assertions that follow read a settled state.
async fn notify_and_await(fixture: &LifecycleFixture, payload: &str) {
    let before = invalidations_applied(fixture);
    notify(&fixture.pool, payload).await;
    wait_until(
        async || invalidations_applied(fixture) > before,
        "the runtime config listener never applied the notification under test",
    )
    .await;
}

/// Emits `sentinel` on the channel and returns every payload that preceded it.
///
/// Postgres delivers notifications to one connection in commit order, so a sentinel that
/// arrives first proves everything committed before it announced nothing. That is an
/// acknowledgement gate rather than a delay, and unlike a timeout it cannot pass by
/// giving up early. Borrowed from `tests/policy_reads_do_not_write.rs`.
async fn drain_until_sentinel(
    listener: &mut sqlx::postgres::PgListener,
    pool: &PgPool,
    sentinel: &str,
) -> Vec<String> {
    sqlx::query("select pg_notify('moira_runtime_config', $1)")
        .bind(sentinel)
        .execute(pool)
        .await
        .expect("emit the sentinel notification");
    let mut seen = Vec::new();
    loop {
        let notification = listener.recv().await.expect("listener stayed connected");
        let payload = notification.payload().to_string();
        if payload == sentinel {
            return seen;
        }
        seen.push(payload);
    }
}

struct Listener {
    task: tokio::task::JoinHandle<()>,
}

impl Listener {
    async fn start(fixture: &LifecycleFixture) -> Self {
        let task = moira::infra::db::spawn_runtime_config_listener(
            fixture.pool.clone(),
            moira::infra::db::RuntimeInvalidationTargets::from_state(&fixture.state),
        );
        // The listener loops forever, so nothing else will ever join it; aborting on
        // drop is the only teardown available and it must be the last thing to touch
        // the pool, which the fixture outlives.
        let listener = Self { task };
        drain_listener(&fixture.pool, &fixture.state.circuits).await;
        listener
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The mapping this plan added, exercised through the production trigger rather than a
/// hand-built payload: an `UPDATE` on `provider_models` must clear that model's breaker
/// and nothing else.
///
/// Fails on the pre-fix code at the final assertion — `reset_all()` clears the whole
/// map, so the unrelated circuit is closed too.
#[tokio::test]
async fn provider_model_notify_resets_only_that_models_circuit() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let circuits = fixture.state.circuits.clone();
    let _listener = Listener::start(&fixture).await;

    let provider = fixture
        .add_provider(
            "http://127.0.0.1:9/v1".to_string(),
            100,
            RuntimePolicy::default(),
        )
        .await;
    // A provider that exists only in the breaker map. Nothing in this test writes a row
    // naming it, so any reset it suffers came from over-broad invalidation.
    let unrelated_provider = Uuid::now_v7();
    let unrelated_model = Uuid::now_v7();

    // `add_provider` writes providers, provider_models, provider_credentials, the
    // runtime policy and a routing policy — five notifications of its own. Drain them
    // before opening the circuits, or the setup would clear what it just established.
    drain_listener(&fixture.pool, &circuits).await;
    open_circuit(&circuits, provider.provider_id, provider.model_id).await;
    open_circuit(&circuits, unrelated_provider, unrelated_model).await;

    sqlx::query("update provider_models set display_name = $2 where id = $1")
        .bind(provider.model_id)
        .bind("renamed by runtime_config_invalidation")
        .execute(&fixture.pool)
        .await
        .expect("update provider model");

    wait_until(
        async || !is_open(&circuits, provider.provider_id, provider.model_id).await,
        "the changed model's circuit was never reset by its own NOTIFY",
    )
    .await;

    assert!(
        is_open(&circuits, unrelated_provider, unrelated_model).await,
        "a provider_models change reset a circuit belonging to a different provider; \
         reset_all() is still being called on the notification path"
    );
}

/// The actual finding: a write to a table that has nothing to do with provider health
/// must not discard breaker state at all.
///
/// `applications` is one of eighteen such tables wired to the same channel. Fails on
/// the pre-fix code — `reset_all()` cannot tell the tables apart.
#[tokio::test]
async fn unrelated_table_notify_leaves_all_circuits_intact() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let circuits = fixture.state.circuits.clone();
    let _listener = Listener::start(&fixture).await;

    let first_provider = Uuid::now_v7();
    let first_model = Uuid::now_v7();
    let second_provider = Uuid::now_v7();
    let second_model = Uuid::now_v7();
    open_circuit(&circuits, first_provider, first_model).await;
    open_circuit(&circuits, second_provider, second_model).await;

    sqlx::query("update applications set display_name = $2 where id = $1")
        .bind(fixture.application_id)
        .bind("renamed by runtime_config_invalidation")
        .execute(&fixture.pool)
        .await
        .expect("update application");

    // Ordering barrier: once this returns, the `applications` notification above has
    // already been handled, so the assertions below are reading a settled state.
    drain_listener(&fixture.pool, &circuits).await;

    assert!(
        is_open(&circuits, first_provider, first_model).await,
        "an applications row change reset a provider circuit"
    );
    assert!(
        is_open(&circuits, second_provider, second_model).await,
        "an applications row change reset a provider circuit"
    );
}

/// Narrowing must not have narrowed the caches on a **configuration** write. An
/// `applications` row is not provider health — so it resets no breaker — but it very much
/// is runtime configuration, so every cache must still be dropped.
///
/// # This test used to be called `…_on_every_notify`, and that name is now wrong
///
/// It was written when `apply_invalidation` cleared the three caches unconditionally, and
/// its old name asserted that as the property. F51 is precisely that the unconditional
/// clearing was wrong for the two tables on this channel that carry per-request data, so
/// a test named for "every notify" would have pinned the defect in place — the shape
/// HANDOFF §3.4 records twice, where a test's own literal or name is what keeps the wrong
/// behaviour in the code. The property it actually guards, and always did, is the one in
/// the new name: a *config* write still invalidates.
#[tokio::test]
async fn runtime_cache_still_invalidates_on_every_config_notify() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let circuits = fixture.state.circuits.clone();
    let cache: RuntimeConfigCache = fixture.state.runtime_cache.clone();
    let _listener = Listener::start(&fixture).await;

    let sentinel = sentinel_provider();
    cache.put_provider(sentinel.clone()).await;
    assert!(
        cache.get_provider(sentinel.id).await.is_some(),
        "sentinel provider was not cached, so its disappearance would prove nothing"
    );

    // An unrelated table — precisely the case that no longer resets a circuit. The
    // cache must still be dropped.
    sqlx::query("update applications set display_name = $2 where id = $1")
        .bind(fixture.application_id)
        .bind("cache invalidation probe")
        .execute(&fixture.pool)
        .await
        .expect("update application");

    drain_listener(&fixture.pool, &circuits).await;

    assert!(
        cache.get_provider(sentinel.id).await.is_none(),
        "the runtime config cache survived a NOTIFY; scoping the breaker reset must not \
         have narrowed the caches"
    );
}

/// Asserts that one runtime-admin mutation left both breakers where it found them.
///
/// Two circuits, because the two ways of getting this wrong look different: `owned`
/// belongs to the provider whose rows the mutation actually touches, and `unrelated`
/// belongs to a provider no row in this database names at all. `reset_all()` clears
/// both; a mapping that guessed `Provider(id)` from the method name would clear only
/// the first.
async fn assert_circuits_survived(
    circuits: &CircuitBreakerRegistry,
    owned: (Uuid, Uuid),
    unrelated: (Uuid, Uuid),
    mutation: &str,
) {
    assert!(
        is_open(circuits, owned.0, owned.1).await,
        "{mutation} cleared the breaker of the provider whose rows it touched; none of \
         the four tables this service writes can change whether that provider answers"
    );
    assert!(
        is_open(circuits, unrelated.0, unrelated.1).await,
        "{mutation} cleared a breaker belonging to a provider it never named; \
         invalidate_runtime is still calling reset_all()"
    );
}

/// The service-side half of the finding at the top of this file, driven through all
/// thirteen call sites of `RuntimeAdminService::invalidate_runtime`.
///
/// Fixing the notification path alone fixed nothing an operator would notice: the admin
/// request that caused the notification also runs `invalidate_runtime` in-process, and
/// that called `reset_all()`, so every runtime-admin mutation still discarded the health
/// of every provider in the process — the exact defect, on the exact registry, one layer
/// up. Disabling one agent profile was enough to re-close a circuit opened by a
/// still-failing provider on a completely unrelated route.
///
/// Every caller is driven rather than one per table, because the scope is now an argument
/// each call site passes for itself: a wrong argument at, say, `delete_agent_profile`
/// would be invisible to a test that only exercised `create_agent_profile`.
///
/// No barrier machinery is used and none is needed. `LifecycleFixture` does not spawn
/// `spawn_runtime_config_listener` and this test does not start a `Listener`, so the only
/// thing that can reach this registry is the service calls below — and their reset runs
/// inside the awaited call, so each assertion reads a settled state the instant the call
/// returns.
///
/// Under the pre-fix `reset_all()` it fails on the first assertion, at
/// `create_route_definition`.
#[tokio::test]
async fn every_runtime_admin_mutation_leaves_provider_circuits_intact() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let circuits = fixture.state.circuits.clone();
    let runtime = RuntimeAdminService::new(&fixture.state).expect("runtime admin service");
    let suffix = Uuid::now_v7().simple().to_string();

    // Created first: `add_provider` is itself a run of these mutations, so opening the
    // circuits before it would only prove that it clears them.
    let provider = fixture
        .add_provider(
            "http://127.0.0.1:9/v1".to_string(),
            100,
            RuntimePolicy::default(),
        )
        .await;
    let policy_version = runtime
        .get_provider_runtime_policy(&fixture.actor, provider.provider_id)
        .await
        .expect("read the provider runtime policy written by add_provider")
        .version;

    let owned = (provider.provider_id, provider.model_id);
    let unrelated = (Uuid::now_v7(), Uuid::now_v7());
    open_circuit(&circuits, owned.0, owned.1).await;
    open_circuit(&circuits, unrelated.0, unrelated.1).await;

    // --- route_definitions -------------------------------------------------------
    let route = runtime
        .create_route_definition(
            &fixture.actor,
            &request_context(),
            RouteDefinitionCreateRequest {
                route_key: format!("scoped_{suffix}"),
                display_name: format!("Scoped route {suffix}"),
                description: None,
                selection_strategy: RouteSelectionStrategy::Default,
                agent_profile_id: None,
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create route definition");
    assert_circuits_survived(&circuits, owned, unrelated, "create_route_definition").await;

    // Each mutation is rebound from the record it returns: `If-Match` is now enforced inside
    // the write's own transaction, so every call has to carry the version its predecessor
    // produced rather than the one the create returned.
    let route = runtime
        .patch_route_definition(
            &fixture.actor,
            &request_context(),
            route.id,
            route.version,
            RouteDefinitionPatchRequest {
                display_name: Some(format!("Scoped route {suffix} renamed")),
                ..RouteDefinitionPatchRequest::default()
            },
        )
        .await
        .expect("patch route definition");
    assert_circuits_survived(&circuits, owned, unrelated, "patch_route_definition").await;

    let route = runtime
        .set_route_definition_enabled(
            &fixture.actor,
            &request_context(),
            route.id,
            route.version,
            false,
        )
        .await
        .expect("disable route definition");
    assert_circuits_survived(&circuits, owned, unrelated, "set_route_definition_enabled").await;

    // --- routing_policies --------------------------------------------------------
    // Bound to the fixture's own route rather than the one above, which is about to be
    // deleted. This is the family whose rows carry a `(provider_id, provider_model_id)`
    // pair, so it is the one most likely to be mis-scoped to `Provider(id)`.
    let routing_policy = runtime
        .create_routing_policy(
            &fixture.actor,
            &request_context(),
            RoutingPolicyCreateRequest {
                application_id: Some(fixture.application_id),
                external_tenant_id: None,
                route_id: fixture.route_id,
                provider_id: provider.provider_id,
                provider_model_id: provider.model_id,
                priority: 50,
                weight: 1,
                cost_weight: 0.0,
                latency_weight: 0.0,
                quality_weight: 0.0,
                privacy_class: None,
                required_capabilities: Vec::new(),
                maximum_cost_per_request: None,
                maximum_input_tokens: None,
                maximum_output_tokens: None,
                timeout_ms: Some(2_000),
                retry_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create routing policy");
    assert_circuits_survived(&circuits, owned, unrelated, "create_routing_policy").await;

    let routing_policy = runtime
        .patch_routing_policy(
            &fixture.actor,
            &request_context(),
            routing_policy.id,
            routing_policy.version,
            RoutingPolicyPatchRequest {
                priority: Some(60),
                ..RoutingPolicyPatchRequest::default()
            },
        )
        .await
        .expect("patch routing policy");
    assert_circuits_survived(&circuits, owned, unrelated, "patch_routing_policy").await;

    let routing_policy = runtime
        .set_routing_policy_enabled(
            &fixture.actor,
            &request_context(),
            routing_policy.id,
            routing_policy.version,
            false,
        )
        .await
        .expect("disable routing policy");
    assert_circuits_survived(&circuits, owned, unrelated, "set_routing_policy_enabled").await;

    runtime
        .delete_routing_policy(
            &fixture.actor,
            &request_context(),
            routing_policy.id,
            routing_policy.version,
        )
        .await
        .expect("delete routing policy");
    assert_circuits_survived(&circuits, owned, unrelated, "delete_routing_policy").await;

    // --- agent_profiles ----------------------------------------------------------
    let profile = runtime
        .create_agent_profile(
            &fixture.actor,
            &request_context(),
            AgentProfileCreateRequest {
                profile_key: format!("scoped_{suffix}"),
                display_name: format!("Scoped profile {suffix}"),
                preamble: None,
                temperature: Some(0.2),
                max_tokens: Some(64),
                tool_policy: json!({}),
                context_policy: json!({}),
                memory_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create agent profile");
    assert_circuits_survived(&circuits, owned, unrelated, "create_agent_profile").await;

    let profile = runtime
        .patch_agent_profile(
            &fixture.actor,
            &request_context(),
            profile.id,
            profile.version,
            AgentProfilePatchRequest {
                display_name: Some(format!("Scoped profile {suffix} renamed")),
                ..AgentProfilePatchRequest::default()
            },
        )
        .await
        .expect("patch agent profile");
    assert_circuits_survived(&circuits, owned, unrelated, "patch_agent_profile").await;

    let profile = runtime
        .set_agent_profile_enabled(
            &fixture.actor,
            &request_context(),
            profile.id,
            profile.version,
            false,
        )
        .await
        .expect("disable agent profile");
    assert_circuits_survived(&circuits, owned, unrelated, "set_agent_profile_enabled").await;

    runtime
        .delete_agent_profile(
            &fixture.actor,
            &request_context(),
            profile.id,
            profile.version,
        )
        .await
        .expect("delete agent profile");
    assert_circuits_survived(&circuits, owned, unrelated, "delete_agent_profile").await;

    // --- provider_runtime_policies -----------------------------------------------
    // The only caller holding a `provider_id`, and the one place `Provider(id)` would
    // have been expressible. It must still leave `owned` alone: `before_call` resets an
    // entry whose stored `policy_version` no longer matches, so a policy edit self-heals
    // where it is consulted, and clearing here would discard a provider's whole breaker
    // set on an edit to an unrelated field.
    //
    // The breaker below is unaffected by that self-heal because `open_circuit` and
    // `is_open` both use the pinned `breaker_policy()`; the registry never reads this row.
    runtime
        .put_provider_runtime_policy(
            &fixture.actor,
            &request_context(),
            provider.provider_id,
            Some(policy_version),
            ProviderRuntimePolicyPutRequest {
                connect_timeout_ms: Some(2_000),
                request_timeout_ms: Some(2_000),
                stream_idle_timeout_ms: Some(2_000),
                max_concurrent_requests: Some(8),
                max_concurrent_streams: Some(4),
                retry_limit: Some(0),
                retry_base_delay_ms: Some(0),
                retry_max_delay_ms: Some(0),
                circuit_failure_threshold: Some(5),
                circuit_open_duration_ms: Some(30_000),
                status: Some(RuntimePolicyStatus::Active),
            },
        )
        .await
        .expect("put provider runtime policy");
    assert_circuits_survived(&circuits, owned, unrelated, "put_provider_runtime_policy").await;

    // Deleted last so the route family's fourth caller runs after everything that needed
    // a live route, and the count of exercised call sites reaches thirteen.
    runtime
        .delete_route_definition(&fixture.actor, &request_context(), route.id, route.version)
        .await
        .expect("delete route definition");
    assert_circuits_survived(&circuits, owned, unrelated, "delete_route_definition").await;
}

/// F51, the classifier half: a per-request data payload must clear **no** cache.
///
/// # Why this observes all three caches and not just the runtime config cache
///
/// The finding is three unconditional `invalidate_all()` calls. A guard that watched only
/// `runtime_cache` would stay green against the cheapest edit that preserves the defect —
/// honour the plan for that one cache and keep clearing the other two — and the most
/// expensive of the three is `runtime_handles`, which holds *built provider clients* with
/// their connection pools, not query results. So a real handle and a real auth-methods
/// list are seeded alongside the provider config, and all three are asserted.
///
/// # Silence here is checked against a control that must speak
///
/// "The caches survived" proves nothing if nothing could have cleared them — the listener
/// might not be attached, the seeds might have expired. So the same test then issues an
/// `applications` payload on the same listener and requires all three to be cleared by
/// *that*. Survival and liveness are one fact, per §3.4.
///
/// # It sends a hand-built payload rather than writing a conversation row
///
/// Deliberately. `migrations/0022` drops the trigger from those tables, so a real
/// conversation write emits nothing at all and this test would pass without the
/// classifier ever being consulted — the two barriers would be indistinguishable. This
/// one bypasses the trigger and therefore tests only the classifier;
/// [`writing_a_conversation_announces_no_runtime_config_change`] writes a real row and
/// therefore tests only the trigger.
#[tokio::test]
async fn a_conversation_notification_invalidates_no_cache() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let cache = fixture.state.runtime_cache.clone();
    let handles = fixture.state.runtime_handles.clone();
    let auth_settings = fixture.state.auth_settings_cache.clone();
    let _listener = Listener::start(&fixture).await;

    // Seed all three caches. Each seed is checked, so a later disappearance cannot be
    // read off something that was never there.
    let sentinel = sentinel_provider();
    cache.put_provider(sentinel.clone()).await;
    let handle_key = sentinel_cache_key();
    handles.insert(handle_key.clone(), sentinel_handle()).await;
    auth_settings
        .put_enabled_methods(vec![sentinel_auth_method()])
        .await;

    assert!(
        cache.get_provider(sentinel.id).await.is_some(),
        "runtime config cache was not seeded"
    );
    assert!(
        handles.get(&handle_key).await.is_some(),
        "provider runtime handle cache was not seeded"
    );
    assert!(
        auth_settings.enabled_methods().await.is_some(),
        "auth settings cache was not seeded"
    );

    // The finding: a conversation is per-request data, not configuration.
    notify_and_await(&fixture, &change_payload("conversations", Uuid::now_v7())).await;

    assert!(
        cache.get_provider(sentinel.id).await.is_some(),
        "a conversations notification dropped the runtime config cache; every replica now \
         re-reads every provider it had cached"
    );
    assert!(
        handles.get(&handle_key).await.is_some(),
        "a conversations notification dropped every built provider client handle, \
         connection pools included — this is F51's most expensive consequence"
    );
    assert!(
        auth_settings.enabled_methods().await.is_some(),
        "a conversations notification dropped the auth settings cache"
    );

    // The control, on the same listener and the same seeds: a configuration write must
    // still clear all three. Without it every assertion above would hold just as well
    // against a listener that was never attached.
    notify_and_await(
        &fixture,
        &change_payload("applications", fixture.application_id),
    )
    .await;

    assert!(
        cache.get_provider(sentinel.id).await.is_none(),
        "an applications notification left the runtime config cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
    assert!(
        handles.get(&handle_key).await.is_none(),
        "an applications notification left the provider handle cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
    assert!(
        auth_settings.enabled_methods().await.is_none(),
        "an applications notification left the auth settings cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
}

/// F51, the trigger half: writing a conversation row announces nothing at all.
///
/// The classifier is one barrier; `migrations/0022` dropping the trigger is the other,
/// and this is the one that observes it. A real row is written and the channel is checked
/// for silence with the sentinel technique from `tests/policy_reads_do_not_write.rs`:
/// Postgres delivers notifications on one connection in commit order, so if the test's
/// own sentinel is the first thing received, the write announced nothing. No sleep, no
/// timeout, and exact.
///
/// The liveness control is in the same test for the same reason as everywhere else — an
/// assertion that nothing arrived is worthless unless something can arrive — and it is a
/// write to `applications`, which still carries its trigger.
#[tokio::test]
async fn writing_a_conversation_announces_no_runtime_config_change() {
    const SENTINEL: &str = "f51-sentinel";

    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // No `Listener` here: this test reads the raw channel itself rather than watching a
    // cache, so a listener task consuming in parallel would only add noise.
    let mut listener = sqlx::postgres::PgListener::connect_with(&fixture.pool)
        .await
        .expect("attach a listener");
    listener
        .listen("moira_runtime_config")
        .await
        .expect("listen");

    let conversation_id = Uuid::now_v7();
    sqlx::query(
        "insert into conversations (id, public_id, application_id, title, metadata) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(conversation_id)
    .bind(format!("conv_{conversation_id}"))
    .bind(fixture.application_id)
    .bind("F51 conversation")
    .bind(json!({ "test_fixture": true }))
    .execute(&fixture.pool)
    .await
    .expect("insert a conversation");

    // An UPDATE and a DELETE too: the trigger fired on all three operations, and dropping
    // it for one is not dropping it.
    sqlx::query("update conversations set title = $2 where id = $1")
        .bind(conversation_id)
        .bind("F51 conversation renamed")
        .execute(&fixture.pool)
        .await
        .expect("update the conversation");
    sqlx::query("delete from conversations where id = $1")
        .bind(conversation_id)
        .execute(&fixture.pool)
        .await
        .expect("delete the conversation");

    let announced = drain_until_sentinel(&mut listener, &fixture.pool, SENTINEL).await;
    let conversation_notifications: Vec<&String> = announced
        .iter()
        .filter(|payload| payload.contains("conversations"))
        .collect();
    assert!(
        conversation_notifications.is_empty(),
        "a conversation write announced a runtime-config change, so every replica dropped \
         its runtime config and provider-handle caches: {conversation_notifications:?}"
    );

    // The control: the channel is live and this listener is on it.
    sqlx::query("update applications set display_name = $2 where id = $1")
        .bind(fixture.application_id)
        .bind("f51 liveness probe")
        .execute(&fixture.pool)
        .await
        .expect("update the application");
    let announced = drain_until_sentinel(&mut listener, &fixture.pool, SENTINEL).await;
    assert!(
        announced
            .iter()
            .any(|payload| payload.contains("applications")),
        "the notification channel is not live, so the silence asserted above proves \
         nothing; saw {announced:?}"
    );
}

/// F53, the classifier half: a RAG **content** notification clears nothing either.
///
/// # Why this is a separate finding from F51 and not a separate property
///
/// `rag_collections` and `rag_documents` came from the same `0007` block as
/// `conversations` and `memory_records` and had the same defect, at a different rate:
/// these two are written from admin routes, so a bulk import of N documents is N
/// cross-replica cache wipes rather than one per conversation turn. The rate is why it
/// was recorded separately. The property is identical, so this test is deliberately the
/// twin of [`a_conversation_notification_invalidates_no_cache`] — including seeding a
/// real [`RuntimeModelHandle`], because `runtime_handles` holds built provider clients
/// with their connection pools and is the expensive thing the fix protects. A guard that
/// watched only `runtime_cache` stays green against an edit that honours the plan for
/// that one cache and keeps wiping the other two.
///
/// # Both tables, not one
///
/// F53 required establishing whether a collection's own configuration is read through a
/// cache before either could lose its trigger, precisely because the two might have had
/// different answers. They did not — a collection carries no runtime configuration at all
/// — so both are narrowed, and the cheapest edit that preserves half the defect is to
/// revert half the classifier. Each resource type is therefore asserted on its own.
///
/// # Hand-built payloads, for the reason its twin gives
///
/// `migrations/0024` drops both triggers, so a real document write emits nothing and this
/// test would pass without the classifier ever being consulted. This one bypasses the
/// trigger and tests only the classifier;
/// [`writing_rag_content_announces_no_runtime_config_change`] writes real rows and
/// therefore tests only the trigger.
#[tokio::test]
async fn a_rag_content_notification_invalidates_no_cache() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let cache = fixture.state.runtime_cache.clone();
    let handles = fixture.state.runtime_handles.clone();
    let auth_settings = fixture.state.auth_settings_cache.clone();
    let _listener = Listener::start(&fixture).await;

    let sentinel = sentinel_provider();
    cache.put_provider(sentinel.clone()).await;
    let handle_key = sentinel_cache_key();
    handles.insert(handle_key.clone(), sentinel_handle()).await;
    auth_settings
        .put_enabled_methods(vec![sentinel_auth_method()])
        .await;

    assert!(
        cache.get_provider(sentinel.id).await.is_some(),
        "runtime config cache was not seeded"
    );
    assert!(
        handles.get(&handle_key).await.is_some(),
        "provider runtime handle cache was not seeded"
    );
    assert!(
        auth_settings.enabled_methods().await.is_some(),
        "auth settings cache was not seeded"
    );

    for resource_type in ["rag_documents", "rag_collections"] {
        notify_and_await(&fixture, &change_payload(resource_type, Uuid::now_v7())).await;

        assert!(
            cache.get_provider(sentinel.id).await.is_some(),
            "a {resource_type} notification dropped the runtime config cache; a bulk \
             import does this once per document, on every replica"
        );
        assert!(
            handles.get(&handle_key).await.is_some(),
            "a {resource_type} notification dropped every built provider client handle, \
             connection pools included — this is F53's most expensive consequence and the \
             reason the guard does not stop at the config cache"
        );
        assert!(
            auth_settings.enabled_methods().await.is_some(),
            "a {resource_type} notification dropped the auth settings cache"
        );
    }

    // The control, on the same listener and the same seeds. Without it every assertion
    // above would hold just as well against a listener that was never attached.
    notify_and_await(
        &fixture,
        &change_payload("applications", fixture.application_id),
    )
    .await;

    assert!(
        cache.get_provider(sentinel.id).await.is_none(),
        "an applications notification left the runtime config cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
    assert!(
        handles.get(&handle_key).await.is_none(),
        "an applications notification left the provider handle cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
    assert!(
        auth_settings.enabled_methods().await.is_none(),
        "an applications notification left the auth settings cache in place, so the \
         survival asserted above proves nothing about scoping"
    );
}

/// F53, the trigger half: writing a RAG collection or document announces nothing at all.
///
/// The twin of [`writing_a_conversation_announces_no_runtime_config_change`], and it
/// observes the other barrier — `migrations/0024` — with the same sentinel technique:
/// Postgres delivers notifications on one connection in commit order, so if this test's
/// own sentinel is the first thing received, everything committed before it announced
/// nothing. No sleep, no timeout, exact.
///
/// INSERT, UPDATE **and** DELETE on both tables, because the trigger fired on all three
/// operations and dropping it for one is not dropping it. The document is written and
/// removed before the collection, since `rag_documents.collection_id` is
/// `on delete cascade` and a cascaded delete would fire the document trigger from the
/// collection's own `delete` — which is exactly the shape that would otherwise leave one
/// of the two tables' triggers untested.
#[tokio::test]
async fn writing_rag_content_announces_no_runtime_config_change() {
    const SENTINEL: &str = "f53-sentinel";

    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let mut listener = sqlx::postgres::PgListener::connect_with(&fixture.pool)
        .await
        .expect("attach a listener");
    listener
        .listen("moira_runtime_config")
        .await
        .expect("listen");

    let collection_id = Uuid::now_v7();
    sqlx::query(
        "insert into rag_collections (id, public_id, application_id, collection_key, \
         display_name, metadata) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(collection_id)
    .bind(format!("collection_{collection_id}"))
    .bind(fixture.application_id)
    .bind(format!("f53_{}", Uuid::now_v7().simple()))
    .bind("F53 collection")
    .bind(json!({ "test_fixture": true }))
    .execute(&fixture.pool)
    .await
    .expect("insert a rag collection");

    let document_id = Uuid::now_v7();
    sqlx::query(
        "insert into rag_documents (id, public_id, collection_id, title, source_type, \
         mime_type, metadata) values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(document_id)
    .bind(format!("doc_{document_id}"))
    .bind(collection_id)
    .bind("F53 document")
    .bind("inline")
    .bind("text/plain")
    .bind(json!({ "test_fixture": true }))
    .execute(&fixture.pool)
    .await
    .expect("insert a rag document");

    sqlx::query("update rag_documents set title = $2 where id = $1")
        .bind(document_id)
        .bind("F53 document renamed")
        .execute(&fixture.pool)
        .await
        .expect("update the rag document");
    sqlx::query("delete from rag_documents where id = $1")
        .bind(document_id)
        .execute(&fixture.pool)
        .await
        .expect("delete the rag document");

    sqlx::query("update rag_collections set display_name = $2 where id = $1")
        .bind(collection_id)
        .bind("F53 collection renamed")
        .execute(&fixture.pool)
        .await
        .expect("update the rag collection");
    sqlx::query("delete from rag_collections where id = $1")
        .bind(collection_id)
        .execute(&fixture.pool)
        .await
        .expect("delete the rag collection");

    let announced = drain_until_sentinel(&mut listener, &fixture.pool, SENTINEL).await;
    let rag_notifications: Vec<&String> = announced
        .iter()
        .filter(|payload| payload.contains("rag_collections") || payload.contains("rag_documents"))
        .collect();
    assert!(
        rag_notifications.is_empty(),
        "a RAG content write announced a runtime-config change, so every replica dropped \
         its runtime config, auth settings and provider-handle caches — once per document \
         in a bulk import: {rag_notifications:?}"
    );

    // The control: the channel is live and this listener is on it.
    sqlx::query("update applications set display_name = $2 where id = $1")
        .bind(fixture.application_id)
        .bind("f53 liveness probe")
        .execute(&fixture.pool)
        .await
        .expect("update the application");
    let announced = drain_until_sentinel(&mut listener, &fixture.pool, SENTINEL).await;
    assert!(
        announced
            .iter()
            .any(|payload| payload.contains("applications")),
        "the notification channel is not live, so the silence asserted above proves \
         nothing; saw {announced:?}"
    );
}

/// The fail-safe. A payload the listener cannot understand must behave exactly as the
/// code did before scoping existed, because the alternative — narrowing a payload that
/// was misread — is a breaker that stays stale with no signal that anything is wrong.
#[tokio::test]
async fn malformed_notify_payload_falls_back_to_full_reset() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let circuits = fixture.state.circuits.clone();
    let _listener = Listener::start(&fixture).await;

    let first_provider = Uuid::now_v7();
    let first_model = Uuid::now_v7();
    let second_provider = Uuid::now_v7();
    let second_model = Uuid::now_v7();
    open_circuit(&circuits, first_provider, first_model).await;
    open_circuit(&circuits, second_provider, second_model).await;

    notify(&fixture.pool, "this is not the payload you are looking for").await;

    wait_until(
        async || {
            !is_open(&circuits, first_provider, first_model).await
                && !is_open(&circuits, second_provider, second_model).await
        },
        "an unparseable payload did not fall back to a full circuit reset",
    )
    .await;
}
