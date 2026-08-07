use std::{
    collections::HashMap,
    future::Future,
    hash::{Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::warn;
use uuid::Uuid;

use crate::{
    domain::{ExecutionFailure, ExecutionFailureClass, ProviderRuntimePolicyRecord},
    error::AppError,
};

use super::runtime_factory::RuntimeModelHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCacheKey {
    pub provider_id: Uuid,
    pub provider_version: i64,
    pub model_id: Uuid,
    pub model_version: i64,
    pub credential_id: Uuid,
    pub credential_version: i64,
    pub runtime_policy_version: i64,
}

impl Hash for RuntimeCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.provider_id.hash(state);
        self.provider_version.hash(state);
        self.model_id.hash(state);
        self.model_version.hash(state);
        self.credential_id.hash(state);
        self.credential_version.hash(state);
        self.runtime_policy_version.hash(state);
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRuntimeCache {
    ttl: Duration,
    max_entries: usize,
    entries: Arc<Mutex<HashMap<RuntimeCacheKey, RuntimeCacheEntry>>>,
    build_locks: Arc<Mutex<HashMap<RuntimeCacheKey, Arc<Mutex<()>>>>>,
}

#[derive(Debug, Clone)]
struct RuntimeCacheEntry {
    handle: Arc<RuntimeModelHandle>,
    expires_at: Instant,
    inserted_at: Instant,
}

impl ProviderRuntimeCache {
    pub fn new(ttl_seconds: u64, max_entries: usize) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_seconds),
            max_entries: max_entries.max(1),
            entries: Arc::new(Mutex::new(HashMap::new())),
            build_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get_or_insert_with<F, Fut>(
        &self,
        key: RuntimeCacheKey,
        build: F,
    ) -> Result<Arc<RuntimeModelHandle>, AppError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<RuntimeModelHandle, AppError>>,
    {
        if let Some(handle) = self.get(&key).await {
            return Ok(handle);
        }

        let build_lock = {
            let mut locks = self.build_locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = build_lock.lock().await;
        if let Some(handle) = self.get(&key).await {
            self.build_locks.lock().await.remove(&key);
            return Ok(handle);
        }

        let handle = match build().await {
            Ok(handle) => handle,
            Err(error) => {
                self.build_locks.lock().await.remove(&key);
                return Err(error);
            }
        };
        let handle = self.insert(key.clone(), handle).await;
        self.build_locks.lock().await.remove(&key);
        Ok(handle)
    }

    pub async fn get(&self, key: &RuntimeCacheKey) -> Option<Arc<RuntimeModelHandle>> {
        let now = Instant::now();
        let entries = self.entries.lock().await;
        entries
            .get(key)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| entry.handle.clone())
    }

    pub async fn insert(
        &self,
        key: RuntimeCacheKey,
        handle: RuntimeModelHandle,
    ) -> Arc<RuntimeModelHandle> {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        if entries.len() >= self.max_entries
            && let Some(oldest_key) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest_key);
        }
        let handle = Arc::new(handle);
        entries.insert(
            key,
            RuntimeCacheEntry {
                handle: handle.clone(),
                expires_at: now + self.ttl,
                inserted_at: now,
            },
        );
        handle
    }

    pub async fn invalidate_all(&self) {
        self.entries.lock().await.clear();
    }
}

/// Cluster-wide coordination primitives, as the control layer needs them.
///
/// # Why a trait and not `RedisClient` directly
///
/// Three reasons, in order of how much they matter.
///
/// 1. **Layering.** `src/infra/db.rs` already depends on this module; a reverse
///    import from `orchestration` into `infra` would close that loop. The trait
///    keeps every arrow pointing the same way — `infra` implements what
///    `orchestration` declares.
/// 2. **The default arm is testable without Redis.** Plan 10 §0.4b requires a
///    test that the *shipped* (Redis-off) build coordinates correctly, and that a
///    Redis code path fails closed. Both are pure logic once the transport is a
///    trait: a fake that always errors proves fail-closed with no socket, no
///    container and no timing.
/// 3. Redis is not load-bearing to the design. It is one implementation of
///    "somewhere all replicas can count together".
///
/// Note what is **not** here: no circuit-breaker state. Breaker health is earned
/// by a replica observing its own transport failures, and sharing it would let
/// one replica's bad network path open the circuit for healthy replicas. Breakers
/// stay per-process even when this trait has an implementation behind it.
#[async_trait]
pub trait ClusterCoordinator: Send + Sync + std::fmt::Debug {
    /// One fixed-window rate-limit check. `Ok(true)` admits.
    async fn check_rate_window(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<bool, AppError>;

    /// Takes one cluster-wide concurrency slot. `Ok(true)` acquired.
    async fn try_acquire_permit(
        &self,
        key: &str,
        limit: usize,
        ttl: Duration,
    ) -> Result<bool, AppError>;

    /// Returns one cluster-wide concurrency slot. Best-effort by construction —
    /// see [`ClusterPermitGuard`].
    async fn release_permit(&self, key: &str) -> Result<(), AppError>;

    /// Namespaces a key suffix.
    fn key(&self, suffix: &str) -> String;
}

#[derive(Debug, Clone)]
pub struct ConcurrencyController {
    global: Arc<Semaphore>,
    /// The semaphore's configured size, stored because `tokio` exposes only the
    /// *remaining* permit count and the cluster counter needs the ceiling.
    global_limit: usize,
    application_limit: usize,
    user_limit: usize,
    max_dynamic_limiters: usize,
    providers: Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
    provider_streams: Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
    applications: Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
    users: Arc<Mutex<HashMap<String, LimiterEntry>>>,
    /// Present only when a coordinator is configured.
    ///
    /// The cluster layer sits **on top of** the per-process one rather than
    /// replacing it: every replica still takes its local permit first, and only
    /// then asks the cluster. That ordering is deliberate. The local check is a
    /// compare-and-swap on an atomic; the cluster check is a network round trip.
    /// Rejecting locally first means a replica already at its own ceiling never
    /// pays for a round trip to be told the same thing, and — more importantly —
    /// never takes a cluster slot it is about to hand back.
    ///
    /// The two ceilings are the same number, so the local one is never the
    /// binding constraint in a multi-replica deployment; it is the binding
    /// constraint in a single-replica one, which is exactly today's behaviour.
    cluster: Option<ClusterConcurrency>,
}

#[derive(Clone)]
struct ClusterConcurrency {
    coordinator: Arc<dyn ClusterCoordinator>,
    ttl: Duration,
}

impl std::fmt::Debug for ClusterConcurrency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterConcurrency")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

/// A held cluster-wide concurrency slot.
///
/// # Release is best-effort, and the TTL is what makes that acceptable
///
/// `Drop` cannot be `async`, so the decrement is handed to a detached task. Three
/// things can therefore lose a release: no runtime is running at drop time, the
/// spawned task is cancelled with the runtime, or the process dies outright. Each
/// leaks one slot.
///
/// That is why every cluster counter carries a TTL refreshed on acquire (see
/// `RedisSettings::permit_ttl_seconds`): a leaked slot is reclaimed one TTL after
/// the last acquire against that key. The in-memory `DynamicPermit` needs no such
/// net because process death frees the `Arc` naturally — this is the one place
/// where the cluster backend is genuinely weaker than the local one, and the TTL
/// is the bound on how weak.
pub struct ClusterPermitGuard {
    coordinator: Arc<dyn ClusterCoordinator>,
    key: String,
}

impl std::fmt::Debug for ClusterPermitGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterPermitGuard")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for ClusterPermitGuard {
    fn drop(&mut self) {
        let coordinator = self.coordinator.clone();
        let key = std::mem::take(&mut self.key);
        // `try_current` rather than `Handle::current`: the latter panics when no
        // runtime is running, and a panic inside `Drop` during unwinding aborts the
        // process. A dropped guard outside a runtime is a leaked slot the TTL
        // reclaims; an abort is an outage.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("cluster permit released outside a runtime; the slot is reclaimed by its TTL");
            return;
        };
        handle.spawn(async move {
            if let Err(error) = coordinator.release_permit(&key).await {
                // Not escalated: the TTL bounds the leak, and a failing release
                // during shutdown is the common case.
                warn!(%error, "cluster permit release failed; the slot is reclaimed by its TTL");
            }
        });
    }
}

#[derive(Debug, Clone)]
struct LimiterEntry {
    limiter: Arc<DynamicLimiter>,
    last_used: Instant,
}

