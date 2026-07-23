use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    domain::{CredentialType, OwnerScope, ProviderConfig, ScopeType},
    error::AppError,
    infra::pg_rows::{credential_type_from_db, provider_from_row, scope_type_from_db},
    security::{
        Actor, CredentialAadParts, EncryptedSecret, SecretCipher,
        credential_aad as envelope_credential_aad,
    },
};

#[derive(Debug, Clone)]
pub struct RuntimeConfigCache {
    ttl: Duration,
    providers: Arc<RwLock<HashMap<Uuid, CacheEntry<ProviderConfig>>>>,
}

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    value: T,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub provider: ProviderConfig,
    pub model: String,
    pub api_key: String,
}

#[derive(Debug, Clone)]
pub struct CredentialCandidate {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub credential_type: CredentialType,
    pub scope_type: ScopeType,
    pub external_tenant_id: Option<String>,
    pub application_id: Option<Uuid>,
    pub external_user_id: Option<String>,
    pub encrypted: EncryptedSecret,
    pub expires_at: Option<DateTime<Utc>>,
    pub priority: i32,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl RuntimeConfigCache {
    pub fn new(ttl_seconds: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_seconds),
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_provider(&self, id: Uuid) -> Option<ProviderConfig> {
        let providers = self.providers.read().await;
        providers
            .get(&id)
            .filter(|entry| entry.expires_at > Instant::now())
            .map(|entry| entry.value.clone())
    }

    pub async fn put_provider(&self, provider: ProviderConfig) {
        let mut providers = self.providers.write().await;
        providers.insert(
            provider.id,
            CacheEntry {
                value: provider,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    pub async fn invalidate_all(&self) {
        self.providers.write().await.clear();
    }
}

pub async fn resolve_provider(
    pool: &PgPool,
    cache: &RuntimeConfigCache,
    cipher: &impl SecretCipher,
    actor: &Actor,
    provider_id: Option<Uuid>,
    requested_model: Option<&str>,
    explicit_api_key: Option<&str>,
) -> Result<ResolvedProvider, AppError> {
    let provider = match provider_id {
        Some(id) => get_provider(pool, cache, id).await?,
        None => find_default_provider(pool, actor).await?,
    };

    if !provider.enabled {
        return Err(AppError::Forbidden("provider is disabled".to_string()));
    }

    let model = requested_model
        .filter(|value| !value.is_empty())
        .unwrap_or(&provider.default_model)
        .to_string();

    let api_key = match explicit_api_key {
        Some(key) => key.to_string(),
        None => resolve_api_key(pool, cipher, actor, provider.id).await?,
    };

    Ok(ResolvedProvider {
        provider,
        model,
        api_key,
    })
}

pub async fn get_provider(
    pool: &PgPool,
    cache: &RuntimeConfigCache,
    id: Uuid,
) -> Result<ProviderConfig, AppError> {
    if let Some(provider) = cache.get_provider(id).await {
        return Ok(provider);
    }

    let sql = provider_runtime_select_sql("where p.id = $1 and p.deleted_at is null");
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("provider {id}")))?;

    let provider = provider_from_row(&row)?;
    cache.put_provider(provider.clone()).await;
    Ok(provider)
}

pub async fn find_default_provider(
    pool: &PgPool,
    _actor: &Actor,
) -> Result<ProviderConfig, AppError> {
    let sql = provider_runtime_select_sql(
        "where p.status = 'active' and p.deleted_at is null order by p.created_at asc limit 1",
    );
    let row = sqlx::query(&sql)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("no enabled provider is configured".to_string()))?;
    provider_from_row(&row)
}

pub async fn resolve_api_key(
    pool: &PgPool,
    cipher: &impl SecretCipher,
    actor: &Actor,
    provider_id: Uuid,
) -> Result<String, AppError> {
    let rows = sqlx::query(
        r#"
        select id, provider_id, credential_type, scope_type, external_tenant_id,
               application_id, external_user_id, encrypted_payload, encryption_algorithm,
               encryption_version, encrypted_data_key, nonce, expires_at, priority,
               last_validated_at, last_used_at
        from provider_credentials
        where provider_id = $1
          and credential_type = 'api_key'
          and status = 'active'
          and deleted_at is null
          and (expires_at is null or expires_at > now())
          and (
            (scope_type = 'user'
                and external_user_id = $2
                and (application_id is null or application_id = $3)
                and (external_tenant_id is null or external_tenant_id = $4))
            or (scope_type = 'application'
                and application_id = $3
                and (external_tenant_id is null or external_tenant_id = $4))
            or (scope_type = 'tenant' and external_tenant_id = $4)
            or (scope_type = 'global')
          )
        "#,
    )
    .bind(provider_id)
    .bind(actor.external_user_id.as_ref().or(actor.subject.as_ref()))
    .bind(actor.internal_application_id)
    .bind(
        actor
            .external_tenant_id
            .as_ref()
            .or(actor.tenant_id.as_ref()),
    )
    .fetch_all(pool)
    .await?;

    let mut candidates = Vec::new();
    for row in rows {
        candidates.push(CredentialCandidate {
            id: row.try_get("id")?,
            provider_id: row.try_get("provider_id")?,
            credential_type: credential_type_from_db(row.try_get::<String, _>("credential_type")?)?,
            scope_type: scope_type_from_db(row.try_get::<String, _>("scope_type")?)?,
            external_tenant_id: row.try_get("external_tenant_id")?,
            application_id: row.try_get("application_id")?,
            external_user_id: row.try_get("external_user_id")?,
            encrypted: EncryptedSecret {
                algorithm: row.try_get("encryption_algorithm")?,
                version: row.try_get("encryption_version")?,
                key_id: String::new(),
                encrypted_data_key: row.try_get("encrypted_data_key")?,
                nonce: row.try_get("nonce")?,
                ciphertext: row.try_get("encrypted_payload")?,
            },
            expires_at: row.try_get("expires_at")?,
            priority: row.try_get("priority")?,
            last_validated_at: row.try_get("last_validated_at")?,
            last_used_at: row.try_get("last_used_at")?,
        });
    }

    candidates.sort_by_key(|candidate| {
        (
            credential_priority(candidate),
            candidate.priority,
            candidate.last_validated_at.map(std::cmp::Reverse),
            candidate.last_used_at,
        )
    });
    let candidate = candidates
        .into_iter()
        .next()
        .ok_or_else(|| AppError::NotFound("no usable provider credential".to_string()))?;
    let aad = envelope_credential_aad(CredentialAadParts {
        credential_id: candidate.id,
        provider_id: candidate.provider_id,
        credential_type: "api_key",
        scope_type: scope_type_to_aad(&candidate.scope_type),
        external_tenant_id: candidate.external_tenant_id.as_deref(),
        application_id: candidate.application_id,
        external_user_id: candidate.external_user_id.as_deref(),
        encryption_version: candidate.encrypted.version,
    });
    let plaintext = cipher.decrypt(&candidate.encrypted, aad.as_bytes())?;
    let value: serde_json::Value = serde_json::from_slice(&plaintext)
        .map_err(|err| AppError::Internal(format!("credential payload is invalid JSON: {err}")))?;
    api_key_from_credential_payload(value)
}

pub fn credential_priority(candidate: &CredentialCandidate) -> u8 {
    match candidate.scope_type {
        ScopeType::User
            if candidate.application_id.is_some() && candidate.external_tenant_id.is_some() =>
        {
            1
        }
        ScopeType::User if candidate.application_id.is_some() => 2,
        ScopeType::User if candidate.external_tenant_id.is_some() => 3,
        ScopeType::User => 4,
        ScopeType::Application if candidate.external_tenant_id.is_some() => 5,
        ScopeType::Application => 6,
        ScopeType::Tenant => 7,
        ScopeType::Global => 8,
    }
}

pub fn credential_aad(
    provider_id: Uuid,
    owner_scope: &OwnerScope,
    owner_id: Option<&str>,
) -> String {
    format!(
        "provider:{provider_id}:scope:{owner_scope:?}:owner:{}",
        owner_id.unwrap_or("")
    )
}

pub fn normalize_openai_base_url(base_url: &str) -> Result<String, AppError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::BadRequest(format!("invalid provider base_url: {err}")))?;
    if parsed.path().trim_end_matches('/').ends_with("/v1") {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}/v1"))
    }
}

