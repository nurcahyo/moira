//! The optional Redis coordination client.
//!
//! # Redis is off by default and that is the shipped configuration
//!
//! Plan 10 §0.4b: Postgres and per-process memory are the default coordination
//! path. [`RedisSettings::enabled`] defaults to `false`, `redis_is_optional_by_default`
//! below pins it, and every caller of this module takes `Option<RedisClient>` so
//! the absent case is the one the type system makes you handle first.
//!
//! What Redis buys when it *is* enabled is exactly two things that Postgres and
//! per-process state cannot give: a cluster-wide rate-limit window and a
//! cluster-wide concurrency counter. Everything else Moira coordinates —
//! admission, leader election, idempotency, runtime-config invalidation — is
//! already correct across replicas without it.
//!
//! # What is deliberately *not* here
//!
//! No circuit-breaker backend. Breaker state is *earned* by a replica observing
//! its own transport failures; sharing it would let one replica's bad network
//! path open the circuit for healthy replicas, which converts a local fault into
//! a cluster-wide outage. Breakers stay per-process even with Redis enabled.
//!
//! # No secret ever reaches Redis
//!
//! Every key this module builds is composed of a namespace, a fixed subsystem
//! segment, and identifiers that are UUIDs or already-hashed fingerprints. Values
//! are integers. `no_redis_key_carries_a_secret_or_raw_identifier` pins it.

use std::{sync::Arc, time::Duration};

use redis::{AsyncCommands, Script, aio::MultiplexedConnection};
use serde::Serialize;
use tokio::{sync::Mutex, time::timeout};
use uuid::Uuid;

use crate::{config::RedisSettings, error::AppError};

/// Fixed-window rate limit, in the exact arithmetic [`crate::orchestration::InMemoryRateLimiter`]
/// uses so the two backends cannot disagree about a boundary.
///
/// The rule being mirrored is `count >= limit.max(1)` **rejects**, checked before
/// the increment, so a window admits exactly `limit` requests. The window is
/// anchored at the first admitted request — the in-memory bucket stamps
/// `window_started` on creation and resets when `elapsed >= window`; here the key
/// is given its TTL on the increment that creates it and disappears when the TTL
/// runs out. Both are fixed windows anchored the same way.
///
/// Returns 1 when admitted, 0 when rejected.
const RATE_LIMIT_SCRIPT: &str = r#"
local limit = tonumber(ARGV[1])
local window_ms = tonumber(ARGV[2])
local current = tonumber(redis.call('GET', KEYS[1]) or '0')
if current >= limit then
    return 0
end
local updated = redis.call('INCR', KEYS[1])
if updated == 1 then
    redis.call('PEXPIRE', KEYS[1], window_ms)
end
return 1
"#;

/// Cluster-wide concurrency counter acquire.
///
/// Mirrors `DynamicLimiter::try_acquire`: reject when `active >= limit.max(1)`,
/// otherwise take a slot.
///
/// # The TTL is a leak bound, not an expiry policy
///
/// An in-memory permit is released by `Drop`, and a process that dies releases
/// every permit it held by ceasing to exist. A Redis counter has no such
/// guarantee: a replica killed mid-request leaves its slot taken forever. The
/// TTL is refreshed on every acquire, so a *busy* counter never expires out from
/// under live work, and an *abandoned* counter is reclaimed one TTL after the
/// last acquire. Choose the TTL longer than the longest execution — see
/// `RedisSettings::permit_ttl_seconds`.
const PERMIT_ACQUIRE_SCRIPT: &str = r#"
local limit = tonumber(ARGV[1])
local ttl_ms = tonumber(ARGV[2])
local current = tonumber(redis.call('GET', KEYS[1]) or '0')
if current >= limit then
    return 0
end
redis.call('INCR', KEYS[1])
redis.call('PEXPIRE', KEYS[1], ttl_ms)
return 1
"#;

/// Cluster-wide concurrency counter release.
///
/// Clamped at zero rather than allowed to go negative. A negative counter would
/// be *worse* than a leaked slot: it would silently raise the effective ceiling
/// above the configured limit, and nothing downstream would ever notice. A
/// missing key (TTL already reclaimed it) is a no-op rather than a `DECR` that
/// would create the key at `-1`.
const PERMIT_RELEASE_SCRIPT: &str = r#"
local current = tonumber(redis.call('GET', KEYS[1]) or '0')
if current <= 0 then
    return 0
end
local updated = redis.call('DECR', KEYS[1])
if updated < 0 then
    redis.call('SET', KEYS[1], 0)
    return 0
end
if updated == 0 then
    redis.call('DEL', KEYS[1])
end
return updated
"#;

fn rate_limit_script() -> &'static Script {
    static SCRIPT: std::sync::LazyLock<Script> =
        std::sync::LazyLock::new(|| Script::new(RATE_LIMIT_SCRIPT));
    &SCRIPT
}