#[derive(Debug)]
struct DynamicLimiter {
    limit: AtomicUsize,
    active: AtomicUsize,
}

#[derive(Debug)]
struct DynamicPermit {
    limiter: Arc<DynamicLimiter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityScope {
    Global,
    ProviderRequest,
    ProviderStream,
    Application,
    User,
    LimiterRegistry,
}

#[derive(Debug)]
pub struct CapacityExhaustion {
    scope: CapacityScope,
}

#[derive(Debug)]
pub struct ExecutionPermits {
    _global: OwnedSemaphorePermit,
    _provider_request: DynamicPermit,
    _provider_stream: Option<DynamicPermit>,
    _application: Option<DynamicPermit>,
    _user: Option<DynamicPermit>,
    /// Empty unless a [`ClusterCoordinator`] is configured. Declared last so it
    /// is dropped last, which keeps the cluster counter occupied for at least as
    /// long as the local one — never the other way round, which would open a
    /// window where the cluster believes there is room and no replica does.
    _cluster: Vec<ClusterPermitGuard>,
}

#[derive(Debug, Clone)]
pub struct InMemoryRateLimiter {
    max_entries: usize,
    buckets: Arc<Mutex<HashMap<String, RateLimitBucket>>>,
}

#[derive(Debug, Clone)]
struct RateLimitBucket {
    window_started: Instant,
    count: u32,
}

impl ConcurrencyController {
    pub fn new(
        global_limit: usize,
        application_limit: usize,
        user_limit: usize,
        max_dynamic_limiters: usize,
    ) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global_limit.max(1))),
            global_limit: global_limit.max(1),
            application_limit: application_limit.max(1),
            user_limit: user_limit.max(1),
            max_dynamic_limiters: max_dynamic_limiters.max(1),
            providers: Arc::new(Mutex::new(HashMap::new())),
            provider_streams: Arc::new(Mutex::new(HashMap::new())),
            applications: Arc::new(Mutex::new(HashMap::new())),
            users: Arc::new(Mutex::new(HashMap::new())),
            cluster: None,
        }
    }

    /// Adds the cluster-wide layer. Called once, from `AppState::new`, and only
    /// when a coordinator is configured.
    ///
    /// Consuming rather than mutating: the controller is `Clone` and handed to
    /// every request path, so a setter would let one clone acquire against a
    /// coordinator another clone does not have.
    #[must_use]
    pub fn with_cluster(mut self, coordinator: Arc<dyn ClusterCoordinator>, ttl: Duration) -> Self {
        self.cluster = Some(ClusterConcurrency { coordinator, ttl });
        self
    }

    /// Whether cluster-wide permits are in force.
    ///
    /// `false` is the shipped default and means every ceiling below is enforced
    /// **per replica**: N replicas admit up to N times the configured limit. The
    /// cluster-admission lease is what bounds N and makes that trade tolerable.
    pub fn is_cluster_wide(&self) -> bool {
        self.cluster.is_some()
    }

    /// The **only** admission entry point. `is_stream` is a required argument and has no
    /// default, deliberately.
    ///
    /// F43 — there used to be a second, `pub` one called `acquire`, which took four arguments
    /// and supplied `is_stream: false` and `provider_stream_limit = provider_limit` itself.
    /// Every one of its 29 call sites was test code (20 in this file's `#[cfg(test)]` modules,
    /// 9 across `tests/cluster_coordination.rs` and `tests/coordination_default_path.rs`), so
    /// the hardcoded `false` was never wrong in practice — but it was the shorter name, the
    /// obvious name, and the one a future streaming caller would reach for, and reaching for it
    /// would silently take a **request** permit and leave `max_concurrent_streams` unenforced.
    ///
    /// The fix is not "delete the dead code": the 9 integration-test call sites are a separate
    /// crate and genuinely need a `pub` entry, so deleting it was never available. What was
    /// available was removing the *choice*. There is now one function, it cannot be called
    /// without stating which ceiling the caller wants, and `CapacityExhaustion`/`CapacityScope`
    /// are `pub` so a caller can tell the two apart.
    ///
    /// The one production call site is `ExecutionService::execute_attempt`, which passes
    /// `command.options.stream`. That *wiring* — not this predicate — is the thing worth
    /// guarding, because a correct predicate with wrong wiring is this repository's most
    /// repeated defect shape, and it is already guarded:
    /// `stream_capacity_is_independent_from_request_capacity` in `tests/execution_lifecycle.rs`
    /// runs `max_concurrent_requests: 2` against `max_concurrent_streams: 1` — two *distinct*
    /// numbers, so the two ceilings stay distinguishable — holds one stream open, proves a
    /// non-streaming execution still passes, and then requires a second stream to be refused
    /// with `CapacityExhausted` and `call_count` unchanged. Passing `false` here instead of
    /// `command.options.stream` reds it. Verified by running that edit, not by reading.
    ///
    /// **What does not stop this coming back:** nothing prevents a new four-argument
    /// convenience wrapper being added above. There is no dead-code lint that would catch one
    /// (it would have callers in tests immediately, which is exactly how the old one survived).
    /// What changed is that the obvious name is taken by the function that demands the answer.
    pub async fn acquire(
        &self,
        provider_id: Uuid,
        provider_request_limit: usize,
        is_stream: bool,
        provider_stream_limit: usize,
        application_id: Option<Uuid>,
        external_user_id: Option<&str>,
    ) -> Result<ExecutionPermits, CapacityExhaustion> {
        // Accumulated as we go and returned inside `ExecutionPermits` on success.
        // On any early return it drops here, which releases every cluster slot
        // taken so far — the guard's `Drop` is the whole unwind path, so there is
        // no manual cleanup to forget.
        let mut cluster = Vec::new();

        let global = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| CapacityExhaustion::new(CapacityScope::Global))?;
        self.acquire_cluster_slot(
            &mut cluster,
            "permit:global",
            self.global_limit,
            CapacityScope::Global,
        )
        .await?;

        let provider_request = self
            .acquire_uuid_limiter(
                &self.providers,
                provider_id,
                provider_request_limit,
                CapacityScope::ProviderRequest,
            )
            .await?;
        self.acquire_cluster_slot(
            &mut cluster,
            &format!("permit:provider:{provider_id}"),
            provider_request_limit,
            CapacityScope::ProviderRequest,
        )
        .await?;

        let provider_stream = if is_stream {
            let permit = self
                .acquire_uuid_limiter(
                    &self.provider_streams,
                    provider_id,
                    provider_stream_limit,
                    CapacityScope::ProviderStream,
                )
                .await?;
            self.acquire_cluster_slot(
                &mut cluster,
                &format!("permit:provider-stream:{provider_id}"),
                provider_stream_limit,
                CapacityScope::ProviderStream,
            )
            .await?;
            Some(permit)
        } else {
            None
        };

        let application = match application_id {
            Some(id) => {
                let permit = self
                    .acquire_uuid_limiter(
                        &self.applications,
                        id,
                        self.application_limit,
                        CapacityScope::Application,
                    )
                    .await?;
                self.acquire_cluster_slot(
                    &mut cluster,
                    &format!("permit:application:{id}"),
                    self.application_limit,
                    CapacityScope::Application,
                )
                .await?;
                Some(permit)
            }
            None => None,
        };

        let user = match external_user_id.filter(|value| !value.is_empty()) {
            Some(user_id) => {
                let permit = self
                    .acquire_string_limiter(
                        &self.users,
                        user_id,
                        self.user_limit,
                        CapacityScope::User,
                    )
                    .await?;
                self.acquire_cluster_slot(
                    &mut cluster,
                    &format!("permit:user:{}", hashed_key_segment(user_id)),
                    self.user_limit,
                    CapacityScope::User,
                )
                .await?;
                Some(permit)
            }
            None => None,
        };

        Ok(ExecutionPermits {
            _global: global,
            _provider_request: provider_request,
            _provider_stream: provider_stream,
            _application: application,
            _user: user,
            _cluster: cluster,
        })
    }

    /// Takes one cluster-wide slot, or returns the same [`CapacityExhaustion`] the
    /// local limiter would have.
    ///
    /// **Fails closed.** A coordinator error — unreachable, timed out, a script
    /// error — is treated as "no capacity", not as "unlimited capacity". Serving
    /// unbounded traffic because the thing that counts it is down is the exact
    /// failure this layer exists to prevent, and it would be silent.
    async fn acquire_cluster_slot(
        &self,
        held: &mut Vec<ClusterPermitGuard>,
        suffix: &str,
        limit: usize,
        scope: CapacityScope,
    ) -> Result<(), CapacityExhaustion> {
        let Some(cluster) = self.cluster.as_ref() else {
            return Ok(());
        };
        let key = cluster.coordinator.key(suffix);
        match cluster
            .coordinator
            .try_acquire_permit(&key, limit, cluster.ttl)
            .await
        {
            Ok(true) => {
                held.push(ClusterPermitGuard {
                    coordinator: cluster.coordinator.clone(),
                    key,
                });
                Ok(())
            }
            Ok(false) => Err(CapacityExhaustion::new(scope)),
            Err(error) => {
                warn!(
                    %error,
                    ?scope,
                    "cluster concurrency coordinator failed; refusing the request"
                );
                Err(CapacityExhaustion::new(scope))
            }
        }
    }

    async fn acquire_uuid_limiter(
        &self,
        map: &Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
        key: Uuid,
        limit: usize,
        scope: CapacityScope,
    ) -> Result<DynamicPermit, CapacityExhaustion> {
        let mut map = map.lock().await;
        acquire_dynamic_limiter(&mut map, key, limit, scope, self.max_dynamic_limiters)
    }

    async fn acquire_string_limiter(
        &self,
        map: &Arc<Mutex<HashMap<String, LimiterEntry>>>,
        key: &str,
        limit: usize,
        scope: CapacityScope,
    ) -> Result<DynamicPermit, CapacityExhaustion> {
        let mut map = map.lock().await;
        acquire_dynamic_limiter(
            &mut map,
            key.to_string(),
            limit,
            scope,
            self.max_dynamic_limiters,
        )
    }
}

