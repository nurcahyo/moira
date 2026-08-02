use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use sqlx::{
    PgPool,
    postgres::{PgListener, PgPoolOptions},
};
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::DatabaseSettings,
    error::AppError,
    infra::{
        metrics::{InvalidationChannel, MetricsRegistry, RedisOperation},
        redis::RedisClient,
    },
    orchestration::{
        AuthProviderSettingsCache, CircuitBreakerRegistry, CircuitResetScope, ProviderRuntimeCache,
        RuntimeConfigCache,
    },
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(settings: &DatabaseSettings) -> Result<Option<PgPool>, AppError> {
    let Some(url) = settings.url.as_deref().filter(|url| !url.is_empty()) else {
        if settings.require {
            return Err(AppError::Config("database url is required".to_string()));
        }
        return Ok(None);
    };

    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .min_connections(settings.min_connections)
        .acquire_timeout(Duration::from_secs(settings.connect_timeout_seconds))
        .connect(url)
        .await
        .map(Some)
        .map_err(AppError::from)
}

pub async fn migrate(pool: &PgPool) -> Result<(), AppError> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|err| AppError::Internal(format!("run migrations: {err}")))
}

/// The caches one invalidation clears, bundled so the two listeners cannot be
/// handed different sets.
///
/// Plan 10 wave 2 adds a second listener over Redis pub/sub. The sequence it has
/// to reproduce is **four calls, and the last one is scoped** — a subscriber that
/// dropped the third would leave plan 07's auth-settings cache stale on every
/// replica that learned of a change over Redis, and one that used `reset_all`
/// instead of `reset_for_resource` would discard every provider's earned breaker
/// health on every config write. Bundling them into one type and one
/// [`apply_invalidation`] means neither listener can get the sequence wrong
/// independently of the other.
#[derive(Clone)]
pub struct RuntimeInvalidationTargets {
    pub cache: RuntimeConfigCache,
    pub runtime_handles: ProviderRuntimeCache,
    pub auth_settings: AuthProviderSettingsCache,
    pub circuits: CircuitBreakerRegistry,
    pub metrics: MetricsRegistry,
}

impl RuntimeInvalidationTargets {
    /// The five handles, taken from the one place that owns all of them.
    ///
    /// Both listeners and every test that starts one go through here, so a cache
    /// added to `AppState` later cannot be wired into one listener and forgotten
    /// in the other.
    pub fn from_state(state: &crate::app::AppState) -> Self {
        Self {
            cache: state.runtime_cache.clone(),
            runtime_handles: state.runtime_handles.clone(),
            auth_settings: state.auth_settings_cache.clone(),
            circuits: state.circuits.clone(),
            metrics: state.metrics.clone(),
        }
    }
}

/// Applies one invalidation, whichever channel delivered it.
///
/// # Every target is scoped, not just the breakers (F51)
///
/// The three caches were once cleared unconditionally, on the argument that they
/// are keyed by version and rebuild on the next read "so re-reading them costs a
/// query". That argument is sound for a *configuration* write and false for
/// everything else: the channel is also attached to tables whose rows are
/// per-request data, and `runtime_handles` does not hold query results — it holds
/// built provider clients, connection pools included. One conversation row
/// therefore dropped every replica's entire runtime-config cache and every cached
/// provider client handle, on a write that cannot change any of them.
///
/// So [`invalidation_plan`] now decides all four targets from one parse of the
/// payload, and the caches are skipped for the resource types that are declared
/// not to be configuration. The narrowing is deliberately one-way: anything this
/// function does not recognise still clears everything and resets every breaker,
/// exactly as it did before scoping existed.
///
/// # Why the three caches share one flag
///
/// The only narrowing that can be justified from a payload is "the row that
/// changed is not configuration at all". *Which* config table invalidates *which*
/// cache is a finer question nobody has established, and guessing it is how the
/// identity configuration goes stale — CONVENTIONS §7.2 wants an auth-settings
/// change to invalidate even if a future trigger, view or rename makes it arrive
/// under a payload this function does not recognise. Over-invalidating a list of
/// three rows on a genuine config write is free; under-invalidating it is not.
async fn apply_invalidation(
    targets: &RuntimeInvalidationTargets,
    payload: &str,
    source: InvalidationChannel,
) {
    let plan = invalidation_plan(payload);
    let scope = plan.circuits;
    if plan.caches {
        targets.cache.invalidate_all().await;
        targets.runtime_handles.invalidate_all().await;
        targets.auth_settings.invalidate_all().await;
    }
    targets.circuits.reset_for_resource(scope).await;
    targets.metrics.record_runtime_invalidation(source);
    info!(
        channel = source.label(),
        payload,
        caches_invalidated = plan.caches,
        circuit_reset_scope = ?scope,
        "runtime config invalidation applied"
    );
}