fn provider_runtime_select_sql(suffix: &str) -> String {
    format!(
        r#"
        select p.id,
               null::text as tenant_id,
               null::text as application_id,
               p.display_name as name,
               p.provider_type as kind,
               p.base_url,
               coalesce((
                   select pm.model_key
                   from provider_models pm
                   where pm.provider_id = p.id
                     and pm.status = 'active'
                     and pm.deleted_at is null
                   order by pm.created_at asc
                   limit 1
               ), '') as default_model,
               (p.status = 'active') as enabled,
               coalesce(nullif(p.metadata->>'legacy_timeout_ms', '')::integer, 120000) as timeout_ms,
               p.metadata,
               p.created_at,
               p.updated_at
        from providers p
        {suffix}
        "#
    )
}

fn scope_type_to_aad(scope_type: &ScopeType) -> &'static str {
    match scope_type {
        ScopeType::Global => "global",
        ScopeType::Tenant => "tenant",
        ScopeType::Application => "application",
        ScopeType::User => "user",
    }
}

fn api_key_from_credential_payload(value: serde_json::Value) -> Result<String, AppError> {
    if let Some(secret) = value.as_str() {
        return Ok(secret.to_string());
    }
    value
        .get("api_key")
        .or_else(|| value.get("token"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::Forbidden("credential payload does not contain an api key".to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_vllm_base_url_to_openai_v1() {
        assert_eq!(
            normalize_openai_base_url("http://192.168.1.13:8000").unwrap(),
            "http://192.168.1.13:8000/v1"
        );
        assert_eq!(
            normalize_openai_base_url("http://localhost:8000/v1/").unwrap(),
            "http://localhost:8000/v1"
        );
    }

    #[test]
    fn credential_priority_uses_approved_resolution_order() {
        let provider_id = Uuid::now_v7();
        let encrypted = EncryptedSecret {
            algorithm: "AES-256-GCM".to_string(),
            version: 1,
            key_id: "k".to_string(),
            encrypted_data_key: None,
            nonce: vec![1; 12],
            ciphertext: vec![2; 16],
        };
        let mut candidates = [
            CredentialCandidate {
                id: Uuid::now_v7(),
                provider_id,
                credential_type: CredentialType::ApiKey,
                scope_type: ScopeType::Global,
                external_tenant_id: None,
                application_id: None,
                external_user_id: None,
                encrypted: encrypted.clone(),
                expires_at: None,
                priority: 100,
                last_validated_at: None,
                last_used_at: None,
            },
            CredentialCandidate {
                id: Uuid::now_v7(),
                provider_id,
                credential_type: CredentialType::ApiKey,
                scope_type: ScopeType::Application,
                external_tenant_id: None,
                application_id: Some(Uuid::now_v7()),
                external_user_id: None,
                encrypted: encrypted.clone(),
                expires_at: None,
                priority: 100,
                last_validated_at: None,
                last_used_at: None,
            },
            CredentialCandidate {
                id: Uuid::now_v7(),
                provider_id,
                credential_type: CredentialType::ApiKey,
                scope_type: ScopeType::User,
                external_tenant_id: Some("tenant".to_string()),
                application_id: Some(Uuid::now_v7()),
                external_user_id: Some("user".to_string()),
                encrypted,
                expires_at: None,
                priority: 100,
                last_validated_at: None,
                last_used_at: None,
            },
        ];

        candidates.sort_by_key(credential_priority);
        assert_eq!(credential_priority(&candidates[0]), 1);
        assert!(credential_aad(provider_id, &OwnerScope::User, Some("user")).contains("provider:"));
    }
}