impl DynamicLimiter {
    fn new(limit: usize) -> Self {
        Self {
            limit: AtomicUsize::new(limit.max(1)),
            active: AtomicUsize::new(0),
        }
    }

    fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Release);
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    fn try_acquire(self: &Arc<Self>) -> Option<DynamicPermit> {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.limit.load(Ordering::Acquire) {
                return None;
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(DynamicPermit {
                        limiter: self.clone(),
                    });
                }
                Err(current) => active = current,
            }
        }
    }
}

impl Drop for DynamicPermit {
    fn drop(&mut self) {
        let previous = self.limiter.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "dynamic limiter permit count underflowed");
    }
}

impl CapacityExhaustion {
    fn new(scope: CapacityScope) -> Self {
        Self { scope }
    }

    pub fn scope(&self) -> CapacityScope {
        self.scope
    }
}

impl From<CapacityExhaustion> for ExecutionFailure {
    fn from(exhaustion: CapacityExhaustion) -> Self {
        let provider_specific = matches!(
            exhaustion.scope(),
            CapacityScope::ProviderRequest | CapacityScope::ProviderStream
        );
        let message = match exhaustion.scope() {
            CapacityScope::Global => "global execution capacity is exhausted",
            CapacityScope::ProviderRequest => "provider request capacity is exhausted",
            CapacityScope::ProviderStream => "provider stream capacity is exhausted",
            CapacityScope::Application => "application execution capacity is exhausted",
            CapacityScope::User => "user execution capacity is exhausted",
            CapacityScope::LimiterRegistry => "dynamic limiter registry capacity is exhausted",
        };
        let mut failure = ExecutionFailure::new(ExecutionFailureClass::CapacityExhausted, message);
        failure.retryable = false;
        failure.fallback_eligible = provider_specific;
        failure
    }
}

fn acquire_dynamic_limiter<K>(
    map: &mut HashMap<K, LimiterEntry>,
    key: K,
    limit: usize,
    scope: CapacityScope,
    max: usize,
) -> Result<DynamicPermit, CapacityExhaustion>
where
    K: Eq + Hash + Clone,
{
    if !map.contains_key(&key) && map.len() >= max && !evict_oldest_inactive_limiter(map) {
        return Err(CapacityExhaustion::new(CapacityScope::LimiterRegistry));
    }

    let entry = map.entry(key).or_insert_with(|| LimiterEntry {
        limiter: Arc::new(DynamicLimiter::new(limit)),
        last_used: Instant::now(),
    });
    entry.last_used = Instant::now();
    entry.limiter.set_limit(limit);
    entry
        .limiter
        .try_acquire()
        .ok_or_else(|| CapacityExhaustion::new(scope))
}

impl InMemoryRateLimiter {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn check(&self, key: String, limit: u32, window: Duration) -> Result<(), AppError> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().await;
        if buckets.len() >= self.max_entries
            && !buckets.contains_key(&key)
            && let Some(oldest_key) = buckets
                .iter()
                .min_by_key(|(_, bucket)| bucket.window_started)
                .map(|(key, _)| key.clone())
        {
            buckets.remove(&oldest_key);
        }
        let bucket = buckets.entry(key).or_insert_with(|| RateLimitBucket {
            window_started: now,
            count: 0,
        });
        if now.duration_since(bucket.window_started) >= window {
            bucket.window_started = now;
            bucket.count = 0;
        }
        if bucket.count >= limit.max(1) {
            return Err(rate_limited());
        }
        bucket.count = bucket.count.saturating_add(1);
        Ok(())
    }
}

/// The rate-limit rejection, spelled once.
///
/// Both backends return this exact value, so the wire contract — `429`, code
/// `rate_limited`, and therefore the derived `moira.error.rate_limited` message
/// key — cannot drift between them. `rate_limit_rejection_is_identical_across_backends`
/// pins it.
fn rate_limited() -> AppError {
    AppError::coded(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "public execution rate limit exceeded",
    )
}

/// A caller-supplied identifier, reduced to something safe to put in a shared key.
///
/// `external_user_id` is caller input and may be an email, a customer reference,
/// anything. It must not be written verbatim into a Redis key: Redis is
/// operationally visible (`KEYS`, `MONITOR`, an RDB dump, a managed provider's
/// console) in a way process memory is not, and a key is a value.
///
/// A plain digest, not a keyed one. This is not an authentication boundary and
/// nothing is verified against it — the property needed is only that two distinct
/// users get distinct keys and that the key does not read back as the identifier.
/// The keyed, peppered hashing lives in `src/security` where it is checked.
fn hashed_key_segment(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    // Half the digest. 128 bits is far past any collision concern for a
    // per-deployment key space, and a shorter key is a smaller Redis footprint.
    digest[..16].iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The public rate limiter, in whichever backing the deployment configured.
///
/// An enum rather than a trait object because there are exactly two arms and the
/// choice is made once, at `AppState::new`. Callers see one `check` with the
/// signature `InMemoryRateLimiter::check` already had, so
/// `src/application/public.rs` is untouched by this split.
#[derive(Debug, Clone)]
pub enum RateLimiterBackend {
    /// The shipped default. Per-process, and therefore **per replica**: N replicas
    /// admit up to N times the configured limit. Correct for one replica, and the
    /// documented trade for more — see plan 10 §0.4b.
    InMemory(InMemoryRateLimiter),
    /// Cluster-wide. One window shared by every replica.
    Cluster(ClusterRateLimiter),
}

impl RateLimiterBackend {
    pub async fn check(&self, key: String, limit: u32, window: Duration) -> Result<(), AppError> {
        match self {
            Self::InMemory(limiter) => limiter.check(key, limit, window).await,
            Self::Cluster(limiter) => limiter.check(key, limit, window).await,
        }
    }

    /// Whether the window is shared across replicas. `false` is the default.
    pub fn is_cluster_wide(&self) -> bool {
        matches!(self, Self::Cluster(_))
    }
}

/// A rate limiter whose window lives where every replica can see it.
#[derive(Clone)]
pub struct ClusterRateLimiter {
    coordinator: Arc<dyn ClusterCoordinator>,
}

impl std::fmt::Debug for ClusterRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterRateLimiter").finish_non_exhaustive()
    }
}

impl ClusterRateLimiter {
    pub fn new(coordinator: Arc<dyn ClusterCoordinator>) -> Self {
        Self { coordinator }
    }