fn permit_acquire_script() -> &'static Script {
    static SCRIPT: std::sync::LazyLock<Script> =
        std::sync::LazyLock::new(|| Script::new(PERMIT_ACQUIRE_SCRIPT));
    &SCRIPT
}

fn permit_release_script() -> &'static Script {
    static SCRIPT: std::sync::LazyLock<Script> =
        std::sync::LazyLock::new(|| Script::new(PERMIT_RELEASE_SCRIPT));
    &SCRIPT
}

/// The `moira_runtime_config` payload, in the shape the Postgres trigger emits it.
///
/// The Redis channel carries **the same JSON as the Postgres NOTIFY**, because the
/// subscriber runs it through the same `circuit_reset_scope` classifier. A
/// free-form payload would land in that function's unparseable arm, which fails
/// safe to `CircuitResetScope::All` — turning every invalidation message into a
/// full breaker reset and discarding health every replica earned by observing
/// real failures.
///
/// Constructing this type is the only way to publish, so a free-form string
/// cannot reach the channel by accident.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeConfigInvalidation {
    pub resource_type: &'static str,
    pub resource_id: Uuid,
}

impl RuntimeConfigInvalidation {
    pub fn new(resource_type: &'static str, resource_id: Uuid) -> Self {
        Self {
            resource_type,
            resource_id,
        }
    }

    /// The wire payload. Infallible: both fields serialise unconditionally.
    pub fn to_payload(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // Unreachable — a `&'static str` and a `Uuid` always serialise — but a
            // panic here would take down an admin write for a logging concern.
            format!(
                r#"{{"resource_type":"{}","resource_id":"{}"}}"#,
                self.resource_type, self.resource_id
            )
        })
    }
}

#[derive(Clone)]
pub struct RedisClient {
    client: redis::Client,
    namespace: String,
    connect_timeout: Duration,
    invalidation_channel: String,
    /// Reused across calls: the rate limiter and the concurrency counters sit on
    /// the request path, and opening a fresh TCP connection per check would cost
    /// more than the coordination it buys.
    ///
    /// `MultiplexedConnection` is cheap to clone (it is a handle onto one
    /// socket's request pipeline) and does **not** reconnect itself, so
    /// [`RedisClient::forget_connection`] clears the cache whenever a command
    /// fails and the next call dials again.
    connection: Arc<Mutex<Option<MultiplexedConnection>>>,
}

/// Hand-written: the client carries the connection URL, which may embed a
/// password, and this type is exactly the sort of thing a panic message formats.
impl std::fmt::Debug for RedisClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisClient")
            .field("namespace", &self.namespace)
            .field("invalidation_channel", &self.invalidation_channel)
            .finish_non_exhaustive()
    }
}

