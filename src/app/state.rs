use std::{sync::Arc, time::Duration};

use reqwest::Client;
use sqlx::PgPool;

use crate::{
    app::cluster_lease::ClusterLeaseStatus,
    config::{ContentEncryptionCustody, Settings},
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
        ContentKeyring, EnvironmentMasterKeyCustody, IdempotencyHasher, JwksCache,
        LocalSecretCipher, MasterKeyCustody,
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
    /// Custody of the master key that wraps content data keys.
    ///
    /// Constructed and **preflighted** in [`AppState::new`], which is why that function is
    /// `async`. Nothing reads or writes an `*_encrypted` column yet; the seam and its boot
    /// validation ship one full release ahead of any behaviour change so operators can set
    /// `MOIRA_CONTENT_ENCRYPTION__KEYS` before the release that requires it.
    ///
    /// Behind an `Arc<dyn _>` from the first commit rather than as a concrete type: the whole
    /// point of the seam is that swapping in AWS KMS or Vault Transit later touches this line
    /// and nothing else.
    pub content_custody: Arc<dyn MasterKeyCustody>,
    /// The content keyring, loaded and fully unwrapped before the listener binds.
    ///
    /// `None` only when there is no database at all — the health-and-metrics-only mode
    /// [`AppState::pool`] already encodes. There is no keyring to load without one, and there
    /// is nothing that could read a sealed column either, so the absence is the same absence.
    /// It is emphatically **not** a lenient fallback: with a pool present, a keyring that
    /// cannot be fully opened aborts `AppState::new`.
    ///
    /// Behind an `Arc` because the background refresh task holds one too.
    pub content_keyring: Option<Arc<ContentKeyring>>,
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
    /// `async` because of [`AppState::content_custody`]'s preflight and
    /// [`AppState::content_keyring`]'s load, and for no other reason.
    ///
    /// It is awaited here — before `main` binds a listener — rather than lazily on first use,
    /// on the same reasoning as the console's Postgres secret store: a bad key must fail at
    /// construction, not on some user's first read. `MasterKeyCustody::wrap` is `async` from
    /// day one because a KMS or Vault backend does network I/O, so making this synchronous now
    /// and asynchronous later would break every caller at exactly the moment the backend swap
    /// is supposed to be cheap.
    pub async fn new(settings: Settings, pool: Option<PgPool>) -> Result<Self, AppError> {
        let http = Client::builder()
            .user_agent("moira/0.1")
            .build()
            .map_err(AppError::from)?;
        let cipher = LocalSecretCipher::new(
            settings.secrets.master_key_bytes()?,
            settings.secrets.key_id.clone(),
        );
        let content_custody = build_content_custody(&settings).await?;
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
        // Boot steps 3 to 5 of `docs/decision-encryption-at-rest.md` §10, before the listener
        // binds and before any worker starts. A keyring this process cannot fully open aborts
        // here, with a message naming both remedies; see `ContentKeyring::load`.
        let content_keyring = match pool.clone() {
            Some(pool) => Some(Arc::new(
                ContentKeyring::load(
                    pool,
                    content_custody.clone(),
                    &settings.content_encryption,
                    metrics.clone(),
                )
                .await?,
            )),
            None => None,
        };
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
            content_custody,
            content_keyring,
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

/// Builds the configured custody backend and proves it is *usable*, not merely configured.
///
/// The `match` is the seam: adding `Kms` or `Vault` adds an arm here and changes nothing else.
async fn build_content_custody(settings: &Settings) -> Result<Arc<dyn MasterKeyCustody>, AppError> {
    let (keys, active_key_id) = settings.content_encryption.master_keys()?;
    let custody: Arc<dyn MasterKeyCustody> = match settings.content_encryption.custody {
        ContentEncryptionCustody::Environment => Arc::new(
            EnvironmentMasterKeyCustody::new(keys, active_key_id).map_err(|err| {
                AppError::Config(format!("content encryption key custody: {err}"))
            })?,
        ),
    };

    preflighted(custody).await
}

/// Runs the boot probe and turns a failure into a refusal to start.
///
/// Separate from [`build_content_custody`] so the failure path can be exercised with a backend
/// that *can* fail it: the environment backend cannot, because a 32-byte AES key that decoded
/// and loaded always round-trips. A KMS or Vault backend fails here routinely — wrong IAM
/// policy, unreachable endpoint, disabled key — which is the case this refusal exists for.
async fn preflighted(
    custody: Arc<dyn MasterKeyCustody>,
) -> Result<Arc<dyn MasterKeyCustody>, AppError> {
    custody.preflight().await.map_err(|err| {
        AppError::Config(format!(
            "content encryption key custody preflight failed for backend {:?} and active key \
             {:?}: {err}",
            custody.backend_name(),
            custody.active_master_key_id()
        ))
    })?;
    Ok(custody)
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use zeroize::Zeroizing;

    use super::*;
    use crate::security::{KeyCustodyError, WrappedKey};

    #[tokio::test]
    async fn boot_builds_a_usable_content_custody() {
        let state = AppState::new(Settings::default(), None).await.unwrap();

        assert_eq!(state.content_custody.backend_name(), "environment");
        assert_eq!(state.content_custody.active_master_key_id(), "dev-local");

        // "Usable", not merely "constructed" — the same property `preflight` asserts, checked
        // here through the handle the rest of the process will actually hold.
        let dek = Zeroizing::new([7u8; 32]);
        let wrapped = state.content_custody.wrap(&dek, b"probe").await.unwrap();
        assert_eq!(
            state
                .content_custody
                .unwrap(&wrapped, b"probe")
                .await
                .unwrap()
                .as_slice(),
            dek.as_slice()
        );
    }

    /// The break this release owns: no key, no boot. Asserted at `AppState::new`, which runs
    /// before `main` binds a listener, and asserted on the message text because that string is
    /// the only instruction the operator gets.
    #[tokio::test]
    async fn boot_fails_when_the_content_encryption_key_is_missing() {
        let mut settings = Settings::default();
        settings.content_encryption.allow_insecure_dev_key = false;
        // Set, and deliberately unused: there is no implicit fallback from the content keyring
        // to the provider-credential master key.
        settings.secrets.master_key_base64 = Some(STANDARD.encode([42; 32]));

        let error = match AppState::new(settings, None).await {
            Ok(_) => panic!("a missing content encryption key must abort startup"),
            Err(err) => err.to_string(),
        };
        assert!(
            error.contains("MOIRA_CONTENT_ENCRYPTION__KEYS must be set"),
            "{error}"
        );
    }

    #[derive(Debug)]
    struct UnusableCustody;

    #[async_trait::async_trait]
    impl MasterKeyCustody for UnusableCustody {
        fn backend_name(&self) -> &'static str {
            "aws_kms"
        }
        fn active_master_key_id(&self) -> &str {
            "arn-alias-moira"
        }
        fn can_unwrap(&self, _master_key_id: &str) -> bool {
            true
        }
        fn master_key_ids(&self) -> Vec<String> {
            vec!["arn-alias-moira".to_string()]
        }
        async fn wrap(
            &self,
            _dek: &Zeroizing<[u8; 32]>,
            _aad: &[u8],
        ) -> Result<WrappedKey, KeyCustodyError> {
            Err(KeyCustodyError::Unavailable { backend: "aws_kms" })
        }
        async fn unwrap(
            &self,
            _wrapped: &WrappedKey,
            _aad: &[u8],
        ) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
            Err(KeyCustodyError::Unavailable { backend: "aws_kms" })
        }
        async fn preflight(&self) -> Result<(), KeyCustodyError> {
            Err(KeyCustodyError::Unavailable { backend: "aws_kms" })
        }
    }

    /// A backend that is *configured* but not *usable* must abort startup, and the refusal must
    /// name the backend and the active key id — the two facts that tell an operator whether to
    /// look at their key policy or at their key list.
    #[tokio::test]
    async fn a_failing_preflight_aborts_startup() {
        let error = preflighted(Arc::new(UnusableCustody))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("preflight failed"), "{error}");
        assert!(error.contains("aws_kms"), "{error}");
        assert!(error.contains("arn-alias-moira"), "{error}");
    }
}
