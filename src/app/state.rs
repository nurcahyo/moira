use std::{sync::Arc, time::Duration};

use reqwest::Client;
use sqlx::PgPool;

use crate::{
    app::cluster_lease::ClusterLeaseStatus,
    config::Settings,
    error::AppError,
    infra::{
        coordination::RedisCoordinator, metrics::MetricsRegistry, redis::RedisClient,
        workers::WorkerRegistry,
    },
    orchestration::{
        AuthProviderSettingsCache, CircuitBreakerRegistry, ClusterCoordinator, ClusterRateLimiter,
        ConcurrencyController, InMemoryRateLimiter, ProviderRuntimeCache, RateLimiterBackend,
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
    /// The general-purpose outbound client: provider execution calls and anything else
    /// that is *not* a JWKS fetch.
    ///
    /// Deliberately left on `reqwest`'s defaults — including `Policy::limited(10)` —
    /// because changing redirect or timeout behaviour here would silently alter provider
    /// execution semantics, which plan 03 explicitly excludes. JWKS fetches do **not**
    /// use this client; they use the purpose-built one inside [`JwksCache`], which
    /// refuses redirects outright. See `src/security/ssrf.rs`.
    pub http: Client,
    pub cipher: LocalSecretCipher,
    pub key_hasher: ApiKeyHasher,
    pub idempotency_hasher: IdempotencyHasher,
    pub auth: AuthService,
    pub authz: AuthorizationService,
    pub redis: Option<RedisClient>,
    pub metrics: MetricsRegistry,
    /// This replica's cluster-admission-lease state (plan 10, P3-2).
    ///
    /// Constructed here as `not_enforced` and handed to the startup gate in
    /// `src/main.rs`, which flips it once a lease is granted. It lives on
    /// `AppState` rather than inside the lease handle because the thing that has
    /// to read it — `/health/ready` — is on the request path and the thing that
    /// writes it is a background heartbeat, exactly like the runtime caches
    /// above.
    pub cluster_lease: ClusterLeaseStatus,
    pub workers: WorkerRegistry,
    pub runtime_cache: RuntimeConfigCache,
    /// The enabled auth methods behind `GET /api/v1/admin/setup/auth-methods` (plan 07,
    /// module 13a).
    ///
    /// Held on `AppState` alongside the other runtime caches rather than inside the
    /// service, because the thing that invalidates it — the `moira_runtime_config`
    /// listener — lives outside the request path and needs a handle to it. That is also
    /// what makes an auth-settings write on one instance visible on every other one.
    pub auth_settings_cache: AuthProviderSettingsCache,
    pub runtime_handles: ProviderRuntimeCache,
    pub concurrency: ConcurrencyController,
    pub public_rate_limiter: RateLimiterBackend,
    /// **Per-process, deliberately, whether or not Redis is enabled.**
    ///
    /// Breaker state is *earned* by a replica observing its own transport
    /// failures. Sharing it would let one replica behind a bad network path open
    /// the circuit for replicas whose path to the same provider is healthy —
    /// converting a local fault into a cluster-wide one. Plan 10 §0.4b makes this
    /// explicit; there is no Redis breaker backend and adding one would be a
    /// regression, not a feature.
    pub circuits: CircuitBreakerRegistry,
    pub admin_auth: AdminAuthenticator,
    pub caller_auth: CallerAuthenticator,
    /// Shared with all three authentication paths. Exposed on `AppState` so the admin
    /// `refresh-jwks` command can reuse the same hardened client, validation, caps and
    /// cache rather than issuing a fetch of its own.
    pub jwks_cache: JwksCache,
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
        // paths plus the admin refresh command, so every JWKS fetch in the process
        // shares a single SSRF/redirect/singleflight/stale-retention posture instead of
        // drifting apart. The cache owns its own hardened `reqwest::Client`; `http`
        // above is never used for a JWKS fetch.
        let jwks_cache = JwksCache::new(settings.auth.jwks.clone())?;
        let auth = AuthService::new(
            settings.auth.admin.clone(),
            settings.auth.caller.clone(),
            key_hasher.clone(),
            jwks_cache.clone(),
        );
        let authz = AuthorizationService::new();
        let redis = RedisClient::from_settings(&settings.redis)?;
        let metrics = MetricsRegistry::new(&settings.telemetry.service_name, pool.clone());
        let cluster_lease = ClusterLeaseStatus::not_enforced();
        let workers = WorkerRegistry::new(
            settings.workers.clone(),
            // Resolved once, here: `Settings::leader_election_enabled` is the
            // only place `workers.leader_election_enabled`'s `None` is folded
            // against `cluster.admission_enabled`.
            settings.leader_election_enabled(),
        );
        let runtime_cache = RuntimeConfigCache::new(settings.cache.runtime_config_ttl_seconds);
        let auth_settings_cache =
            AuthProviderSettingsCache::new(settings.auth.provider_settings_cache_ttl_seconds);
        let runtime_handles = ProviderRuntimeCache::new(
            settings.runtime.runtime_cache_ttl_seconds,
            settings.runtime.runtime_cache_max_entries,
        );
        // The one place the coordination backend is chosen, and it is chosen from
        // one condition: whether a Redis client exists. `RedisClient::from_settings`
        // already returned `None` for the default `redis.enabled = false`, so the
        // absent case needs no second flag to agree with — there is nothing for a
        // second flag to disagree with it about.
        //
        // Rate limiting and concurrency are the *only* two controls this affects.
        // They are also the only two that are not already cluster-correct: the
        // admission lease, leader election, idempotency and runtime-config
        // invalidation all coordinate through Postgres regardless.
        let coordinator: Option<Arc<dyn ClusterCoordinator>> = redis
            .clone()
            .map(|redis| Arc::new(RedisCoordinator::new(redis, metrics.clone())) as Arc<_>);

        let concurrency = ConcurrencyController::new(
            settings.runtime.global_execution_concurrency,
            settings.runtime.application_execution_concurrency,
            settings.runtime.external_user_execution_concurrency,
            settings.runtime.runtime_cache_max_entries,
        );
        let concurrency = match coordinator.as_ref() {
            Some(coordinator) => concurrency.with_cluster(
                coordinator.clone(),
                Duration::from_secs(settings.redis.permit_ttl_seconds.max(1)),
            ),
            None => concurrency,
        };
        let public_rate_limiter = match coordinator.as_ref() {
            Some(coordinator) => {
                RateLimiterBackend::Cluster(ClusterRateLimiter::new(coordinator.clone()))
            }
            None => RateLimiterBackend::InMemory(InMemoryRateLimiter::new(
                settings.public_api.rate_limiter_max_entries,
            )),
        };
        let circuits = CircuitBreakerRegistry::new();
        let admin_auth = AdminAuthenticator::new(
            settings.auth.admin.clone(),
            jwks_cache.clone(),
            pool.clone(),
        );
        let caller_auth = CallerAuthenticator::new(
            settings.auth.caller.clone(),
            jwks_cache.clone(),
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
            cluster_lease,
            workers,
            runtime_cache,
            auth_settings_cache,
            runtime_handles,
            concurrency,
            public_rate_limiter,
            circuits,
            admin_auth,
            caller_auth,
            jwks_cache,
        })
    }

    pub fn pool(&self) -> Result<&PgPool, AppError> {
        self.pool.as_ref().ok_or(AppError::DatabaseUnavailable)
    }
}
