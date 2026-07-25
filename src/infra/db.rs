use std::time::Duration;

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
    orchestration::{
        CircuitBreakerRegistry, CircuitResetScope, ProviderRuntimeCache, RuntimeConfigCache,
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

pub fn spawn_runtime_config_listener(
    pool: PgPool,
    cache: RuntimeConfigCache,
    runtime_handles: ProviderRuntimeCache,
    circuits: CircuitBreakerRegistry,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = listen_once(&pool, &cache, &runtime_handles, &circuits).await {
                warn!(error = %err, "runtime config listener disconnected");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    })
}

async fn listen_once(
    pool: &PgPool,
    cache: &RuntimeConfigCache,
    runtime_handles: &ProviderRuntimeCache,
    circuits: &CircuitBreakerRegistry,
) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("moira_runtime_config").await?;
    info!("runtime config listener attached");

    loop {
        let notification = listener.recv().await?;
        let scope = circuit_reset_scope(notification.payload());
        // The two caches stay unconditional: they are keyed by version and rebuild on
        // the next read, so re-reading them costs a query. Breaker state is earned by
        // observing real failures and cannot be rebuilt, which is why only it is scoped.
        cache.invalidate_all().await;
        runtime_handles.invalidate_all().await;
        circuits.reset_for_resource(scope).await;
        info!(
            channel = notification.channel(),
            payload = notification.payload(),
            circuit_reset_scope = ?scope,
            "runtime config cache invalidated"
        );
    }
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
///
/// Anything not listed here and not mapped below is *unknown*, not unaffected — a table
/// added by a later migration whose triggers nobody taught this function about — and
/// falls back to a full reset.
const CIRCUIT_UNAFFECTED_RESOURCE_TYPES: &[&str] = &[
    "agent_profiles",
    "application_conversation_policies",
    "application_embedding_policies",
    "application_execution_policies",
    "application_memory_policies",
    "application_retrieval_policies",
    "applications",
    "consumer_api_keys",
    "conversations",
    "memory_records",
    "provider_credentials",
    "provider_runtime_policies",
    "rag_collections",
    "rag_documents",
    "route_definitions",
    "routing_policies",
    "system_api_keys",
    "trusted_jwt_issuers",
];

/// Maps one notification payload onto the breaker entries it may clear.
///
/// Fails safe in every direction it cannot understand: an unparseable payload, a
/// `resource_id` that is not a UUID, or a `resource_type` this function has never heard
/// of all yield [`CircuitResetScope::All`], which is exactly the behaviour that shipped
/// before scoping existed. Narrowing must never turn a parse bug into a silently stale
/// breaker.
fn circuit_reset_scope(payload: &str) -> CircuitResetScope {
    let change = match serde_json::from_str::<RuntimeConfigChange>(payload) {
        Ok(change) => change,
        Err(err) => {
            warn!(
                error = %err,
                "runtime config notification payload did not parse; resetting every circuit"
            );
            return CircuitResetScope::All;
        }
    };

    let Ok(resource_id) = Uuid::parse_str(&change.resource_id) else {
        warn!(
            resource_type = change.resource_type,
            "runtime config notification carried a non-uuid resource id; resetting every circuit"
        );
        return CircuitResetScope::All;
    };

    match change.resource_type.as_str() {
        // `CircuitBreakerRegistry` keys on `(provider_id, model_id)`, so `providers`
        // rows scope by the first element and `provider_models` rows by the second.
        "providers" => CircuitResetScope::Provider(resource_id),
        "provider_models" => CircuitResetScope::Model(resource_id),
        other if CIRCUIT_UNAFFECTED_RESOURCE_TYPES.contains(&other) => {
            CircuitResetScope::Unaffected
        }
        other => {
            warn!(
                resource_type = other,
                "unrecognised runtime config resource type; resetting every circuit"
            );
            CircuitResetScope::All
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Every table wired to the `moira_runtime_config` channel must be classified.
    /// A table this function has never heard of is treated as unknown and falls back
    /// to a full reset, which is safe but silently undoes the narrowing — so the list
    /// is pinned here against the trigger list in `migrations/`.
    #[test]
    fn every_triggered_table_has_a_scope() {
        let triggered = [
            "agent_profiles",
            "application_conversation_policies",
            "application_embedding_policies",
            "application_execution_policies",
            "application_memory_policies",
            "application_retrieval_policies",
            "applications",
            "consumer_api_keys",
            "conversations",
            "memory_records",
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
        let resource_id = Uuid::now_v7();
        for table in triggered {
            let scope = circuit_reset_scope(&format!(
                r#"{{"resource_type":"{table}","resource_id":"{resource_id}"}}"#
            ));
            assert_ne!(
                scope,
                CircuitResetScope::All,
                "{table} is triggered but unclassified, so it still resets every circuit"
            );
        }
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
