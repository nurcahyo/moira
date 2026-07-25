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

mod support;

use std::time::{Duration, Instant};

use chrono::Utc;
use moira::{
    domain::{
        ExecutionFailureClass, ProviderConfig, ProviderKind, ProviderRuntimePolicyRecord,
        RuntimePolicyStatus,
    },
    orchestration::{CircuitBreakerRegistry, RuntimeConfigCache},
};
use serde_json::json;
use sqlx::PgPool;
use support::{LifecycleFixture, RuntimePolicy};
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
    let deadline = Instant::now() + BARRIER_TIMEOUT;
    loop {
        notify(pool, &payload).await;
        let attempt_until = Instant::now() + Duration::from_millis(250);
        while Instant::now() < attempt_until {
            if !is_open(circuits, probe_provider, probe_model).await {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
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
    let deadline = Instant::now() + BARRIER_TIMEOUT;
    while Instant::now() < deadline {
        if condition().await {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
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

struct Listener {
    task: tokio::task::JoinHandle<()>,
}

impl Listener {
    async fn start(fixture: &LifecycleFixture) -> Self {
        let task = moira::infra::db::spawn_runtime_config_listener(
            fixture.pool.clone(),
            fixture.state.runtime_cache.clone(),
            fixture.state.runtime_handles.clone(),
            fixture.state.circuits.clone(),
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

/// Narrowing the breaker reset must not have narrowed the caches with it. They are
/// keyed by row version and rebuild from a query, so invalidating them on every
/// notification stays correct and cheap; breaker state is the only thing that cannot be
/// rebuilt, which is why it is the only thing that was scoped.
#[tokio::test]
async fn runtime_cache_still_invalidates_on_every_notify() {
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