pub fn spawn_runtime_config_listener(
    pool: PgPool,
    targets: RuntimeInvalidationTargets,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = listen_once(&pool, &targets).await {
                warn!(error = %err, "runtime config listener disconnected");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    })
}

async fn listen_once(
    pool: &PgPool,
    targets: &RuntimeInvalidationTargets,
) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("moira_runtime_config").await?;
    info!("runtime config listener attached");

    loop {
        let notification = listener.recv().await?;
        apply_invalidation(
            targets,
            notification.payload(),
            InvalidationChannel::Postgres,
        )
        .await;
    }
}

/// The Redis pub/sub invalidation listener.
///
/// # A second channel, never a replacement
///
/// Postgres `LISTEN/NOTIFY` is authoritative and is fired by a database trigger,
/// so it is delivered for *every* runtime-config write regardless of which code
/// path performed it and regardless of whether Redis is enabled — which it is
/// not, by default. This listener exists only where Redis is enabled, and only as
/// a lower-latency second signal. A deployment with Redis off still invalidates,
/// on every replica, exactly as it did before plan 10.
///
/// # It runs the identical payload through the identical classifier
///
/// [`apply_invalidation`] is shared with the Postgres path, so the four-call
/// sequence and the `circuit_reset_scope` mapping cannot drift between them. A
/// message whose payload is not the `{resource_type, resource_id}` shape lands in
/// that function's unparseable arm and falls back to `CircuitResetScope::All`
/// with a `warn!` — safe, loud, and the reason `RuntimeConfigInvalidation` is the
/// only way to publish.
pub fn spawn_redis_invalidation_listener(
    redis: RedisClient,
    targets: RuntimeInvalidationTargets,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = subscribe_once(&redis, &targets).await {
                warn!(error = %err, "redis invalidation listener disconnected");
                targets
                    .metrics
                    .record_redis_operation_failure(RedisOperation::Subscribe);
                // The same backoff the Postgres listener uses. Deliberately not
                // exponential: this is a local dependency, an outage is measured
                // in seconds, and a listener that backs off to minutes would leave
                // the second channel dark long after the first recovered.
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    })
}

async fn subscribe_once(
    redis: &RedisClient,
    targets: &RuntimeInvalidationTargets,
) -> Result<(), AppError> {
    let mut pubsub = redis.subscribe_invalidation().await?;
    info!(
        channel = redis.invalidation_channel(),
        "redis invalidation listener attached"
    );
    let mut messages = pubsub.on_message();
    while let Some(message) = messages.next().await {
        let payload = match message.get_payload::<String>() {
            Ok(payload) => payload,
            Err(error) => {
                // A non-UTF-8 message is not ours. Skipped rather than treated as
                // an unparseable invalidation, because handing it to
                // `apply_invalidation` would reset every breaker in the process on
                // the say-so of anything with `PUBLISH` on this channel.
                warn!(%error, "redis invalidation message was not a string; ignoring");
                continue;
            }
        };
        apply_invalidation(targets, &payload, InvalidationChannel::Redis).await;
    }
    // The stream ending means the connection went away.
    Err(AppError::Config(
        "redis invalidation subscription ended".to_string(),
    ))
}

/// The `moira_runtime_config` payload, as `notify_moira_runtime_config_change` builds
/// it (`migrations/0004_admin_api_contract.sql:108-127`). The trigger carries no
/// `tg_op`, so INSERT, UPDATE and DELETE are indistinguishable here and the mapping
/// below must be correct for all three.
#[derive(Debug, Deserialize)]
struct RuntimeConfigChange {
    resource_type: String,
    resource_id: String,
}

