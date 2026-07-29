use async_trait::async_trait;
use sqlx::{PgPool, Row};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetupReadinessSnapshot {
    pub root_system_key: bool,
    pub application: bool,
    pub route: bool,
    pub provider: bool,
    pub provider_model: bool,
    pub provider_credential: bool,
    pub routing_policy: bool,
    pub executable_path: bool,
}

#[derive(Clone)]
pub struct PgSetupRepository {
    pool: PgPool,
}

/// The setup-readiness read surface, abstracted so `SetupService` can be exercised without a
/// Postgres instance. Mirrors [`AdminRepository`](super::AdminRepository): the trait carries the
/// documentation and the `#[async_trait] impl` below carries only SQL.
#[async_trait]
pub trait SetupRepository: Send + Sync {
    /// Answers every setup check in a single round trip. The query is deliberately read-only and
    /// never selects secret material — see `readiness_query_never_selects_secret_material`.
    async fn inspect(&self) -> Result<SetupReadinessSnapshot, AppError>;
}

impl PgSetupRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SetupRepository for PgSetupRepository {
    async fn inspect(&self) -> Result<SetupReadinessSnapshot, AppError> {
        let row = sqlx::query(SETUP_READINESS_SQL)
            .fetch_one(&self.pool)
            .await?;

        Ok(SetupReadinessSnapshot {
            root_system_key: row.try_get("root_system_key")?,
            application: row.try_get("application")?,
            route: row.try_get("route")?,
            provider: row.try_get("provider")?,
            provider_model: row.try_get("provider_model")?,
            provider_credential: row.try_get("provider_credential")?,
            routing_policy: row.try_get("routing_policy")?,
            executable_path: row.try_get("executable_path")?,
        })
    }
}

const SETUP_READINESS_SQL: &str = r#"
with
eligible_routes as (
    select id
    from route_definitions
    where status = 'active' and deleted_at is null
),
eligible_applications as (
    select a.id
    from applications a
    left join application_execution_policies aep on aep.application_id = a.id
    where a.status = 'active'
      and a.deleted_at is null
      and coalesce(aep.responses_enabled, true)
),
supported_providers as (
    select id, provider_type
    from providers
    where status = 'active'
      and deleted_at is null
      and provider_type in (
          'openai_compatible',
          'openai',
          'anthropic',
          'gemini',
          'deepseek',
          'azure_openai',
          'local'
      )
),
eligible_models as (
    select pm.id, pm.provider_id
    from provider_models pm
    join supported_providers p on p.id = pm.provider_id
    where pm.status = 'active' and pm.deleted_at is null
),
eligible_credentials as (
    select pc.provider_id, pc.scope_type, pc.application_id, pc.external_tenant_id
    from provider_credentials pc
    join supported_providers p on p.id = pc.provider_id
    where pc.status = 'active'
      and pc.deleted_at is null
      and (pc.expires_at is null or pc.expires_at > now())
      and (
          (p.provider_type = 'azure_openai'
              and pc.credential_type in ('azure_openai', 'api_key'))
          or
          (p.provider_type in ('openai_compatible', 'openai', 'local')
              and pc.credential_type in ('api_key', 'bearer_token'))
          or
          (p.provider_type in ('anthropic', 'gemini', 'deepseek')
              and pc.credential_type = 'api_key')
      )
),
matching_policies as (
    select rp.id, rp.application_id, rp.external_tenant_id, rp.provider_id
    from routing_policies rp
    join eligible_routes er on er.id = rp.route_id
    join supported_providers p on p.id = rp.provider_id
    join eligible_models pm
      on pm.id = rp.provider_model_id
     and pm.provider_id = rp.provider_id
    left join provider_runtime_policies prp on prp.provider_id = rp.provider_id
    where rp.status = 'active'
      and rp.deleted_at is null
      and coalesce(prp.status, 'active') = 'active'
)
select
    exists(
        select 1
        from system_api_keys
        where status = 'active'
          and deleted_at is null
          and revoked_at is null
          and (expires_at is null or expires_at > now())
          and 'moira:admin' = any(scopes)
    ) as root_system_key,
    exists(select 1 from eligible_applications) as application,
    exists(select 1 from eligible_routes) as route,
    exists(select 1 from supported_providers) as provider,
    exists(select 1 from eligible_models) as provider_model,
    exists(select 1 from eligible_credentials) as provider_credential,
    exists(select 1 from matching_policies) as routing_policy,
    exists(
        select 1
        from eligible_applications a
        join matching_policies rp
          on rp.external_tenant_id is null
         and (rp.application_id is null or rp.application_id = a.id)
        join eligible_credentials pc
          on pc.provider_id = rp.provider_id
         and pc.external_tenant_id is null
         and (
             pc.scope_type = 'global'
             or (pc.scope_type = 'application' and pc.application_id = a.id)
         )
    ) as executable_path