    /// **Fails closed.** A coordinator that cannot answer produces the same `429`
    /// a full window does.
    ///
    /// The alternative — falling back to the in-process limiter — is worse than it
    /// looks: it silently reinstates the per-replica multiplier this backend exists
    /// to remove, at precisely the moment nobody is watching, and the only signal
    /// would be a log line. A `429` is wrong in a recoverable, visible direction;
    /// serving N times the configured rate is wrong in neither.
    pub async fn check(&self, key: String, limit: u32, window: Duration) -> Result<(), AppError> {
        let key = self.coordinator.key(&format!("ratelimit:{key}"));
        match self
            .coordinator
            .check_rate_window(&key, limit, window)
            .await
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(rate_limited()),
            Err(error) => {
                warn!(
                    %error,
                    "cluster rate-limit coordinator failed; refusing the request"
                );
                Err(rate_limited())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CircuitBreakerRegistry {
    states: Arc<Mutex<HashMap<(Uuid, Uuid), CircuitEntry>>>,
}

#[derive(Debug, Clone)]
struct CircuitEntry {
    state: CircuitState,
    failure_count: i32,
    opened_at: Option<Instant>,
    policy_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Which breaker entries a single `moira_runtime_config` notification may clear.
///
/// Breakers are keyed on `(provider_id, model_id)`, so a config change can only be
/// narrowed to one of these shapes. The registry deliberately does not know about
/// table names — `src/infra/db.rs` owns the payload-to-scope mapping, and this enum
/// is the contract between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitResetScope {
    /// Clear every entry belonging to one provider, whatever model it names.
    Provider(Uuid),
    /// Clear every entry naming one provider model, whatever provider owns it.
    Model(Uuid),
    /// The change cannot have altered provider health; leave every entry alone.
    Unaffected,
    /// Fail-safe: clear everything, exactly as `reset_all` does. Used when a
    /// notification cannot be understood well enough to narrow it.
    All,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self {
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn before_call(
        &self,
        provider_id: Uuid,
        model_id: Uuid,
        policy: &ProviderRuntimePolicyRecord,
    ) -> Result<CircuitState, ExecutionFailure> {
        let mut states = self.states.lock().await;
        let entry = states
            .entry((provider_id, model_id))
            .or_insert(CircuitEntry {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                policy_version: policy.version,
            });
        if entry.policy_version != policy.version {
            *entry = CircuitEntry {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                policy_version: policy.version,
            };
        }

        match entry.state {
            CircuitState::Closed | CircuitState::HalfOpen => Ok(entry.state),
            CircuitState::Open => {
                let open_for = Duration::from_millis(policy.circuit_open_duration_ms as u64);
                if entry
                    .opened_at
                    .is_some_and(|opened| opened.elapsed() >= open_for)
                {
                    entry.state = CircuitState::HalfOpen;
                    Ok(CircuitState::HalfOpen)
                } else {
                    Err(ExecutionFailure::new(
                        ExecutionFailureClass::CircuitOpen,
                        "provider circuit is open",
                    ))
                }
            }
        }
    }

    pub async fn on_success(&self, provider_id: Uuid, model_id: Uuid) {
        let mut states = self.states.lock().await;
        states.insert(
            (provider_id, model_id),
            CircuitEntry {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                policy_version: 0,
            },
        );
    }

    pub async fn on_failure(
        &self,
        provider_id: Uuid,
        model_id: Uuid,
        policy: &ProviderRuntimePolicyRecord,
        class: ExecutionFailureClass,
    ) {
        if !is_circuit_failure(class) {
            return;
        }
        let mut states = self.states.lock().await;
        let entry = states
            .entry((provider_id, model_id))
            .or_insert(CircuitEntry {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                policy_version: policy.version,
            });
        entry.failure_count += 1;
        entry.policy_version = policy.version;
        if entry.state == CircuitState::HalfOpen
            || entry.failure_count >= policy.circuit_failure_threshold
        {
            entry.state = CircuitState::Open;
            entry.opened_at = Some(Instant::now());
        }
    }

    /// Clears every breaker entry. Kept for callers that genuinely mean "everything"
    /// — process startup, and the fail-safe path in [`Self::reset_for_resource`].
    pub async fn reset_all(&self) {
        self.states.lock().await.clear();
    }

    /// Clears only the breaker entries a config change can plausibly have affected.
    ///
    /// The unconditional `reset_all` this replaces on the NOTIFY path discarded the
    /// health of every provider in the process whenever any one row changed, so a
    /// single unrelated write re-closed circuits that were open for good reason and
    /// sent traffic back at providers still failing.
    ///
    /// Narrowing is deliberately one-directional: the worst case of an incomplete
    /// mapping is a breaker that should have reset and did not, which self-heals on
    /// the next relevant notification or on `circuit_open_duration_ms`.
    pub async fn reset_for_resource(&self, scope: CircuitResetScope) {
        let mut states = self.states.lock().await;
        match scope {
            CircuitResetScope::Provider(provider_id) => {
                states.retain(|(entry_provider, _), _| *entry_provider != provider_id);
            }
            CircuitResetScope::Model(model_id) => {
                states.retain(|(_, entry_model), _| *entry_model != model_id);
            }
            CircuitResetScope::Unaffected => {}
            CircuitResetScope::All => states.clear(),
        }
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionFailure {
    pub fn new(class: ExecutionFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retryable: is_retryable(class),
            fallback_eligible: is_fallback_eligible(class),
        }
    }
}

/// Whether the same provider and model are worth trying again for this failure.
///
/// # `StructuredOutputInvalid` is deliberately absent — F29's first precondition
///
/// Membership here means "the next attempt has a materially different chance of succeeding
/// *without changing anything about the request*". That is true of a timeout, a 429 and a
/// connection reset; it is false of every structured-output failure, in both directions, and the
/// reasons differ per direction so they are recorded separately:
///
/// **Two of the three emitters are caller errors.** Both reject the *caller's schema* before the
/// model ever sees it — `validate_response_format` over `public_api.maximum_schema_bytes`, and
/// `build_completion_request` over a schema that is not a readable `schemars::Schema`. F42 and
/// issue #80 pin the emitter set at exactly three
/// (`structured_output_invalid_has_only_the_three_emitters_its_catalog_entry_describes`). A
/// schema that is too large or unreadable is exactly as large and unreadable on the second
/// attempt, so a retry is a guaranteed-identical failure paid for in latency.
///
/// **The third emitter is a model error — issue #80 shipped it, and the answer is still no.**
/// A reply that did not satisfy the schema *might* satisfy it on a second sample — but Moira
/// pins `temperature: Some(0.0)` on its own schema-carrying calls (memory extraction), where a
/// second sample is bit-identical; and where a caller did choose a non-zero temperature, a retry
/// is a coin flip charged to them. Worse, the retry budget is shared: spending an attempt on a
/// chatty model leaves fewer for the transport failure that arrives later in the same execution,
/// which is the failure retries exist for.
///
/// See [`is_fallback_eligible`] for why the *other* provider question gets the same answer for a
/// different reason, and [`is_circuit_failure`] for why counting this against a breaker would be
/// a caller-triggered denial of service. The disposition is pinned by
/// `structured_output_invalid_is_in_none_of_the_three_dispositions`.
pub fn is_retryable(class: ExecutionFailureClass) -> bool {
    matches!(
        class,
        ExecutionFailureClass::ProviderTimeout
            | ExecutionFailureClass::ProviderConnectionFailed
            | ExecutionFailureClass::ProviderRateLimited
            | ExecutionFailureClass::ProviderUnavailable
            | ExecutionFailureClass::ProviderUpstreamError
            | ExecutionFailureClass::CircuitOpen
            | ExecutionFailureClass::CapacityExhausted
    )
}

/// Whether a *different* provider is worth trying for this failure.
///
/// # `StructuredOutputInvalid` is deliberately absent — and this is the direction that had a real
/// case for inclusion
///
/// "Retry the same provider" and "try another one" are genuinely different questions, and the
/// argument for fallback here was the strongest one in the family: a provider that *structurally
/// cannot* send a schema will never comply, while the next provider in the chain might. That was
/// concretely true of DeepSeek, where `rig-core`'s `SUPPORTS_RESPONSE_FORMAT = false` dropped the
/// schema before the wire.
///
/// **F39 answered that question at routing time instead, which is why the answer here is no.**
/// A model row that cannot carry a schema is no longer *selected* for a schema-carrying request
/// (`a_deepseek_row_claiming_structured_output_is_not_routed_a_structured_request` and
/// `a_structured_request_routes_past_deepseek_to_a_provider_that_sends_the_schema`, both in
/// `tests/structured_output.rs`). What remains after F39 is not a capability failure that another
/// provider fixes — it is either the caller's schema being unusable, which is unusable everywhere,
/// or a model declining to comply, which is a quality question. Moira's fallback chain is a
/// reliability mechanism: it answers "this provider is unavailable", not "this model's prose was
/// disappointing", and silently answering from a different model because the first was chatty
/// changes who produced the answer with nothing on the wire to say so.
///
/// **The decisive constraint: a class carries exactly one disposition, and this class has three
/// emitters.** `is_fallback_eligible` cannot distinguish "the model replied badly" from "the
/// caller sent a 2 MB schema". Admitting the class would let one caller's malformed schema walk
/// the entire fallback chain on every request — a caller-triggered amplification against every
/// provider the route lists. Whatever the reply case deserves, the request case must not pay for
/// it, and today they share a class.
pub fn is_fallback_eligible(class: ExecutionFailureClass) -> bool {
    matches!(
        class,
        ExecutionFailureClass::CredentialNotFound
            | ExecutionFailureClass::ProviderTimeout
            | ExecutionFailureClass::ProviderConnectionFailed
            | ExecutionFailureClass::ProviderRateLimited
            | ExecutionFailureClass::ProviderUnavailable
            | ExecutionFailureClass::ProviderUpstreamError
            | ExecutionFailureClass::CircuitOpen
            | ExecutionFailureClass::CapacityExhausted
    )
}

/// Whether this failure is evidence that the provider itself is unhealthy.
///
/// # `StructuredOutputInvalid` is deliberately absent, and here the reason is blast radius
///
/// Every class in this set is a statement *about the provider*: it timed out, it refused the
/// connection, it returned a body that is not a completion. A breaker entry is keyed on
/// `(provider_id, model_id)` and, once open, refuses traffic for **every** caller on that pair.
///
/// A structured-output failure is a statement about the *request* — the caller's schema, or (since
/// issue #80) a model's reply to it. Admitting it would let a single tenant
/// posting an unreadable schema in a loop trip `circuit_failure_threshold` and take a healthy
/// provider offline for everyone routed through it. That is a caller-triggered denial of service
/// wearing a health check's clothes, and it is the reason this exclusion is the least arguable of
/// the three.
fn is_circuit_failure(class: ExecutionFailureClass) -> bool {
    matches!(
        class,
        ExecutionFailureClass::ProviderTimeout
            | ExecutionFailureClass::ProviderConnectionFailed
            | ExecutionFailureClass::ProviderUnavailable
            | ExecutionFailureClass::ProviderInvalidResponse
            | ExecutionFailureClass::ProviderUpstreamError
            | ExecutionFailureClass::ProviderRateLimited
    )
}

fn evict_oldest_inactive_limiter<K>(map: &mut HashMap<K, LimiterEntry>) -> bool
where
    K: Eq + Hash + Clone,
{
    let Some(oldest_key) = map
        .iter()
        .filter(|(_, entry)| entry.limiter.active() == 0)
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(key, _)| key.clone())
    else {
        return false;
    };
    map.remove(&oldest_key);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RuntimePolicyStatus;
    use chrono::Utc;

    /// The disposition every failure class is *supposed* to have, written out independently of
    /// the three `matches!` blocks it guards.
    ///
    /// Hand-written rather than derived, for F52's reason: a table computed from the thing it
    /// checks agrees with it by construction and proves nothing. The `match` is exhaustive, so a
    /// new [`ExecutionFailureClass`] variant does not compile until someone has decided what it
    /// means for retry, fallback and the breaker.
    fn expected_disposition(class: ExecutionFailureClass) -> (bool, bool, bool) {
        use ExecutionFailureClass as C;
        // (retryable, fallback_eligible, circuit_failure)
        match class {
            // Caller and configuration errors: identical on the next attempt, identical on the
            // next provider, and no evidence about provider health.
            C::InvalidExecutionRequest
            | C::ApplicationUnavailable
            | C::RouteNotFound
            | C::RouteForbidden
            // Issue #79. A route naming a missing or disabled agent profile is deployment
            // configuration: the same route resolves to the same unusable profile on the next
            // attempt, every provider in the chain would be handed the same route, and no
            // provider was contacted, so there is nothing to say about anyone's health.
            | C::AgentProfileNotFound
            | C::AgentProfileDisabled
            | C::ModelNotFound
            | C::ModelForbidden
            | C::ModelCapabilityMismatch
            | C::NoEligibleModel
            | C::CredentialForbidden
            | C::CredentialExpired
            | C::CredentialDisabled
            | C::CredentialDecryptionFailed
            | C::ProviderConfigurationInvalid
            | C::RequestCancelled
            | C::DeadlineExceeded
            | C::ProviderAuthenticationFailed
            | C::StreamBackpressureExceeded
            | C::InternalError => (false, false, false),
            // F29's first precondition, and unchanged by issue #80's third emitter — the row was
            // written with that emitter already in view. See the three `is_*` doc comments: no
            // retry (the schema is the same schema; a zero-temperature resample is the same
            // reply), no fallback (F39 moved the capability question to routing, and the class
            // cannot tell a bad caller schema from a bad model reply), no breaker (a caller must
            // not be able to take a healthy provider offline for everyone).
            C::StructuredOutputInvalid => (false, false, false),
            // This provider cannot serve the request but another may hold a usable credential.
            C::CredentialNotFound => (false, true, false),
            // Operational: worth another attempt, worth another provider.
            C::ProviderTimeout
            | C::ProviderConnectionFailed
            | C::ProviderUnavailable
            | C::ProviderRateLimited
            | C::ProviderUpstreamError => (true, true, true),
            // Retry and fallback, but the breaker is what raised it — feeding it back would keep
            // it open on its own output.
            C::CircuitOpen | C::CapacityExhausted => (true, true, false),
            // A body that is not a completion is provider health evidence, but replaying the same
            // request at the same provider is not expected to change it.
            C::ProviderInvalidResponse => (false, false, true),
            // Issue #139. The four content-encryption classes are raised on the conversation
            // *persistence* path — sealing a message or opening a stored one — and no provider is
            // contacted on any of them. So all three answers are the same "no", for three
            // different reasons rather than by default:
            //
            // * **No retry.** `ContentDecryptionFailed`, `ContentEnvelopeUnsupported` and
            //   `ContentKeyAbandoned` describe bytes that are already stored; a second attempt
            //   reads the same bytes with the same keyring. `ContentKeyUnavailable` *is* resolved
            //   by waiting, but by a keyring refresh or an operator action on a scale of
            //   minutes — not by this execution's retry budget, which would burn its attempts in
            //   milliseconds and report the same thing. The caller gets a `503` and retries.
            // * **No fallback.** The keyring is process-wide, not per provider. Handing the same
            //   row to the next provider in the chain changes nothing about whether it opens.
            // * **No breaker.** None of these says anything about a provider's health, and a
            //   single application with a damaged row must not be able to take a provider
            //   offline for every other caller routed through it.
            C::ContentDecryptionFailed
            | C::ContentEnvelopeUnsupported
            | C::ContentKeyUnavailable
            | C::ContentKeyAbandoned => (false, false, false),
        }
    }

    /// F29's first precondition, stated as the property rather than as a membership check.
    ///
    /// **Why the whole table and not three asserts about one variant.** HANDOFF §3.4's thirteenth
    /// shape is that a guard which iterates a constant cannot see a name being *removed* from it;
    /// membership guards are one-directional. Asserting the full `(retry, fallback, circuit)`
    /// triple for every class is bidirectional by construction — an addition and a removal both
    /// go red — and it makes the disposition of `StructuredOutputInvalid` a recorded decision
    /// sitting next to every other class's, which is what "give it a disposition" has to mean if
    /// it is to survive the next edit.
    ///
    /// **Honest about its limit.** The loop walks [`ExecutionFailureClass::ALL`], and that array
    /// can rot — its own doc comment says so. The exhaustive `match` in [`expected_disposition`]
    /// is the backstop: a variant missing from `ALL` is still a compile error here.
    #[test]
    fn every_failure_class_has_a_recorded_retry_fallback_and_circuit_disposition() {
        for class in ExecutionFailureClass::ALL {
            let actual = (
                is_retryable(class),
                is_fallback_eligible(class),
                is_circuit_failure(class),
            );
            assert_eq!(
                actual,
                expected_disposition(class),
                "{class:?}: (retryable, fallback_eligible, circuit_failure) changed. \
                 This table is the record of the decision, not a mirror of the code — \
                 if the new behaviour is intended, change the table and say why in the \
                 is_retryable / is_fallback_eligible / is_circuit_failure doc comments."
            );
        }
    }

    /// The precondition itself, said out loud so a reader grepping for it finds a test.
    ///
    /// Redundant with the table above *by design*: the table is what catches a drive-by edit, and
    /// this is what tells whoever caused the red which decision they walked into. Deleting the
    /// table would leave this one-directional; deleting this would leave the reason implicit.
    #[test]
    fn structured_output_invalid_is_in_none_of_the_three_dispositions() {
        let class = ExecutionFailureClass::StructuredOutputInvalid;
        assert!(
            !is_retryable(class),
            "a schema that is unreadable is unreadable on the second attempt too, and a \
             zero-temperature resample of a non-conforming reply is the same reply"
        );
        assert!(
            !is_fallback_eligible(class),
            "F39 moved the capability question to routing; what is left is a caller schema that \
             fails everywhere, and this class cannot tell that apart from a model's reply — so \
             admitting it lets one bad schema walk the whole fallback chain"
        );
        assert!(
            !is_circuit_failure(class),
            "breaker entries are per (provider, model) and refuse traffic for every caller; a \
             request-shaped failure must never be able to open one"
        );
        // The disposition is only safe while the emitter set is what the catalog says it is.
        // `structured_output_invalid_has_only_the_three_emitters_its_catalog_entry_describes`
        // (src/i18n/catalog/mod.rs) is the interlock: a fourth emitter goes red there, which is
        // the prompt to re-read this decision rather than inherit it. Issue #80's third emitter
        // went through exactly that prompt and left the disposition unchanged.
    }

    fn policy(threshold: i32) -> ProviderRuntimePolicyRecord {
        ProviderRuntimePolicyRecord {
            id: Uuid::now_v7(),
            provider_id: Uuid::now_v7(),
            connect_timeout_ms: 1,
            request_timeout_ms: 1,
            stream_idle_timeout_ms: 1,
            max_concurrent_requests: 1,
            max_concurrent_streams: 1,
            retry_limit: 0,
            retry_base_delay_ms: 1,
            retry_max_delay_ms: 1,
            circuit_failure_threshold: threshold,
            circuit_open_duration_ms: 60_000,
            status: RuntimePolicyStatus::Active,
            updated_at: Utc::now(),
            version: 1,
        }
    }

    #[tokio::test]
    async fn global_concurrency_rejects_when_full() {
        let controller = ConcurrencyController::new(1, 1, 1, 8);
        let provider = Uuid::now_v7();
        let first = controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap();
        let second = controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap_err();
        // F43 — the entry point now returns `CapacityExhaustion`, which names the ceiling that
        // was hit; the old wrapper returned an `ExecutionFailure` that had already thrown that
        // away. Both are asserted: the scope, because it is the discriminating fact, and the
        // conversion, because it is what the execution path actually reports to a caller.
        assert_eq!(second.scope(), CapacityScope::Global);
        let failure: ExecutionFailure = second.into();
        assert_eq!(failure.class, ExecutionFailureClass::CapacityExhausted);
        drop(first);
        assert!(
            controller
                .acquire(provider, 1, false, 1, None, None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn active_dynamic_limiter_is_not_evicted() {
        let controller = ConcurrencyController::new(4, 1, 1, 1);
        let first_provider = Uuid::now_v7();
        let second_provider = Uuid::now_v7();
        let first = controller
            .acquire(first_provider, 1, false, 1, None, None)
            .await
            .unwrap();

        let exhausted = controller
            .acquire(second_provider, 1, false, 1, None, None)
            .await
            .unwrap_err();
        assert_eq!(exhausted.scope(), CapacityScope::LimiterRegistry);
        assert!(
            controller
                .providers
                .lock()
                .await
                .contains_key(&first_provider)
        );

        drop(first);
        assert!(
            controller
                .acquire(second_provider, 1, false, 1, None, None)
                .await
                .is_ok()
        );
        let providers = controller.providers.lock().await;
        assert!(!providers.contains_key(&first_provider));
        assert!(providers.contains_key(&second_provider));
    }

    #[tokio::test]
    async fn limit_changes_preserve_active_permits() {
        let controller = ConcurrencyController::new(8, 1, 1, 8);
        let provider = Uuid::now_v7();
        let first = controller
            .acquire(provider, 2, false, 1, None, None)
            .await
            .unwrap();
        let second = controller
            .acquire(provider, 2, false, 1, None, None)
            .await
            .unwrap();

        let lowered = controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap_err();
        assert_eq!(lowered.scope(), CapacityScope::ProviderRequest);

        drop(first);
        let still_full = controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap_err();
        assert_eq!(still_full.scope(), CapacityScope::ProviderRequest);

        let raised = controller
            .acquire(provider, 2, false, 1, None, None)
            .await
            .unwrap();
        drop(second);
        drop(raised);
        assert!(
            controller
                .acquire(provider, 1, false, 1, None, None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn streams_consume_request_and_stream_capacity() {
        let controller = ConcurrencyController::new(8, 1, 1, 8);
        let provider = Uuid::now_v7();
        let stream = controller
            .acquire(provider, 3, true, 1, None, None)
            .await
            .unwrap();

        let second_stream = controller
            .acquire(provider, 3, true, 1, None, None)
            .await
            .unwrap_err();
        assert_eq!(second_stream.scope(), CapacityScope::ProviderStream);

        let request = controller
            .acquire(provider, 3, false, 1, None, None)
            .await
            .unwrap();
        drop(stream);
        assert!(
            controller
                .acquire(provider, 3, true, 1, None, None)
                .await
                .is_ok()
        );
        drop(request);
    }

    #[tokio::test]
    async fn capacity_exhaustion_reports_each_scope() {
        let provider = Uuid::now_v7();
        let application = Uuid::now_v7();

        let global_controller = ConcurrencyController::new(1, 2, 2, 8);
        let _global = global_controller
            .acquire(provider, 2, false, 1, None, None)
            .await
            .unwrap();
        assert_eq!(
            global_controller
                .acquire(provider, 2, false, 1, None, None)
                .await
                .unwrap_err()
                .scope(),
            CapacityScope::Global
        );

        let provider_controller = ConcurrencyController::new(4, 2, 2, 8);
        let _provider = provider_controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap();
        assert_eq!(
            provider_controller
                .acquire(provider, 1, false, 1, None, None)
                .await
                .unwrap_err()
                .scope(),
            CapacityScope::ProviderRequest
        );

        let application_controller = ConcurrencyController::new(4, 1, 2, 8);
        let _application = application_controller
            .acquire(provider, 4, false, 1, Some(application), None)
            .await
            .unwrap();
        assert_eq!(
            application_controller
                .acquire(provider, 4, false, 1, Some(application), None)
                .await
                .unwrap_err()
                .scope(),
            CapacityScope::Application
        );

        let user_controller = ConcurrencyController::new(4, 2, 1, 8);
        let _user = user_controller
            .acquire(provider, 4, false, 1, None, Some("user-1"))
            .await
            .unwrap();
        assert_eq!(
            user_controller
                .acquire(provider, 4, false, 1, None, Some("user-1"))
                .await
                .unwrap_err()
                .scope(),
            CapacityScope::User
        );
    }

    #[tokio::test]
    async fn failed_later_scope_releases_earlier_permits() {
        let controller = ConcurrencyController::new(2, 1, 1, 8);
        let provider = Uuid::now_v7();
        let blocked_application = Uuid::now_v7();
        let available_application = Uuid::now_v7();
        let held = controller
            .acquire(provider, 2, false, 1, Some(blocked_application), None)
            .await
            .unwrap();

        let failure = controller
            .acquire(provider, 2, false, 1, Some(blocked_application), None)
            .await
            .unwrap_err();
        assert_eq!(failure.scope(), CapacityScope::Application);

        let recovered = controller
            .acquire(provider, 2, false, 1, Some(available_application), None)
            .await
            .unwrap();
        drop(held);
        drop(recovered);

        assert!(
            controller
                .acquire(provider, 1, false, 1, Some(blocked_application), None)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn circuit_opens_after_operational_failures() {
        let registry = CircuitBreakerRegistry::new();
        let provider = Uuid::now_v7();
        let model = Uuid::now_v7();
        let policy = policy(1);
        assert!(registry.before_call(provider, model, &policy).await.is_ok());
        registry
            .on_failure(
                provider,
                model,
                &policy,
                ExecutionFailureClass::ProviderTimeout,
            )
            .await;
        let failure = registry
            .before_call(provider, model, &policy)
            .await
            .unwrap_err();
        assert_eq!(failure.class, ExecutionFailureClass::CircuitOpen);
    }

    /// Three breakers across two providers, all open, so any leak between them is
    /// visible. `states` is read directly rather than through `before_call`, which
    /// re-inserts a `Closed` entry on a miss and would hide the difference between
    /// "cleared" and "never there".
    async fn three_open_circuits(
        registry: &CircuitBreakerRegistry,
        provider_a: Uuid,
        model_a1: Uuid,
        model_a2: Uuid,
        provider_b: Uuid,
        model_b: Uuid,
    ) {
        let policy = policy(1);
        for (provider, model) in [
            (provider_a, model_a1),
            (provider_a, model_a2),
            (provider_b, model_b),
        ] {
            registry
                .on_failure(
                    provider,
                    model,
                    &policy,
                    ExecutionFailureClass::ProviderTimeout,
                )
                .await;
        }
        assert_eq!(registry.states.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn reset_for_resource_clears_only_the_named_providers_entries() {
        let registry = CircuitBreakerRegistry::new();
        let (provider_a, model_a1, model_a2, provider_b, model_b) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        three_open_circuits(
            &registry, provider_a, model_a1, model_a2, provider_b, model_b,
        )
        .await;

        registry
            .reset_for_resource(CircuitResetScope::Provider(provider_a))
            .await;

        let states = registry.states.lock().await;
        assert_eq!(
            states.keys().collect::<Vec<_>>(),
            vec![&(provider_b, model_b)]
        );
    }

    #[tokio::test]
    async fn reset_for_resource_clears_only_the_named_models_entries() {
        let registry = CircuitBreakerRegistry::new();
        let (provider_a, model_a1, model_a2, provider_b, model_b) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        three_open_circuits(
            &registry, provider_a, model_a1, model_a2, provider_b, model_b,
        )
        .await;

        registry
            .reset_for_resource(CircuitResetScope::Model(model_a1))
            .await;

        let states = registry.states.lock().await;
        assert!(!states.contains_key(&(provider_a, model_a1)));
        assert!(states.contains_key(&(provider_a, model_a2)));
        assert!(states.contains_key(&(provider_b, model_b)));
    }

    #[tokio::test]
    async fn reset_for_resource_ignores_unrelated_resource_types() {
        let registry = CircuitBreakerRegistry::new();
        let (provider_a, model_a1, model_a2, provider_b, model_b) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        three_open_circuits(
            &registry, provider_a, model_a1, model_a2, provider_b, model_b,
        )
        .await;

        registry
            .reset_for_resource(CircuitResetScope::Unaffected)
            .await;

        assert_eq!(registry.states.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn reset_all_still_clears_everything() {
        let registry = CircuitBreakerRegistry::new();
        let (provider_a, model_a1, model_a2, provider_b, model_b) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        three_open_circuits(
            &registry, provider_a, model_a1, model_a2, provider_b, model_b,
        )
        .await;

        registry.reset_all().await;
        assert!(registry.states.lock().await.is_empty());

        three_open_circuits(
            &registry, provider_a, model_a1, model_a2, provider_b, model_b,
        )
        .await;
        registry.reset_for_resource(CircuitResetScope::All).await;
        assert!(registry.states.lock().await.is_empty());
    }
}

#[cfg(test)]
mod cluster_tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };

    /// An in-process stand-in for Redis.
    ///
    /// This is what makes plan 10 §0.4b's requirement testable: *"every Redis code
    /// path needs a test that the default build behaves correctly with Redis
    /// absent — not merely that it compiles"*. Both halves of that are pure logic
    /// once the transport is a trait — the fail-closed arm needs a coordinator
    /// that errors, not a container that is down, and the default arm needs no
    /// coordinator at all.
    ///
    /// It implements the *same* arithmetic the Lua scripts do, deliberately: the
    /// parity tests below assert the in-memory and cluster backends agree at the
    /// boundary, and `src/infra/redis.rs` asserts the scripts spell that
    /// arithmetic the same way.
    #[derive(Debug, Default)]
    struct FakeCoordinator {
        counters: std::sync::Mutex<HashMap<String, i64>>,
        /// When set, every call fails — the Redis-unreachable case.
        fail: bool,
        releases: AtomicUsize,
    }

    impl FakeCoordinator {
        fn healthy() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                fail: true,
                ..Self::default()
            })
        }

        fn value(&self, key: &str) -> i64 {
            *self.counters.lock().unwrap().get(key).unwrap_or(&0)
        }
    }

    #[async_trait]
    impl ClusterCoordinator for FakeCoordinator {
        async fn check_rate_window(
            &self,
            key: &str,
            limit: u32,
            _window: Duration,
        ) -> Result<bool, AppError> {
            if self.fail {
                return Err(AppError::Config("coordinator down".to_string()));
            }
            let mut counters = self.counters.lock().unwrap();
            let count = counters.entry(key.to_string()).or_insert(0);
            if *count >= i64::from(limit.max(1)) {
                return Ok(false);
            }
            *count += 1;
            Ok(true)
        }

        async fn try_acquire_permit(
            &self,
            key: &str,
            limit: usize,
            _ttl: Duration,
        ) -> Result<bool, AppError> {
            if self.fail {
                return Err(AppError::Config("coordinator down".to_string()));
            }
            let mut counters = self.counters.lock().unwrap();
            let active = counters.entry(key.to_string()).or_insert(0);
            if *active >= i64::try_from(limit.max(1)).unwrap_or(i64::MAX) {
                return Ok(false);
            }
            *active += 1;
            Ok(true)
        }

        async fn release_permit(&self, key: &str) -> Result<(), AppError> {
            self.releases.fetch_add(1, AtomicOrdering::AcqRel);
            if self.fail {
                return Err(AppError::Config("coordinator down".to_string()));
            }
            let mut counters = self.counters.lock().unwrap();
            let active = counters.entry(key.to_string()).or_insert(0);
            *active = (*active - 1).max(0);
            Ok(())
        }

        fn key(&self, suffix: &str) -> String {
            format!("moira:{suffix}")
        }
    }

    // ---------------------------------------------------------------------
    // Rate limiting.
    // ---------------------------------------------------------------------

    /// The window admits exactly `limit`, and the boundary is the same in both
    /// backends. This is the pin on `count >= limit.max(1)`, checked *before* the
    /// increment — an off-by-one here is a policy violation nobody would notice.
    #[tokio::test]
    async fn token_bucket_rejects_at_exactly_limit_not_above() {
        let window = Duration::from_secs(60);
        let in_memory = RateLimiterBackend::InMemory(InMemoryRateLimiter::new(16));
        let cluster =
            RateLimiterBackend::Cluster(ClusterRateLimiter::new(FakeCoordinator::healthy()));

        for backend in [&in_memory, &cluster] {
            for attempt in 1..=3 {
                assert!(
                    backend.check("app".to_string(), 3, window).await.is_ok(),
                    "attempt {attempt} within the limit must be admitted"
                );
            }
            assert!(backend.check("app".to_string(), 3, window).await.is_err());
        }
    }

    /// `limit.max(1)`: a zero limit is a misconfiguration, and treating it as "no
    /// requests ever" would take a deployment down on a typo.
    #[tokio::test]
    async fn token_bucket_treats_zero_limit_as_one() {
        let window = Duration::from_secs(60);
        let in_memory = RateLimiterBackend::InMemory(InMemoryRateLimiter::new(16));
        let cluster =
            RateLimiterBackend::Cluster(ClusterRateLimiter::new(FakeCoordinator::healthy()));

        for backend in [&in_memory, &cluster] {
            assert!(backend.check("app".to_string(), 0, window).await.is_ok());
            assert!(backend.check("app".to_string(), 0, window).await.is_err());
        }
    }

    /// Distinct keys must not share a window, in either backend.
    #[tokio::test]
    async fn token_bucket_windows_are_keyed_independently() {
        let window = Duration::from_secs(60);
        let cluster =
            RateLimiterBackend::Cluster(ClusterRateLimiter::new(FakeCoordinator::healthy()));
        assert!(cluster.check("a".to_string(), 1, window).await.is_ok());
        assert!(cluster.check("a".to_string(), 1, window).await.is_err());
        assert!(cluster.check("b".to_string(), 1, window).await.is_ok());
    }

    /// The rejection is byte-identical across backends, so a client cannot tell
    /// which one refused it and no i18n key changes with the deployment shape.
    #[tokio::test]
    async fn rate_limit_rejection_is_identical_across_backends() {
        let window = Duration::from_secs(60);
        let in_memory = RateLimiterBackend::InMemory(InMemoryRateLimiter::new(16));
        let cluster =
            RateLimiterBackend::Cluster(ClusterRateLimiter::new(FakeCoordinator::healthy()));

        let _ = in_memory.check("k".to_string(), 1, window).await;
        let _ = cluster.check("k".to_string(), 1, window).await;
        let local = in_memory
            .check("k".to_string(), 1, window)
            .await
            .unwrap_err();
        let remote = cluster.check("k".to_string(), 1, window).await.unwrap_err();

        let (
            AppError::Api {
                status: ls,
                code: lc,
                message: lm,
                ..
            },
            AppError::Api {
                status: rs,
                code: rc,
                message: rm,
                ..
            },
        ) = (&local, &remote)
        else {
            panic!("both backends must reject with a coded API error: {local:?} / {remote:?}");
        };
        assert_eq!(ls, rs);
        assert_eq!(lc, rc);
        assert_eq!(lm, rm);
        assert_eq!(*lc, "rate_limited");
    }

    /// **Fail closed.** A coordinator that cannot answer must refuse, never admit:
    /// admitting would silently restore the per-replica multiplier this backend
    /// exists to remove, at exactly the moment nobody is watching.
    #[tokio::test]
    async fn rate_limit_fails_closed_when_the_coordinator_is_unreachable() {
        let backend =
            RateLimiterBackend::Cluster(ClusterRateLimiter::new(FakeCoordinator::failing()));
        let error = backend
            .check("k".to_string(), 1_000, Duration::from_secs(60))
            .await
            .expect_err("an unreachable coordinator must not admit traffic");
        assert!(matches!(&error, AppError::Api { code, .. } if *code == "rate_limited"));
    }

    // ---------------------------------------------------------------------
    // Concurrency permits.
    // ---------------------------------------------------------------------

    fn controller(limit: usize) -> ConcurrencyController {
        ConcurrencyController::new(limit, limit, limit, 64)
    }

    /// **The §0.4b test that matters most:** the shipped build, with no
    /// coordinator at all, still admits and refuses correctly. If this fails,
    /// every default deployment is broken regardless of what the Redis arm does.
    #[tokio::test]
    async fn the_default_build_enforces_limits_with_no_coordinator() {
        let controller = controller(2);
        assert!(!controller.is_cluster_wide());
        let provider = Uuid::now_v7();

        let first = controller.acquire(provider, 2, false, 2, None, None).await;
        let second = controller.acquire(provider, 2, false, 2, None, None).await;
        assert!(first.is_ok() && second.is_ok());
        assert!(
            controller
                .acquire(provider, 2, false, 2, None, None)
                .await
                .is_err(),
            "the third acquire exceeds the global limit of 2"
        );

        drop(first);
        assert!(
            controller
                .acquire(provider, 2, false, 2, None, None)
                .await
                .is_ok(),
            "a released permit must be reusable"
        );
    }

    /// Two controllers sharing one coordinator are two replicas. With the cluster
    /// layer on, the ceiling is the *sum* across them — which is the entire point
    /// of P3-1.
    #[tokio::test]
    async fn concurrency_is_enforced_across_two_controllers_sharing_a_coordinator() {
        let coordinator = FakeCoordinator::healthy();
        let ttl = Duration::from_secs(60);
        // Each replica's local ceiling is 2; the cluster ceiling is also 2, so the
        // pair must admit 2 in total rather than 2 each.
        let replica_a = controller(2).with_cluster(coordinator.clone(), ttl);
        let replica_b = controller(2).with_cluster(coordinator.clone(), ttl);
        let provider = Uuid::now_v7();

        let a1 = replica_a.acquire(provider, 2, false, 2, None, None).await;
        let b1 = replica_b.acquire(provider, 2, false, 2, None, None).await;
        assert!(a1.is_ok() && b1.is_ok());

        assert!(
            replica_a
                .acquire(provider, 2, false, 2, None, None)
                .await
                .is_err(),
            "without the cluster layer this replica would still have a local slot free"
        );
        assert!(
            replica_b
                .acquire(provider, 2, false, 2, None, None)
                .await
                .is_err()
        );
    }

    /// The same scenario with **no** coordinator is the documented cost of the
    /// default: N replicas admit N times the limit. Pinned so the trade is a
    /// tested fact rather than a paragraph in a plan.
    #[tokio::test]
    async fn without_a_coordinator_two_replicas_admit_twice_the_limit() {
        let replica_a = controller(1);
        let replica_b = controller(1);
        let provider = Uuid::now_v7();

        let a = replica_a.acquire(provider, 1, false, 1, None, None).await;
        let b = replica_b.acquire(provider, 1, false, 1, None, None).await;
        assert!(
            a.is_ok() && b.is_ok(),
            "this is the per-replica multiplier §0.4b accepts, bounded by the \
             admission lease's cap on N"
        );
    }

    /// Dropping the permits must return the cluster slots, or the ceiling ratchets
    /// down to zero over the life of the process.
    #[tokio::test]
    async fn dropping_permits_returns_the_cluster_slots() {
        let coordinator = FakeCoordinator::healthy();
        let controller = controller(1).with_cluster(coordinator.clone(), Duration::from_secs(60));
        let provider = Uuid::now_v7();
        let key = format!("moira:permit:provider:{provider}");

        let permit = controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .unwrap();
        assert_eq!(coordinator.value(&key), 1);
        drop(permit);

        // The release is a detached task; yield until it lands rather than
        // sleeping. A bounded loop, so a genuinely stuck release fails the test
        // instead of hanging it.
        for _ in 0..64 {
            if coordinator.value(&key) == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            coordinator.value(&key),
            0,
            "the cluster slot was never returned"
        );
    }

    /// A failure part-way through must not leave the earlier scopes' cluster slots
    /// taken. The user scope is the last one acquired, so exhausting it is the way
    /// to exercise the unwind.
    #[tokio::test]
    async fn a_partial_acquire_releases_every_slot_it_already_took() {
        let coordinator = FakeCoordinator::healthy();
        let ttl = Duration::from_secs(60);
        let controller =
            ConcurrencyController::new(8, 8, 1, 64).with_cluster(coordinator.clone(), ttl);
        let provider = Uuid::now_v7();

        let held = controller
            .acquire(provider, 8, false, 8, None, Some("user-a"))
            .await
            .expect("the first acquire fits");

        // Second acquire for the same user exceeds the user limit of 1, after the
        // global and provider slots have already been taken.
        assert!(
            controller
                .acquire(provider, 8, false, 8, None, Some("user-a"))
                .await
                .is_err()
        );

        for _ in 0..64 {
            if coordinator.value("moira:permit:global") == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            coordinator.value("moira:permit:global"),
            1,
            "the failed acquire's global slot was not released"
        );
        drop(held);
    }

    /// **Fail closed**, for the same reason the rate limiter does.
    #[tokio::test]
    async fn concurrency_fails_closed_when_the_coordinator_is_unreachable() {
        let controller =
            controller(64).with_cluster(FakeCoordinator::failing(), Duration::from_secs(60));
        assert!(
            controller
                .acquire(Uuid::now_v7(), 64, false, 64, None, None)
                .await
                .is_err(),
            "an unreachable coordinator must not admit unbounded concurrency"
        );
    }

    /// A caller-supplied user id must never appear verbatim in a shared key.
    /// Redis is operationally visible in ways process memory is not, and a key is
    /// a value.
    #[tokio::test]
    async fn the_user_scope_key_never_carries_the_raw_identifier() {
        let coordinator = FakeCoordinator::healthy();
        let controller = controller(4).with_cluster(coordinator.clone(), Duration::from_secs(60));
        let user = "person@example.com";

        let _permit = controller
            .acquire(Uuid::now_v7(), 4, false, 4, None, Some(user))
            .await
            .unwrap();

        let keys: Vec<String> = coordinator
            .counters
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert!(
            keys.iter().any(|key| key.starts_with("moira:permit:user:")),
            "the user scope was never counted: {keys:?}"
        );
        for key in &keys {
            assert!(!key.contains(user), "raw identifier in a shared key: {key}");
            assert!(
                !key.contains("example.com"),
                "raw identifier in a shared key: {key}"
            );
        }
    }

    #[test]
    fn hashed_key_segments_are_stable_and_distinct() {
        assert_eq!(hashed_key_segment("a"), hashed_key_segment("a"));
        assert_ne!(hashed_key_segment("a"), hashed_key_segment("b"));
        assert_eq!(hashed_key_segment("a").len(), 32);
    }
}
