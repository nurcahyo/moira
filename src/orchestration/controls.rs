use std::{
    collections::HashMap,
    future::Future,
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
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

#[derive(Debug, Clone)]
pub struct ConcurrencyController {
    global: Arc<Semaphore>,
    application_limit: usize,
    user_limit: usize,
    max_dynamic_limiters: usize,
    providers: Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
    applications: Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
    users: Arc<Mutex<HashMap<String, LimiterEntry>>>,
}

#[derive(Debug, Clone)]
struct LimiterEntry {
    semaphore: Arc<Semaphore>,
    limit: usize,
    last_used: Instant,
}

#[derive(Debug)]
pub struct ExecutionPermits {
    _global: OwnedSemaphorePermit,
    _provider: OwnedSemaphorePermit,
    _application: Option<OwnedSemaphorePermit>,
    _user: Option<OwnedSemaphorePermit>,
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
            application_limit: application_limit.max(1),
            user_limit: user_limit.max(1),
            max_dynamic_limiters: max_dynamic_limiters.max(1),
            providers: Arc::new(Mutex::new(HashMap::new())),
            applications: Arc::new(Mutex::new(HashMap::new())),
            users: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn acquire(
        &self,
        provider_id: Uuid,
        provider_limit: usize,
        application_id: Option<Uuid>,
        external_user_id: Option<&str>,
    ) -> Result<ExecutionPermits, ExecutionFailure> {
        let global = self.acquire_semaphore(self.global.clone()).await?;
        let provider = self
            .limiter_for_uuid(&self.providers, provider_id, provider_limit.max(1))
            .await;
        let provider = self.acquire_semaphore(provider).await?;
        let application = match application_id {
            Some(id) => Some(
                self.acquire_semaphore(
                    self.limiter_for_uuid(&self.applications, id, self.application_limit)
                        .await,
                )
                .await?,
            ),
            None => None,
        };
        let user = match external_user_id.filter(|value| !value.is_empty()) {
            Some(user_id) => Some(
                self.acquire_semaphore(
                    self.limiter_for_string(&self.users, user_id, self.user_limit)
                        .await,
                )
                .await?,
            ),
            None => None,
        };

        Ok(ExecutionPermits {
            _global: global,
            _provider: provider,
            _application: application,
            _user: user,
        })
    }

    async fn acquire_semaphore(
        &self,
        semaphore: Arc<Semaphore>,
    ) -> Result<OwnedSemaphorePermit, ExecutionFailure> {
        semaphore.try_acquire_owned().map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::CapacityExhausted,
                "execution capacity is exhausted",
            )
        })
    }

    async fn limiter_for_uuid(
        &self,
        map: &Arc<Mutex<HashMap<Uuid, LimiterEntry>>>,
        key: Uuid,
        limit: usize,
    ) -> Arc<Semaphore> {
        let mut map = map.lock().await;
        evict_limiters_if_needed(&mut map, self.max_dynamic_limiters);
        let entry = map.entry(key).or_insert_with(|| LimiterEntry {
            semaphore: Arc::new(Semaphore::new(limit)),
            limit,
            last_used: Instant::now(),
        });
        if entry.limit != limit {
            *entry = LimiterEntry {
                semaphore: Arc::new(Semaphore::new(limit)),
                limit,
                last_used: Instant::now(),
            };
        }
        entry.last_used = Instant::now();
        entry.semaphore.clone()
    }

    async fn limiter_for_string(
        &self,
        map: &Arc<Mutex<HashMap<String, LimiterEntry>>>,
        key: &str,
        limit: usize,
    ) -> Arc<Semaphore> {
        let mut map = map.lock().await;
        evict_limiters_if_needed(&mut map, self.max_dynamic_limiters);
        let entry = map.entry(key.to_string()).or_insert_with(|| LimiterEntry {
            semaphore: Arc::new(Semaphore::new(limit)),
            limit,
            last_used: Instant::now(),
        });
        if entry.limit != limit {
            *entry = LimiterEntry {
                semaphore: Arc::new(Semaphore::new(limit)),
                limit,
                last_used: Instant::now(),
            };
        }
        entry.last_used = Instant::now();
        entry.semaphore.clone()
    }
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
            return Err(AppError::coded(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "public execution rate limit exceeded",
            ));
        }
        bucket.count = bucket.count.saturating_add(1);
        Ok(())
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

    pub async fn reset_all(&self) {
        self.states.lock().await.clear();
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

fn evict_limiters_if_needed<K>(map: &mut HashMap<K, LimiterEntry>, max: usize)
where
    K: Eq + Hash + Clone,
{
    while map.len() >= max {
        let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        map.remove(&oldest_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RuntimePolicyStatus;
    use chrono::Utc;

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
        let first = controller.acquire(provider, 1, None, None).await.unwrap();
        let second = controller
            .acquire(provider, 1, None, None)
            .await
            .unwrap_err();
        assert_eq!(second.class, ExecutionFailureClass::CapacityExhausted);
        drop(first);
        assert!(controller.acquire(provider, 1, None, None).await.is_ok());
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
}