"#;

/// In-memory [`SetupRepository`] for unit tests. Holds only the boolean readiness flags the real
/// query returns; it never carries credential material, because none is on this surface.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InMemorySetupRepository {
    snapshot: SetupReadinessSnapshot,
}

#[cfg(test)]
impl InMemorySetupRepository {
    pub(crate) fn new(snapshot: SetupReadinessSnapshot) -> Self {
        Self { snapshot }
    }
}

#[cfg(test)]
#[async_trait]
impl SetupRepository for InMemorySetupRepository {
    async fn inspect(&self) -> Result<SetupReadinessSnapshot, AppError> {
        Ok(self.snapshot)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn migrated_database_supports_setup_inspection() {
        let Ok(database_url) = std::env::var("MOIRA_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping setup repository integration: set MOIRA_TEST_DATABASE_URL to run it"
            );
            return;
        };
        let pool = PgPool::connect(&database_url).await.unwrap();
        crate::infra::db::migrate(&pool).await.unwrap();
        PgSetupRepository::new(pool)
            .inspect()
            .await
            .expect("setup readiness snapshot");
    }

    #[test]
    fn readiness_query_never_selects_secret_material() {
        for forbidden in [
            "encrypted_payload",
            "encrypted_data_key",
            "key_hash",
            "key_prefix",
            "masked_secret",
            "secret_fingerprint",
        ] {
            assert!(
                !SETUP_READINESS_SQL.contains(forbidden),
                "query must not select or inspect {forbidden}"
            );
        }
    }

    /// `SetupService` stores its repository as `Arc<dyn SetupRepository>`, which only compiles
    /// while the trait stays object-safe. Adding a generic method or a `Self: Sized` receiver
    /// would break that silently at the consumer; this asserts it here, at the definition.
    #[test]
    fn setup_repository_trait_is_object_safe() {
        let repo: Arc<dyn SetupRepository> = Arc::new(InMemorySetupRepository::default());
        let _: &dyn SetupRepository = repo.as_ref();
    }

    #[tokio::test]
    async fn fake_setup_repository_reports_incomplete_configuration() {
        let repo = InMemorySetupRepository::new(SetupReadinessSnapshot {
            root_system_key: true,
            ..SetupReadinessSnapshot::default()
        });
        let snapshot = repo.inspect().await.expect("fake snapshot");
        assert!(snapshot.root_system_key);
        assert!(!snapshot.executable_path);
    }

    #[test]
    fn readiness_query_matches_runtime_credential_compatibility() {
        assert!(SETUP_READINESS_SQL.contains("'azure_openai', 'api_key'"));
        assert!(SETUP_READINESS_SQL.contains("'api_key', 'bearer_token'"));
        assert!(SETUP_READINESS_SQL.contains("'anthropic', 'gemini', 'deepseek'"));
        assert!(!SETUP_READINESS_SQL.contains("'custom'"));
    }
}