/// Tables whose triggers fire on this channel but whose rows cannot change provider
/// health, so a change to one must not discard breaker state.
///
/// Two of these deserve their reasoning recorded, because "provider-scoped" makes them
/// look like candidates for a reset:
///
/// * `provider_runtime_policies` — `CircuitBreakerRegistry::before_call` already resets
///   an entry whose `policy_version` no longer matches, so a policy edit self-heals
///   exactly where it matters and a reset here would be redundant.
/// * `provider_credentials` — the payload carries the *credential* id, and mapping it to
///   its owning provider needs a query the listener would have to make on every
///   notification. Breaker state is derived from transport-level failures rather than
///   credential validity, and `RuntimeCacheKey` already carries `credential_version`, so
///   the rebuilt handle picks up the new secret. Left unscoped rather than half-guessed.
/// * `auth_provider_settings` — which OAuth/OIDC methods this deployment offers to
///   *humans* signing in to the console. It has no bearing on whether a model provider's
///   API is answering, so it belongs here rather than in the mapping below.
///
/// Anything not listed here and not mapped below is *unknown*, not unaffected — a table
/// added by a later migration whose triggers nobody taught this function about — and
/// falls back to a full reset.
///
/// `auth_provider_settings` is the table that proved the fallback is not free. Plan 07
/// attaches the existing NOTIFY trigger to it and describes that as reusing the existing
/// mechanism with no new behaviour. Without this entry it would have been reused into a
/// full breaker reset on every auth-settings write, plus a `warn!` per write — the caches
/// rebuild from a query, but breaker state is earned by observing real failures and
/// cannot be, so the reset sends live traffic straight back at a provider that was just
/// failing. **Adding a table to the trigger means adding it here in the same change.**
const CIRCUIT_UNAFFECTED_RESOURCE_TYPES: &[&str] = &[
    "agent_profiles",
    "application_conversation_policies",
    "application_embedding_policies",
    "application_execution_policies",
    "application_memory_policies",
    "application_retrieval_policies",
    "applications",
    "auth_provider_settings",
    "consumer_api_keys",
    "provider_credentials",
    "provider_runtime_policies",
    "rag_collections",
    "rag_documents",
    "route_definitions",
    "routing_policies",
    "system_api_keys",
    "trusted_jwt_issuers",
];

/// Resource types whose rows are **per-request data rather than configuration** (F51).
///
/// A write to one of these cannot change anything the three in-process caches hold —
/// provider configuration, built provider client handles, or the enabled auth methods —
/// so it must clear none of them, and it cannot change provider health either.
///
/// # This asks a different question from [`CIRCUIT_UNAFFECTED_RESOURCE_TYPES`]
///
/// That list answers *"can this row change whether a provider is answering?"*. This one
/// answers *"can this row change any cached configuration?"*. They are not the same
/// question and the difference is the whole point: `agent_profiles` cannot change
/// provider health and so is circuit-unaffected, but it very much changes cached runtime
/// configuration and must keep clearing the caches. Only a table that answers **no to
/// both** belongs here.
///
/// A name here is therefore implicitly circuit-unaffected as well, which is why these
/// names are *not* repeated in the list above — one table, one list, so the two cannot
/// disagree about it.
///
/// # These tables no longer carry the trigger either
///
/// `migrations/0022_conversations_and_memory_are_not_runtime_config.sql` drops the NOTIFY
/// trigger from both, so on the Postgres path this arm is unreachable today. It is kept
/// as the second of two independent barriers: the classifier still narrows correctly if
/// the trigger is ever re-attached, and it covers the Redis publish path, which is not
/// gated by any trigger at all. Either barrier alone closes F51; neither is load-bearing.
const RUNTIME_DATA_RESOURCE_TYPES: &[&str] = &["conversations", "memory_records"];