impl RedisClient {
    pub fn from_settings(settings: &RedisSettings) -> Result<Option<Self>, AppError> {
        if !settings.enabled {
            return Ok(None);
        }
        let url = settings
            .url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                AppError::Config("MOIRA_REDIS__URL is required when Redis is enabled".to_string())
            })?;
        let client = redis::Client::open(url)?;
        Ok(Some(Self {
            client,
            namespace: settings.namespace.clone(),
            connect_timeout: Duration::from_secs(settings.connect_timeout_seconds.max(1)),
            invalidation_channel: settings.invalidation_channel.clone(),
            connection: Arc::new(Mutex::new(None)),
        }))
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn invalidation_channel(&self) -> &str {
        &self.invalidation_channel
    }

    /// Every Redis round trip in this module is bounded by this.
    ///
    /// Not a separate knob from the connect timeout on purpose: the property that
    /// matters is *"no await in the request path is unbounded"*, and one value an
    /// operator can reason about serves that better than two that can be set to
    /// contradictory things.
    pub fn command_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// A live connection, dialling one if the cache is empty.
    async fn connection(&self) -> Result<MultiplexedConnection, AppError> {
        let mut cached = self.connection.lock().await;
        if let Some(connection) = cached.as_ref() {
            return Ok(connection.clone());
        }
        let connection = timeout(
            self.connect_timeout,
            self.client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| AppError::Config("redis connection timed out".to_string()))??;
        *cached = Some(connection.clone());
        Ok(connection)
    }

    /// Drops the cached connection so the next call redials.
    ///
    /// Called after any command failure. A `MultiplexedConnection` whose socket
    /// died fails every subsequent command with the same error forever, so a
    /// cached-and-never-cleared connection turns one network blip into a
    /// permanent outage of every Redis-backed control.
    async fn forget_connection(&self) {
        *self.connection.lock().await = None;
    }

    pub async fn ping(&self) -> Result<(), AppError> {
        let mut connection = self.connection().await?;
        let outcome: Result<String, AppError> = timeout(
            self.connect_timeout,
            redis::cmd("PING").query_async(&mut connection),
        )
        .await
        .map_err(|_| AppError::Config("redis ping timed out".to_string()))?
        .map_err(AppError::from);
        match outcome {
            Ok(_) => Ok(()),
            Err(error) => {
                self.forget_connection().await;
                Err(error)
            }
        }
    }

    pub async fn publish_runtime_invalidation(&self, payload: &str) -> Result<(), AppError> {
        let mut connection = self.connection().await?;
        let outcome: Result<usize, AppError> = timeout(
            self.connect_timeout,
            connection.publish(self.invalidation_channel.as_str(), payload),
        )
        .await
        .map_err(|_| AppError::Config("redis publish timed out".to_string()))?
        .map_err(AppError::from);
        match outcome {
            Ok(_) => Ok(()),
            Err(error) => {
                self.forget_connection().await;
                Err(error)
            }
        }
    }

    /// Publishes a runtime-config change onto the invalidation channel.
    ///
    /// The typed entry point — see [`RuntimeConfigInvalidation`] for why the
    /// payload shape is not negotiable.
    pub async fn publish_runtime_config_change(
        &self,
        invalidation: &RuntimeConfigInvalidation,
    ) -> Result<(), AppError> {
        self.publish_runtime_invalidation(&invalidation.to_payload())
            .await
    }

    /// Subscribes to the invalidation channel.
    ///
    /// Returns the raw `PubSub` handle rather than a stream so the caller owns the
    /// reconnect loop, mirroring how `spawn_runtime_config_listener` owns the
    /// `PgListener` reconnect loop.
    pub async fn subscribe_invalidation(&self) -> Result<redis::aio::PubSub, AppError> {
        let mut pubsub = timeout(self.connect_timeout, self.client.get_async_pubsub())
            .await
            .map_err(|_| AppError::Config("redis subscribe timed out".to_string()))??;
        timeout(
            self.connect_timeout,
            pubsub.subscribe(self.invalidation_channel.as_str()),
        )
        .await
        .map_err(|_| AppError::Config("redis subscribe timed out".to_string()))??;
        Ok(pubsub)
    }

    /// One fixed-window rate-limit check. `Ok(true)` admits.
    ///
    /// `key` is the caller's already-namespaced suffix; [`RedisClient::key`]
    /// prefixes it.
    pub async fn check_rate_window(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<bool, AppError> {
        let window_ms = i64::try_from(window.as_millis()).unwrap_or(i64::MAX).max(1);
        let mut invocation = rate_limit_script().key(key);
        invocation.arg(i64::from(limit.max(1))).arg(window_ms);
        let admitted: i64 = self.invoke(&invocation).await?;
        Ok(admitted == 1)
    }

    /// Takes one cluster-wide concurrency slot. `Ok(true)` acquired.
    pub async fn try_acquire_permit(
        &self,
        key: &str,
        limit: usize,
        ttl: Duration,
    ) -> Result<bool, AppError> {
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX).max(1);
        let limit = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
        let mut invocation = permit_acquire_script().key(key);
        invocation.arg(limit).arg(ttl_ms);
        let acquired: i64 = self.invoke(&invocation).await?;
        Ok(acquired == 1)
    }

    /// Returns one cluster-wide concurrency slot. Never drives the counter below
    /// zero; releasing an already-expired counter is a no-op.
    pub async fn release_permit(&self, key: &str) -> Result<(), AppError> {
        let _: i64 = self.invoke(&permit_release_script().key(key)).await?;
        Ok(())
    }

    /// Reads a counter without touching it. Test and diagnostic use only.
    pub async fn counter_value(&self, key: &str) -> Result<i64, AppError> {
        let mut connection = self.connection().await?;
        let outcome: Result<Option<i64>, AppError> = timeout(
            self.connect_timeout,
            redis::cmd("GET").arg(key).query_async(&mut connection),
        )
        .await
        .map_err(|_| AppError::Config("redis get timed out".to_string()))?
        .map_err(AppError::from);
        match outcome {
            Ok(value) => Ok(value.unwrap_or(0)),
            Err(error) => {
                self.forget_connection().await;
                Err(error)
            }
        }
    }

    /// Every key currently under this client's namespace.
    ///
    /// `SCAN` with a `MATCH` bounded to the namespace, never `KEYS` and never
    /// `FLUSHDB`: the integration suite runs against whatever Redis the developer
    /// or CI happens to have, and a test that swept the whole keyspace would break
    /// every other suite sharing it.
    ///
    /// Exists for the no-secret-in-Redis assertion in
    /// `tests/cluster_coordination.rs`. Named for what it is so nobody reaches for
    /// it on a request path — `SCAN` is O(keyspace).
    pub async fn scan_namespace_for_tests(&self) -> Result<Vec<String>, AppError> {
        let mut connection = self.connection().await?;
        let pattern = format!("{}:*", self.namespace);
        let mut cursor = 0u64;
        let mut keys = Vec::new();
        loop {
            let (next, batch): (u64, Vec<String>) = timeout(
                self.connect_timeout,
                redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(512)
                    .query_async(&mut connection),
            )
            .await
            .map_err(|_| AppError::Config("redis scan timed out".to_string()))??;
            keys.extend(batch);
            if next == 0 {
                return Ok(keys);
            }
            cursor = next;
        }
    }

    /// Runs a script invocation under the command timeout, clearing the cached
    /// connection on any failure.
    async fn invoke<T>(&self, invocation: &redis::ScriptInvocation<'_>) -> Result<T, AppError>
    where
        T: redis::FromRedisValue,
    {
        let mut connection = self.connection().await?;
        let outcome: Result<T, AppError> = timeout(
            self.connect_timeout,
            invocation.invoke_async(&mut connection),
        )
        .await
        .map_err(|_| AppError::Config("redis command timed out".to_string()))?
        .map_err(AppError::from);
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => {
                self.forget_connection().await;
                Err(error)
            }
        }
    }

    pub fn key(&self, suffix: &str) -> String {
        format!("{}:{suffix}", self.namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_is_optional_by_default() {
        assert!(
            RedisClient::from_settings(&RedisSettings::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn redis_requires_url_when_enabled() {
        let settings = RedisSettings {
            enabled: true,
            ..RedisSettings::default()
        };
        assert!(RedisClient::from_settings(&settings).is_err());
    }

    /// The published payload must be byte-compatible with what the Postgres
    /// trigger emits, because both are classified by the same
    /// `circuit_reset_scope`. A payload that function cannot parse falls back to
    /// `CircuitResetScope::All`, so a shape change here silently turns every
    /// invalidation into a full breaker reset.
    #[test]
    fn the_published_payload_is_the_notify_payload_shape() {
        let id = Uuid::now_v7();
        let payload = RuntimeConfigInvalidation::new("providers", id).to_payload();
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload is json");
        assert_eq!(parsed["resource_type"], "providers");
        assert_eq!(parsed["resource_id"], id.to_string());
        assert_eq!(
            parsed.as_object().map(|object| object.len()),
            Some(2),
            "an extra field would still parse, but it would mean the two channels \
             had drifted apart"
        );
    }

    #[test]
    fn redis_key_namespacing_is_stable_and_collision_free() {
        let settings = RedisSettings {
            enabled: true,
            url: Some("redis://127.0.0.1:6379/0".to_string()),
            namespace: "moira".to_string(),
            ..RedisSettings::default()
        };
        let client = RedisClient::from_settings(&settings).unwrap().unwrap();
        assert_eq!(client.key("ratelimit:abc"), "moira:ratelimit:abc");
        assert_ne!(client.key("ratelimit:a"), client.key("ratelimit:b"));
        // Two subsystems must not be able to produce the same key from different
        // inputs — the segment before the identifier is what separates them.
        assert_ne!(client.key("ratelimit:x"), client.key("permit:x"));
    }

    /// A `Debug` render that leaked the URL would put a Redis password into every
    /// panic message and every `?state` log line.
    #[test]
    fn debug_never_renders_the_connection_url() {
        let settings = RedisSettings {
            enabled: true,
            url: Some("redis://user:hunter2@127.0.0.1:6379/0".to_string()),
            ..RedisSettings::default()
        };
        let client = RedisClient::from_settings(&settings).unwrap().unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!rendered.contains("redis://"), "{rendered}");
    }

    /// The scripts are the behavioural contract with the in-memory backends. These
    /// assert the decision rule is spelled the way the parity tests in
    /// `src/orchestration/controls.rs` assume — `>=` against the limit, checked
    /// *before* the increment.
    #[test]
    fn the_rate_limit_script_rejects_at_the_limit_not_above_it() {
        assert!(RATE_LIMIT_SCRIPT.contains("if current >= limit then"));
        assert!(
            RATE_LIMIT_SCRIPT
                .contains("local current = tonumber(redis.call('GET', KEYS[1]) or '0')")
        );
    }

    /// Without the TTL a replica killed mid-request leaks its slot forever.
    #[test]
    fn the_permit_script_always_sets_an_expiry() {
        assert!(PERMIT_ACQUIRE_SCRIPT.contains("PEXPIRE"));
    }

    /// A counter allowed to go negative silently raises the effective ceiling
    /// above the configured limit, which is strictly worse than leaking a slot.
    #[test]
    fn the_release_script_never_drives_the_counter_negative() {
        assert!(PERMIT_RELEASE_SCRIPT.contains("if current <= 0 then"));
        assert!(PERMIT_RELEASE_SCRIPT.contains("if updated < 0 then"));
    }
}
