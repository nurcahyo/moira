use std::sync::Arc;

use reqwest::Client;
use sqlx::PgPool;

use crate::{
    config::Settings,
    error::AppError,
    infra::{metrics::MetricsRegistry, redis::RedisClient, workers::WorkerRegistry},
    orchestration::{
        CircuitBreakerRegistry, ConcurrencyController, InMemoryRateLimiter, ProviderRuntimeCache,
        RuntimeConfigCache,
    },
    security::{
        AdminAuthenticator, ApiKeyHasher, AuthService, AuthorizationService, CallerAuthenticator,
        IdempotencyHasher, JwksCache, LocalSecretCipher,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub settings: Arc<Settings>,
    pub pool: Option<PgPool>,
    pub http: Client,
    pub cipher: LocalSecretCipher,
    pub key_hasher: ApiKeyHasher,
    pub idempotency_hasher: IdempotencyHasher,
    pub auth: AuthService,
    pub authz: AuthorizationService,
    pub redis: Option<RedisClient>,
    pub metrics: MetricsRegistry,
    pub workers: WorkerRegistry,
    pub runtime_cache: RuntimeConfigCache,
    pub runtime_handles: ProviderRuntimeCache,
    pub concurrency: ConcurrencyController,
    pub public_rate_limiter: InMemoryRateLimiter,
    pub circuits: CircuitBreakerRegistry,
    pub admin_auth: AdminAuthenticator,
    pub caller_auth: CallerAuthenticator,
}

impl AppState {
    pub fn new(settings: Settings, pool: Option<PgPool>) -> Result<Self, AppError> {
        let http = Client::builder()
            .user_agent("moira/0.1")
            .build()
            .map_err(AppError::from)?;
        let cipher = LocalSecretCipher::new(
            settings.secrets.master_key_bytes()?,
            settings.secrets.key_id.clone(),
        );
        let key_hasher = ApiKeyHasher::new(
            settings.api_keys.pepper_bytes()?,
            settings.api_keys.pepper_version.clone(),
            settings.api_keys.prefix_length,
        );
        let idempotency_hasher = IdempotencyHasher::new(
            settings.idempotency.pepper_bytes()?,
            settings.idempotency.pepper_version.clone(),
        );
        // One JWKS cache, constructed once and cloned into all three authentication
        // paths, so the trusted-issuer path and both static authenticators share a
        // single SSRF/singleflight/stale-retention posture instead of drifting apart.
        let jwks_cache = JwksCache::new(settings.auth.jwks.clone());
        let auth = AuthService::new(
            settings.auth.admin.clone(),
            settings.auth.caller.clone(),
            http.clone(),
            key_hasher.clone(),
            jwks_cache.clone(),
        );
        let authz = AuthorizationService::new();
        let redis = RedisClient::from_settings(&settings.redis)?;
        let metrics = MetricsRegistry::new();
        let workers = WorkerRegistry::new(settings.workers.clone());
        let runtime_cache = RuntimeConfigCache::new(settings.cache.runtime_config_ttl_seconds);
        let runtime_handles = ProviderRuntimeCache::new(
            settings.runtime.runtime_cache_ttl_seconds,
            settings.runtime.runtime_cache_max_entries,
        );
        let concurrency = ConcurrencyController::new(
            settings.runtime.global_execution_concurrency,
            settings.runtime.application_execution_concurrency,
            settings.runtime.external_user_execution_concurrency,
            settings.runtime.runtime_cache_max_entries,
        );
        let public_rate_limiter =
            InMemoryRateLimiter::new(settings.public_api.rate_limiter_max_entries);
        let circuits = CircuitBreakerRegistry::new();
        let admin_auth = AdminAuthenticator::new(
            settings.auth.admin.clone(),
            http.clone(),
            jwks_cache.clone(),
            pool.clone(),
        );
        let caller_auth = CallerAuthenticator::new(
            settings.auth.caller.clone(),
            http.clone(),
            jwks_cache,
            pool.clone(),
        );

        Ok(Self {
            settings: Arc::new(settings),
            pool,
            http,
            cipher,
            key_hasher,
            idempotency_hasher,
            auth,
            authz,
            redis,
            metrics,
            workers,
            runtime_cache,
            runtime_handles,
            concurrency,
            public_rate_limiter,
            circuits,
            admin_auth,
            caller_auth,
        })
    }

    pub fn pool(&self) -> Result<&PgPool, AppError> {
        self.pool.as_ref().ok_or(AppError::DatabaseUnavailable)
    }
}