/// Every table wired to the `moira_runtime_config` channel, as the classifier below must
/// be able to handle it.
///
/// # This list is pinned, not retyped — and the distinction is F52
///
/// The predecessor of this constant lived inside a unit test whose doc comment claimed it
/// was "pinned here against the trigger list in `migrations/`". It was not pinned; it was
/// **retyped** — 21 names written by hand against a schema that had 24 — and the three it
/// omitted were exactly the three that fell through to the `other =>` arm and reset every
/// breaker on every replica. A guard that compares the classifier to a hand-written copy
/// of the classifier cannot detect drift in either; it only proves someone typed the same
/// thing twice.
///
/// So the inventory is now pinned against `pg_trigger`, which is an independent source,
/// by `the_notify_trigger_inventory_is_exactly_the_classified_set` in
/// `tests/runtime_notify_inventory.rs`. Attaching the trigger to a new table reds that
/// test until the table is added here and classified below — which is the obligation
/// `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` already states in prose and, until now, nothing
/// enforced.
///
/// Exported for that test, and for no other reason.
pub const TRIGGERED_RESOURCE_TYPES: &[&str] = &[
    "agent_profiles",
    "application_conversation_policies",
    "application_embedding_policies",
    "application_execution_policies",
    "application_memory_policies",
    "application_retrieval_policies",
    "applications",
    "auth_provider_settings",
    "consumer_api_keys",
    "provider_credentials",
    "provider_models",
    "provider_runtime_policies",
    "providers",
    "rag_collections",
    "rag_documents",
    "route_definitions",
    "routing_policies",
    "system_api_keys",
    "trusted_jwt_issuers",
];

/// Table names that Moira itself publishes onto the Redis invalidation channel.
///
/// Declared **here, beside the classifier**, and referenced by
/// `RuntimeAdminService` rather than re-typed as literals at each call site. That
/// is the whole guard: [`every_publishable_resource_type_is_classified`] proves
/// each of these maps to something other than [`CircuitResetScope::All`], so a
/// publisher cannot emit a name this function has never heard of — which would
/// otherwise turn every admin write into a full breaker reset on every replica,
/// silently, and only when Redis happened to be enabled.
pub const RESOURCE_TYPE_ROUTE_DEFINITIONS: &str = "route_definitions";
pub const RESOURCE_TYPE_ROUTING_POLICIES: &str = "routing_policies";
pub const RESOURCE_TYPE_AGENT_PROFILES: &str = "agent_profiles";
pub const RESOURCE_TYPE_PROVIDER_RUNTIME_POLICIES: &str = "provider_runtime_policies";

/// Every name the paragraph above governs.
pub const PUBLISHABLE_RESOURCE_TYPES: &[&str] = &[
    RESOURCE_TYPE_ROUTE_DEFINITIONS,
    RESOURCE_TYPE_ROUTING_POLICIES,
    RESOURCE_TYPE_AGENT_PROFILES,
    RESOURCE_TYPE_PROVIDER_RUNTIME_POLICIES,
];

/// Everything one notification must clear, decided from a single parse of its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InvalidationPlan {
    /// Whether to clear the three in-process configuration caches. One flag for all
    /// three, for the reason recorded on [`apply_invalidation`].
    caches: bool,
    /// Which breaker entries may be reset.
    circuits: CircuitResetScope,
}

impl InvalidationPlan {
    /// The fail-safe: clear every cache and reset every breaker.
    ///
    /// This is what a payload the classifier cannot understand has always done, and it
    /// is what F51's narrowing must never quietly weaken. Narrowing a payload that was
    /// *misread* is how a breaker goes stale with nothing to say so.
    const EVERYTHING: Self = Self {
        caches: true,
        circuits: CircuitResetScope::All,
    };
}

/// Maps one notification payload onto the caches and breaker entries it may clear.
///
/// Fails safe in every direction it cannot understand: an unparseable payload, a
/// `resource_id` that is not a UUID, or a `resource_type` this function has never heard
/// of all yield [`InvalidationPlan::EVERYTHING`], which is exactly the behaviour that
/// shipped before scoping existed. Narrowing must never turn a parse bug into a silently
/// stale breaker or a silently stale cache.
fn invalidation_plan(payload: &str) -> InvalidationPlan {
    let change = match serde_json::from_str::<RuntimeConfigChange>(payload) {
        Ok(change) => change,
        Err(err) => {
            warn!(
                error = %err,
                "runtime config notification payload did not parse; invalidating everything"
            );
            return InvalidationPlan::EVERYTHING;
        }
    };

    let Ok(resource_id) = Uuid::parse_str(&change.resource_id) else {
        warn!(
            resource_type = change.resource_type,
            "runtime config notification carried a non-uuid resource id; invalidating everything"
        );
        return InvalidationPlan::EVERYTHING;
    };

    match change.resource_type.as_str() {
        // `CircuitBreakerRegistry` keys on `(provider_id, model_id)`, so `providers`
        // rows scope by the first element and `provider_models` rows by the second.
        "providers" => InvalidationPlan {
            caches: true,
            circuits: CircuitResetScope::Provider(resource_id),
        },
        "provider_models" => InvalidationPlan {
            caches: true,
            circuits: CircuitResetScope::Model(resource_id),
        },
        // Checked before the circuit list, and the only arm that clears no cache.
        other if RUNTIME_DATA_RESOURCE_TYPES.contains(&other) => InvalidationPlan {
            caches: false,
            circuits: CircuitResetScope::Unaffected,
        },
        other if CIRCUIT_UNAFFECTED_RESOURCE_TYPES.contains(&other) => InvalidationPlan {
            caches: true,
            circuits: CircuitResetScope::Unaffected,
        },
        other => {
            warn!(
                resource_type = other,
                "unrecognised runtime config resource type; invalidating everything"
            );
            InvalidationPlan::EVERYTHING
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The breaker half of [`invalidation_plan`], which is all most of these tests ask
    /// about. One line delegating to the real function, so it cannot drift from it —
    /// unlike a second copy of the mapping, which is the mistake F52 records.
    fn circuit_reset_scope(payload: &str) -> CircuitResetScope {
        invalidation_plan(payload).circuits
    }

    fn payload(resource_type: &str, resource_id: Uuid) -> String {
        format!(r#"{{"resource_type":"{resource_type}","resource_id":"{resource_id}"}}"#)
    }

    /// F51's property, stated as what gets invalidated rather than as what arrives.
    ///
    /// # Why this is not "a notification was classified"
    ///
    /// A guard that only counted notifications, or only checked the breaker scope, passes
    /// identically whether or not the caches are cleared — and the caches were the whole
    /// finding: three unconditional `invalidate_all()` calls, one of which drops built
    /// provider clients and their connection pools. So the assertion is on the `caches`
    /// flag, per resource type.
    ///
    /// # The three cases are one fact, not three
    ///
    /// A data table that clears nothing is only meaningful beside a config table that
    /// clears everything — otherwise `caches: false` for *everything* would pass the
    /// first assertion — and beside an unknown table that still clears everything, which
    /// is the fail-safe F51's narrowing must not have eaten. Asserted together for the
    /// reason §3.4 records about conjunctions.
    #[test]
    fn only_configuration_changes_invalidate_the_configuration_caches() {
        let id = Uuid::now_v7();

        // Per-request data: the finding. Clears no cache and no breaker.
        for resource_type in RUNTIME_DATA_RESOURCE_TYPES {
            let plan = invalidation_plan(&payload(resource_type, id));
            assert_eq!(
                plan,
                InvalidationPlan {
                    caches: false,
                    circuits: CircuitResetScope::Unaffected,
                },
                "a {resource_type} write is per-request data; invalidating the caches on it \
                 drops every replica's runtime config and every built provider client, \
                 connection pools included"
            );
        }

        // Configuration: unchanged. Every cache still cleared.
        for resource_type in ["providers", "applications", "auth_provider_settings"] {
            assert!(
                invalidation_plan(&payload(resource_type, id)).caches,
                "a {resource_type} write is configuration and must still clear the caches; \
                 narrowing the data tables must not have narrowed these with them"
            );
        }

        // Unknown: the fail-safe, still intact in both directions.
        assert_eq!(
            invalidation_plan(&payload("some_future_table", id)),
            InvalidationPlan::EVERYTHING,
            "an unrecognised resource type must still clear everything and reset every \
             breaker; narrowing a payload that was misread is how a cache goes stale silently"
        );
        assert_eq!(
            invalidation_plan("not json"),
            InvalidationPlan::EVERYTHING,
            "an unparseable payload must still clear everything"
        );
    }

    /// The two lists answer different questions, so no name may appear in both.
    ///
    /// A table in [`RUNTIME_DATA_RESOURCE_TYPES`] is already implicitly circuit-unaffected
    /// — its arm returns [`CircuitResetScope::Unaffected`] and is matched first — so
    /// repeating it in [`CIRCUIT_UNAFFECTED_RESOURCE_TYPES`] would create a second place
    /// to edit and a way for the two to disagree about one table. This is the duplicate-list
    /// shape F52 is about, caught at the point it would be introduced.
    #[test]
    fn no_resource_type_is_in_both_classification_lists() {
        for resource_type in RUNTIME_DATA_RESOURCE_TYPES {
            assert!(
                !CIRCUIT_UNAFFECTED_RESOURCE_TYPES.contains(resource_type),
                "{resource_type} is in both classification lists; the runtime-data arm \
                 already implies circuit-unaffected, so the second entry is dead and can \
                 only drift"
            );
        }
    }

    #[test]
    fn notify_payload_parses_resource_type_and_id() {
        let provider_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();

        assert_eq!(
            circuit_reset_scope(&format!(
                r#"{{"resource_type":"providers","resource_id":"{provider_id}"}}"#
            )),
            CircuitResetScope::Provider(provider_id)
        );
        assert_eq!(
            circuit_reset_scope(&format!(
                r#"{{"resource_type":"provider_models","resource_id":"{model_id}"}}"#
            )),
            CircuitResetScope::Model(model_id)
        );
        assert_eq!(
            circuit_reset_scope(&format!(
                r#"{{"resource_type":"applications","resource_id":"{model_id}"}}"#
            )),
            CircuitResetScope::Unaffected
        );
    }

    /// Which auth methods humans may sign in to the console with says nothing about
    /// whether a model provider's API is answering, so an auth-settings write must not
    /// discard breaker state.
    ///
    /// Pinned separately from [`every_triggered_table_has_a_scope`] because that test
    /// only asserts *not* [`CircuitResetScope::All`], and the failure this guards is
    /// specific: plan 07 attaches the existing NOTIFY trigger to `auth_provider_settings`
    /// and describes it as reusing the existing mechanism with no new behaviour. Reused
    /// without the allow-list entry, it resets every provider circuit on every write.
    /// The caches rebuild from a query; breaker state is earned by observing real
    /// failures and cannot be, so the reset sends live traffic back at a provider that
    /// was just failing.
    #[test]
    fn an_auth_settings_write_leaves_provider_breakers_alone() {
        assert_eq!(
            circuit_reset_scope(&format!(
                r#"{{"resource_type":"auth_provider_settings","resource_id":"{}"}}"#,
                Uuid::now_v7()
            )),
            CircuitResetScope::Unaffected
        );
    }

    /// Every table wired to the `moira_runtime_config` channel must be fully classified.
    ///
    /// A table the classifier has never heard of falls to the `other =>` arm, which is
    /// safe but silently undoes every bit of narrowing at once: a full breaker reset and
    /// a full cache wipe, on every replica, per row.
    ///
    /// # What this test is and is not
    ///
    /// It proves every name in [`TRIGGERED_RESOURCE_TYPES`] classifies correctly. It does
    /// **not** prove that constant matches the schema — it cannot, because it has no
    /// database, and a version of this test that carried its own copy of the list is
    /// exactly the F52 defect. The schema half is
    /// `the_notify_trigger_inventory_is_exactly_the_classified_set` in
    /// `tests/runtime_notify_inventory.rs`, which reads `pg_trigger`. Neither half is
    /// sufficient alone and neither duplicates the other.
    ///
    /// # Both halves of the plan, not just the breaker scope
    ///
    /// The `caches` assertion is what stops a *configuration* table being quietly moved
    /// into [`RUNTIME_DATA_RESOURCE_TYPES`]: that would leave its scope `Unaffected` and
    /// so satisfy the breaker assertion, while stopping every replica from ever noticing
    /// the config change. A table that genuinely is per-request data loses its trigger in
    /// the same change, which takes it out of this list and out of this test's reach.
    #[test]
    fn every_triggered_table_has_a_scope() {
        let resource_id = Uuid::now_v7();
        for table in TRIGGERED_RESOURCE_TYPES {
            let plan = invalidation_plan(&format!(
                r#"{{"resource_type":"{table}","resource_id":"{resource_id}"}}"#
            ));
            assert_ne!(
                plan.circuits,
                CircuitResetScope::All,
                "{table} is triggered but unclassified, so it still resets every circuit"
            );
            assert!(
                plan.caches,
                "{table} still carries the NOTIFY trigger but no longer invalidates the \
                 caches, so a change to it never reaches another replica; a table that is \
                 really per-request data must lose its trigger in the same change"
            );
        }
    }

    /// The guard on the Redis publisher.
    ///
    /// `RuntimeAdminService` publishes each of these onto the invalidation
    /// channel, and the subscriber classifies the payload with the very function
    /// under test. A name this function does not recognise falls back to
    /// [`CircuitResetScope::All`] — so an unclassified publishable name would
    /// discard every provider's earned breaker health on every admin write, on
    /// every replica, and only in deployments that had turned Redis on.
    #[test]
    fn every_publishable_resource_type_is_classified() {
        let resource_id = Uuid::now_v7();
        for resource_type in PUBLISHABLE_RESOURCE_TYPES {
            let scope = circuit_reset_scope(&format!(
                r#"{{"resource_type":"{resource_type}","resource_id":"{resource_id}"}}"#
            ));
            assert_ne!(
                scope,
                CircuitResetScope::All,
                "{resource_type} is published but unclassified, so publishing it resets \
                 every circuit in every replica"
            );
        }
    }

    /// Both listeners must be reachable only through [`apply_invalidation`], which
    /// is what keeps the four-call sequence identical between them. Asserted on
    /// the source rather than the behaviour because the failure is an *omission* —
    /// a future subscriber that inlines three of the four calls compiles, passes
    /// every behavioural test that does not happen to read the auth-settings
    /// cache, and leaves plan 07's identity configuration stale on every replica
    /// that learned of a change over Redis.
    #[test]
    fn both_listeners_share_one_invalidation_sequence() {
        let source = include_str!("db.rs");
        // Needles are assembled from fragments rather than written out, because
        // this file includes its own source: a literal needle would match itself
        // and the count would be off by one for reasons nothing in the failure
        // message would explain.
        let invalidate = concat!("cache.invalidate_all", "().await");
        let scoped_reset = concat!("reset_for_resource", "(scope).await");
        let reset_all = concat!("circuits.reset_all", "()");

        assert_eq!(
            source.matches(invalidate).count(),
            1,
            "invalidate_all is called outside apply_invalidation; both listeners must \
             route through it"
        );
        assert_eq!(
            source.matches(scoped_reset).count(),
            1,
            "a second scoped breaker reset means a second copy of the sequence"
        );
        assert!(
            !source.contains(reset_all),
            "reset_all on an invalidation path discards every provider's earned \
             breaker health; the scoped reset exists precisely to avoid that"
        );
    }

    #[test]
    fn malformed_notify_payload_falls_back_to_reset_all() {
        // Not JSON at all.
        assert_eq!(circuit_reset_scope("not json"), CircuitResetScope::All);
        // JSON, but not this payload — the shape emitted by the superseded
        // `notify_moira_runtime_config_change` in migrations 0002 and 0003.
        assert_eq!(
            circuit_reset_scope(r#"{"table":"providers","operation":"UPDATE"}"#),
            CircuitResetScope::All
        );
        // Right shape, unusable id.
        assert_eq!(
            circuit_reset_scope(r#"{"resource_type":"providers","resource_id":"not-a-uuid"}"#),
            CircuitResetScope::All
        );
        // Right shape, a table nobody taught this function about.
        assert_eq!(
            circuit_reset_scope(&format!(
                r#"{{"resource_type":"some_future_table","resource_id":"{}"}}"#,
                Uuid::now_v7()
            )),
            CircuitResetScope::All
        );
    }
}
