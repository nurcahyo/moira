use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use aes_gcm::aead::rand_core::{OsRng, RngCore};
use argon2::{
    Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng as PasswordOsRng},
};
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::SecretString;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{error::AppError, infra::metrics::MetricsRegistry};

use super::masking::secret_fingerprint;

/// Every namespace [`ApiKeyHasher::generate`] is called with, in one place.
///
/// It exists so [`MIN_API_KEY_PREFIX_LENGTH`] can be *derived* from the longest of them
/// rather than from a number someone measured once. A call site naming a namespace that is
/// not listed here is caught by `every_generate_call_site_names_a_registered_namespace`,
/// which walks `src/` for the same reason the i18n catalog gate does: a literal at a call
/// site is invisible to a constant maintained by hand.
pub const KEY_NAMESPACES: &[&str] = &["moira_sys", "moira_cons", "moira_inv"];

/// Random characters a generated key prefix must retain **after** its namespace and the
/// `_` separator.
///
/// The prefix is a *plaintext lookup key*: `AuthService::verify_api_key` and
/// `AdminIdentityService::resolve_invite` both select the candidate row by it and only then
/// run Argon2. Two properties are exactly as strong as the random part is long.
///
/// 1. **Uniqueness.** `admin_invites_token_prefix_active_unique` and the equivalents on
///    `system_api_keys` / `consumer_api_keys` are unique over live rows, so a prefix
///    collision is a failed write, not a retryable one.
/// 2. **The anonymous preview's cost bound.** `POST /api/v1/admin/admin-invites/preview` is
///    unauthenticated and its whole CPU-exhaustion argument is that "a caller who does not
///    already hold a valid prefix causes zero Argon2 work" — which is true only for as long
///    as guessing a live prefix is infeasible.
///
/// Eight base64url characters is 64⁸ ≈ 2.8 × 10¹⁴. The shipped `api_keys.prefix_length` of
/// 20 leaves nine for the longest namespace.
pub const MIN_RANDOM_PREFIX_CHARS: usize = 8;

/// Smallest `api_keys.prefix_length` that keeps [`MIN_RANDOM_PREFIX_CHARS`] random
/// characters for **every** namespace in [`KEY_NAMESPACES`].
///
/// Derived rather than written down: `"moira_cons"` is a character longer than
/// `"moira_inv"`, and a hand-maintained floor stops being one on the day a longer namespace
/// is added — silently, because nothing would fail.
pub const MIN_API_KEY_PREFIX_LENGTH: usize = min_api_key_prefix_length();

// ===========================================================================================
// The verification gate (issue #176)
//
// # The defect, stated as a concurrency property
//
// `verify` used to be a synchronous function running `Argon2::default().verify_password(..)`
// directly on whatever thread called it — which, for every authenticated request, is a tokio
// **worker** thread. `#[tokio::main]` with no arguments sizes the worker pool from
// `available_parallelism()`, and Rust reads the cgroup CPU quota, so at the shipped chart's
// `limits.cpu: "2"` there are two of them. Two concurrent authenticated requests were therefore
// enough to leave the executor with no thread able to poll anything else: `/healthz`, in-flight
// SSE bodies and the retention worker all stop making progress together. That is the defect —
// *where* the work runs, not how expensive it is. A cheaper Argon2 would starve the same
// executor at a higher request rate.
//
// # What this is not
//
// It is **not** a latency fix and it does not raise the authenticated request rate this process
// can sustain: Argon2id at `p=1` is single-threaded per call, so the ceiling is cores over
// service time either way. What changes is that the cost is paid out of a declared budget with
// a named 503 instead of out of the runtime's ability to poll unrelated futures.
//
// Lowering `m`, `t` or `p` was considered and refused. These are the OWASP-recommended Argon2id
// parameters, and the change would be a credential-security downgrade that *also would not fix
// this* — see the notes on [`MAX_VERIFY_M_COST_KIB`].
// ===========================================================================================

/// Resident bytes one Argon2id operation at [`Params::DEFAULT_M_COST`] allocates.
///
/// Not an estimate. `Argon2::hash_password_into` allocates `vec![Block::default();
/// params.block_count()]` and `fill_blocks` writes every byte of it, so the whole arena is
/// resident rather than merely reserved: 19456 blocks x 1024 B = 19,922,944 B = 19.0 MiB.
///
/// It is the multiplicand in the only arithmetic that matters for the gate's size. At tokio's
/// default blocking-pool bound of 512 threads, 512 x 19 MiB is 9,728 MiB against the chart's
/// `resources.limits.memory: 2Gi` — 4.75x over, which converts an executor stall into an
/// OOMKill. Rust *aborts* on allocation failure, so there is no graceful path to write for that
/// outcome. This is why [`ApiKeyHasher`] carries its own semaphore instead of relying on
/// `spawn_blocking` alone.
pub const ARGON2_ARENA_BYTES: usize = Params::DEFAULT_M_COST as usize * 1024;

/// Largest `m` (KiB) this process will allocate to verify a **stored** hash.
///
/// # The one place a database value can break the memory bound
///
/// The arena size does not come from `Argon2::default()`. `impl<T: PasswordHasher>
/// PasswordVerifier for T` passes `T::Params::try_from(hash)?` — the parameters parsed out of
/// the *stored* PHC string — so a row minted with `m=65536` would allocate 64 MiB per verify and
/// every number in [`ARGON2_ARENA_BYTES`]'s arithmetic would be fiction.
///
/// Today the only writer is [`ApiKeyHasher::hash`], which uses `Argon2::default()`, so every row
/// is 19 MiB. That is incidental, and this constant makes it structural: [`ApiKeyHasher::verify`]
/// parses the stored hash on the async side and refuses anything above this ceiling **before**
/// spending a permit. It fails closed — an unreadable or oversized stored hash is a `500`, never
/// a silent `Ok(false)`.
pub const MAX_VERIFY_M_COST_KIB: u32 = Params::DEFAULT_M_COST;

/// Upper clamp on the bound derived from [`std::thread::available_parallelism`].
///
/// Argon2id at `p=1` is single-threaded per call, so concurrency beyond the core count buys
/// **zero** throughput — it only adds resident memory and queueing latency. Sizing the gate from
/// the memory budget instead would permit 13 concurrent hashes on a two-core box for the same
/// throughput at 7x the RSS.
///
/// The clamp exists for the operator who raises `limits.cpu` to 16 and leaves memory at 2Gi:
/// worst case stays 8 x 19 MiB = 152 MiB, 7.4% of that limit.
pub const MAX_DERIVED_VERIFICATION_CONCURRENCY: usize = 8;

/// Largest `api_keys.verification_concurrency` `Settings::validate` will accept.
///
/// 64 x 19 MiB = 1,216 MiB, 59% of the chart's 2 GiB limit and 237% of its 512Mi request. Above
/// that an operator is configuring an OOMKill, so it is refused at startup rather than clamped —
/// the same reasoning `api_keys.prefix_length` records at [`ApiKeyHasher::new`].
pub const MAX_VERIFICATION_CONCURRENCY: usize = 64;

/// Default `api_keys.verification_queue_timeout_ms`.
///
/// # Bounded wait, then shed — deliberately not the house pattern
///
/// `ConcurrencyController::acquire` uses `try_acquire_owned` and refuses instantly
/// (`src/orchestration/controls.rs`). That is right there and wrong here, and the difference is
/// holding time, not primitive: those permits cover seconds-to-minutes of upstream LLM work,
/// where refusing beats queueing. This permit is held for tens of milliseconds, so instant
/// shedding at a bound of 2 would return `503` to a well-behaved caller's two-request burst —
/// converting a concurrency defect into a false-positive availability failure.
///
/// Queueing without a bound is the other wrong answer: the only backstop would be the route
/// `TimeoutLayer`, which is never below 30 s, and a thirty-second wait for a credential check is
/// a different outage rather than a fix.
///
/// 250 ms is roughly five service times at a bound of 2 — an ordinary burst absorbs, a genuine
/// overload is refused in a quarter of a second. No separate waiter cap is needed: steady-state
/// waiters are the arrival rate times 0.25 s, and each waiter is a cheap future already bounded
/// by `DefaultBodyLimit` and the route timeout.
pub const DEFAULT_VERIFICATION_QUEUE_TIMEOUT_MS: u64 = 250;

/// The gate size when nothing configures one: one permit per core, clamped to
/// `[1, MAX_DERIVED_VERIFICATION_CONCURRENCY]`.
///
/// Read **once**, at construction. A CPU limit changed by `kubectl patch` mid-life is therefore
/// not picked up until restart, which is correct for Kubernetes — a `resources` change restarts
/// the pod — and is stated here so nobody adds a refresh loop for it.
///
/// `max(1, cores - 1)` was considered, to reserve a core for the runtime, and rejected as a
/// default: with the semaphore in place the async workers are no longer starved of *threads*,
/// only of CFS quota share, and being overwhelmingly I/O-bound they are scheduled promptly on
/// wake. Reserving a core would halve auth throughput on the two-core default. It stays the
/// operator's knob rather than a hardcoded reservation.
#[must_use]
pub fn default_verification_concurrency() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .clamp(1, MAX_DERIVED_VERIFICATION_CONCURRENCY)
}

/// Whether `namespace` is one of [`KEY_NAMESPACES`].
///
/// `const` so that a namespace held in a constant can prove its own registration at
/// **compile time**. That is not decoration: the source walker in this module's tests can
/// only see namespaces spelled inline at a `generate` call site, and
/// `ADMIN_INVITE_NAMESPACE` is not one of them — it is a constant, precisely so the schema
/// value and the code agree. Without this the invite namespace would be the one namespace
/// no gate covered, which is the shape of hole that produced finding F13 and the
/// uncatalogued `validate_override` codes.
pub const fn is_registered_key_namespace(namespace: &str) -> bool {
    let mut index = 0;
    while index < KEY_NAMESPACES.len() {
        if const_str_eq(KEY_NAMESPACES[index], namespace) {
            return true;
        }
        index += 1;
    }
    false
}

const fn const_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn min_api_key_prefix_length() -> usize {
    let mut longest = 0;
    let mut index = 0;
    while index < KEY_NAMESPACES.len() {
        let candidate = KEY_NAMESPACES[index].len();
        if candidate > longest {
            longest = candidate;
        }
        index += 1;
    }
    // `+ 1` for the `_` `generate` inserts between the namespace and the random material.
    longest + 1 + MIN_RANDOM_PREFIX_CHARS
}

/// # `Debug` is hand-written for the same reason as [`crate::security::IdempotencyHasher`]
///
/// This type was **not** already redacted. What is redacted in this module is
/// [`GeneratedApiKey::raw_key`], via `SecretString` — a different secret on a different type.
/// The API-key pepper sat behind a plain `#[derive(Debug)]`, reachable from `AuthService`,
/// which does derive `Debug` and is held for the lifetime of the process. A leaked pepper
/// turns every stored Argon2id key hash into an offline-verifiable target, so it is redacted
/// here and the derive is pinned by
/// `crate::security::idempotency::tests::debug_redacts_the_pepper_everywhere_it_is_reachable`.
///
/// # `gate` is `Arc<Semaphore>`, and that is load-bearing
///
/// This type derives `Clone`, and `AppState::new` clones it into `AuthService` while keeping the
/// original on `state.key_hasher`. A per-instance semaphore would give the two halves independent
/// budgets and silently double the bound with nothing failing — the likeliest way to ship a
/// non-fix. `Semaphore` is not `Clone`, so the `Arc` is forced structurally here; what is *not*
/// forced is that both halves come from one clone rather than two constructions, which is what
/// `state::tests::the_auth_service_and_app_state_share_one_verification_budget` pins.
#[derive(Clone)]
pub struct ApiKeyHasher {
    pepper: Vec<u8>,
    pepper_version: String,
    prefix_length: usize,
    /// Bounds *concurrent Argon2 arenas*, not blocking threads. Capping
    /// `Runtime::max_blocking_threads` would bound the memory too and would be wrong: that pool
    /// also serves `getaddrinfo` (hyper/reqwest resolve through `tokio::net::lookup_host`, which
    /// is `spawn_blocking`), so a pool of 2 would put every provider call and JWKS fetch behind
    /// Argon2. Bound the operation; leave the pool alone.
    gate: Arc<Semaphore>,
    /// Kept beside the semaphore only so `Debug` and the tests can read the bound back;
    /// `Semaphore::available_permits` reports what is *free*, which is a different number.
    verification_concurrency: usize,
    queue_timeout: Duration,
    /// `None` for a directly constructed hasher (tests, and any future non-`AppState` caller).
    /// `AppState` always supplies one — see [`ApiKeyHasher::with_verification_gate`].
    metrics: Option<MetricsRegistry>,
}

impl std::fmt::Debug for ApiKeyHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyHasher")
            .field("pepper", &"[redacted]")
            .field("pepper_version", &self.pepper_version)
            .field("prefix_length", &self.prefix_length)
            // The bound and the timeout are operational facts, not secrets, and they are the two
            // numbers an operator reading a startup dump needs to explain an
            // `auth_verification_overloaded`. The semaphore itself is not rendered: its free-permit
            // count is a racing sample that reads like configuration.
            .field("verification_concurrency", &self.verification_concurrency)
            .field("verification_queue_timeout", &self.queue_timeout)
            .finish()
    }
}

/// A freshly minted API key. `raw_key` is the only place the plaintext exists — everything
/// persisted is derived from it.
///
/// **`raw_key` is `SecretString` to make disclosure a compile error, not a review question.**
/// Plan 05 found a QA probe that had written `json!({ "raw": generated.raw_key })` into
/// `audit_logs.metadata`, which the admin audit API serialises verbatim; it survived two cleanup
/// commits and was caught only by a leak test. A `String` there is one careless `json!` away from
/// doing it again, and the next one might not be planted by someone who wanted it found.
///
/// The guarantee does not depend on feature flags: `secrecy` implements
/// `Serialize for Secret<T> where T: SerializableSecret`, and `String` never implements that
/// marker — only `CloneableSecret` and `DebugSecret`. So `Secret<String>` cannot be serialised at
/// all, and `#[derive(Debug, Clone)]` below keeps working with `Debug` redacted.
///
/// Reading the plaintext now requires `expose_secret()`, which is greppable: every legitimate
/// disclosure is one search away from an auditor.
#[derive(Debug, Clone)]
pub struct GeneratedApiKey {
    pub raw_key: SecretString,
    pub key_prefix: String,
    pub key_hash: String,
    pub fingerprint: String,
    pub pepper_version: String,
}

impl ApiKeyHasher {
    /// # The floor, and why configuration no longer reaches it
    ///
    /// This used to clamp at a bare `.max(12)`, a number that knew nothing about the
    /// namespace it would be prefixing. `moira_inv_` is ten characters, so a configured
    /// `prefix_length` of 12 left **two** random base64url characters: 4096 distinct
    /// prefixes, colliding on `admin_invites_token_prefix_active_unique` — an unmapped
    /// unique violation, i.e. a `500` — and collapsing the anonymous preview's "no Argon2
    /// work without a valid prefix" bound to a 4096-guess search.
    ///
    /// The shipped default is 20, so this was configuration-only. It is now refused at
    /// startup by `Settings::validate` rather than clamped here, on the same reasoning
    /// `validated_invite_lifetime` refuses rather than clamps: an operator who believes
    /// they configured one thing and silently received another finds out at the worst
    /// possible moment, and a clamp makes the misconfiguration *invisible* instead of
    /// merely harmless.
    ///
    /// The floor stays, retargeted at [`MIN_API_KEY_PREFIX_LENGTH`], so that a direct
    /// library construction — a test, a future caller that does not come through
    /// `Settings` — still cannot produce a hasher whose prefixes are guessable. It is a
    /// backstop that configuration can no longer reach, not the gate.
    pub fn new(
        pepper: impl Into<Vec<u8>>,
        pepper_version: impl Into<String>,
        prefix_length: usize,
    ) -> Self {
        let verification_concurrency = default_verification_concurrency();
        Self {
            pepper: pepper.into(),
            pepper_version: pepper_version.into(),
            prefix_length: prefix_length.max(MIN_API_KEY_PREFIX_LENGTH),
            gate: Arc::new(Semaphore::new(verification_concurrency)),
            verification_concurrency,
            queue_timeout: Duration::from_millis(DEFAULT_VERIFICATION_QUEUE_TIMEOUT_MS),
            metrics: None,
        }
    }

    /// Replaces the derived gate with the configured one, and attaches the metrics registry.
    ///
    /// One method rather than two so the gate cannot be half-configured: a hasher with an
    /// operator-set bound and no recorder would shed silently, and a shed nobody can see is the
    /// failure mode the counter exists for.
    ///
    /// **Backstop clamps, not policy.** `Settings::validate` refuses a zero concurrency, a
    /// concurrency above [`MAX_VERIFICATION_CONCURRENCY`] and a zero timeout at startup, for the
    /// reason this module already records for `prefix_length`: an operator who believes they
    /// configured one thing and silently received another finds out at the worst possible moment.
    /// The clamps below are the same kind of backstop that floor is — they exist so a *direct
    /// library construction* cannot build a hasher whose semaphore has zero permits, which would
    /// wedge every request in this process forever.
    #[must_use]
    pub fn with_verification_gate(
        mut self,
        concurrency: usize,
        queue_timeout: Duration,
        metrics: MetricsRegistry,
    ) -> Self {
        let concurrency = concurrency.clamp(1, MAX_VERIFICATION_CONCURRENCY);
        self.gate = Arc::new(Semaphore::new(concurrency));
        self.verification_concurrency = concurrency;
        self.queue_timeout = queue_timeout.max(Duration::from_millis(1));
        self.metrics = Some(metrics);
        self
    }

    /// Concurrent Argon2 operations this hasher admits. Peak arena is this times
    /// [`ARGON2_ARENA_BYTES`].
    #[must_use]
    pub fn verification_concurrency(&self) -> usize {
        self.verification_concurrency
    }

    pub async fn generate(&self, namespace: &str) -> Result<GeneratedApiKey, AppError> {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let raw_key = format!("{namespace}_{}", URL_SAFE_NO_PAD.encode(bytes));
        let key_hash = self.hash(&raw_key).await?;
        let key_prefix = self.prefix(&raw_key);
        let fingerprint = secret_fingerprint(raw_key.as_bytes());

        Ok(GeneratedApiKey {
            raw_key: SecretString::new(raw_key),
            key_prefix,
            key_hash,
            fingerprint,
            pepper_version: self.pepper_version.clone(),
        })
    }

    /// Mints the stored PHC string for `raw_key`.
    ///
    /// Lower frequency than [`Self::verify`], same class of work, so it takes the same permit and
    /// runs off the runtime for the same reason.
    pub async fn hash(&self, raw_key: &str) -> Result<String, AppError> {
        let permit = self.acquire_argon2_permit().await?;
        let peppered = self.peppered(raw_key);

        tokio::task::spawn_blocking(move || {
            // Owned by the closure, never by the caller's future: `spawn_blocking` is not
            // cancellable, so a permit tied to the caller would be released while the arena it
            // accounts for is still allocated.
            let _permit = permit;
            let salt = SaltString::generate(&mut PasswordOsRng);
            Argon2::default()
                .hash_password(peppered.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|err| AppError::Internal(format!("hash api key: {err}")))
        })
        .await
        .map_err(|err| AppError::Internal(format!("hash api key task: {err}")))?
    }

    /// Whether `raw_key` is the plaintext behind `encoded_hash`.
    ///
    /// # Order of operations, and why each step is where it is
    ///
    /// 1. **Parse the stored hash first, on the async side.** Only to read `m` and refuse a row
    ///    above [`MAX_VERIFY_M_COST_KIB`] — the one place a database value can break the memory
    ///    bound. It depends on a database row, never on the presented key, and costs no permit.
    /// 2. **Acquire the permit before the spawn.** Acquiring *inside* the closure would park a
    ///    blocking thread per waiter and quietly restore tokio's 512-thread pool as the real
    ///    bound, which is the exact failure this semaphore exists to prevent.
    /// 3. **Move the permit into the closure**, per [`Self::hash`].
    ///
    /// The second `PasswordHash::new` inside the closure is a string parse — microseconds — and
    /// buys the ceiling check without borrowing a parsed value across the spawn boundary.
    ///
    /// # Timing
    ///
    /// Both early returns are independent of the presented key: the parameter-ceiling refusal
    /// depends on a stored row, the queue-timeout `503` on global load. The pre-existing
    /// asymmetry — a caller holding a live prefix pays Argon2, a caller without one does not — is
    /// unchanged and deliberate; it is the documented CPU-exhaustion bound, not an oversight.
    ///
    /// One genuinely new observable: under saturation a valid prefix gets a slow `503` while an
    /// invalid prefix keeps getting a fast `401`. That distinguishes only prefix-match, which
    /// timing already distinguished, so it discloses nothing new. Do **not** "fix" it by shedding
    /// before the prefix lookup: that would spend the gate on unauthenticated traffic and hand
    /// every invalid key a `503` instead of a `401`.
    pub async fn verify(&self, raw_key: &str, encoded_hash: &str) -> Result<bool, AppError> {
        let stored_m_cost = stored_m_cost_kib(encoded_hash)?;
        if stored_m_cost > MAX_VERIFY_M_COST_KIB {
            return Err(AppError::Internal(format!(
                "stored api key hash requests {stored_m_cost} KiB of Argon2 memory, above the \
                 {MAX_VERIFY_M_COST_KIB} KiB this process will allocate per verification"
            )));
        }

        let permit = self.acquire_argon2_permit().await?;
        let peppered = self.peppered(raw_key);
        let stored = encoded_hash.to_string();

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let parsed = PasswordHash::new(&stored)
                .map_err(|err| AppError::Internal(format!("parse api key hash: {err}")))?;
            Ok(Argon2::default()
                .verify_password(peppered.as_bytes(), &parsed)
                .is_ok())
        })
        .await
        .map_err(|err| AppError::Internal(format!("verify api key task: {err}")))?
    }

    /// Waits up to `queue_timeout` for one of the [`Self::verification_concurrency`] permits.
    ///
    /// **Fail closed.** The timeout produces `Err`, never `Ok(false)`. `Ok(false)` is the tempting
    /// shortcut and is wrong: it answers "invalid credential" to a caller whose credential was
    /// never checked, turning an overload into a `401` that operators chase for days as a client
    /// bug.
    async fn acquire_argon2_permit(&self) -> Result<OwnedSemaphorePermit, AppError> {
        let queued_at = Instant::now();
        match tokio::time::timeout(self.queue_timeout, self.gate.clone().acquire_owned()).await {
            Ok(Ok(permit)) => {
                if let Some(metrics) = &self.metrics {
                    metrics.record_api_key_verification(true, Some(queued_at.elapsed()));
                }
                Ok(permit)
            }
            // Unreachable while nothing calls `Semaphore::close`, and handled rather than
            // `expect`ed because the only safe answer to "I cannot check this credential" is to
            // refuse it.
            Ok(Err(_closed)) => Err(AppError::Internal(
                "the api key verification gate is closed".to_string(),
            )),
            Err(_elapsed) => {
                if let Some(metrics) = &self.metrics {
                    metrics.record_api_key_verification(false, None);
                }
                // Spelled as a literal, not a constant: `every_coded_error_literal_in_src_has_a
                // _catalog_entry` resolves codes by reading the source, and a constant here would
                // land in its `dynamic_sites` bucket and need a carve-out to stay green. The
                // literal is what keeps the i18n catalog gate load-bearing for this code.
                Err(AppError::coded(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "auth_verification_overloaded",
                    "credential hashing is at capacity on this instance; retry shortly",
                ))
            }
        }
    }

    /// Holds one gate permit for as long as the returned guard lives.
    ///
    /// Test-only, and it exists because the invariants worth pinning here are about *contention*:
    /// "two clones share one budget" and "the timeout sheds rather than answers" are both
    /// statements about a permit already being held, and reproducing that with racing Argon2
    /// calls would be a timing test rather than an assertion.
    #[cfg(test)]
    pub(crate) async fn hold_one_permit_for_test(&self) -> OwnedSemaphorePermit {
        self.gate
            .clone()
            .acquire_owned()
            .await
            .expect("the verification gate is never closed")
    }

    /// Permits currently **free** in the gate.
    ///
    /// Test-only, and deliberately not `verification_concurrency`: this is a racing sample of
    /// what is available right now, which is the wrong number for anything except waiting for a
    /// verification to have genuinely taken its permit. Production code that wanted it would be
    /// about to make a scheduling decision from a value that is stale before it is read.
    #[cfg(test)]
    pub(crate) fn free_permits_for_test(&self) -> usize {
        self.gate.available_permits()
    }

    pub fn prefix(&self, raw_key: &str) -> String {
        raw_key.chars().take(self.prefix_length).collect()
    }

    pub fn fingerprint(&self, raw_key: &str) -> String {
        secret_fingerprint(raw_key.as_bytes())
    }

    fn peppered(&self, raw_key: &str) -> String {
        format!("{raw_key}:{}", URL_SAFE_NO_PAD.encode(&self.pepper))
    }
}

/// The `m` parameter, in KiB, that verifying `encoded_hash` would allocate.
///
/// Read from the stored PHC string rather than from `Argon2::default()`, because that is where
/// `PasswordVerifier` reads it from — see [`MAX_VERIFY_M_COST_KIB`]. An unreadable stored hash is
/// an error rather than a permissive default: this value is the memory bound's only unchecked
/// input, so "I could not tell how large this is" must refuse, not guess.
fn stored_m_cost_kib(encoded_hash: &str) -> Result<u32, AppError> {
    let parsed = PasswordHash::new(encoded_hash)
        .map_err(|err| AppError::Internal(format!("parse api key hash: {err}")))?;
    Params::try_from(&parsed)
        .map(|params| params.m_cost())
        .map_err(|err| AppError::Internal(format!("read api key hash parameters: {err}")))
}

#[cfg(test)]
mod tests {
    // Test-local: the production paths in this module never read the plaintext back, so importing
    // this at module scope would be an unused import in a non-test build.
    use secrecy::ExposeSecret;

    use super::*;

    #[tokio::test]
    async fn generated_key_verifies_with_argon2id_hash() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20);
        let generated = hasher.generate("moira_sys").await.unwrap();

        assert!(
            hasher
                .verify(generated.raw_key.expose_secret(), &generated.key_hash)
                .await
                .unwrap()
        );
        assert!(!hasher.verify("wrong", &generated.key_hash).await.unwrap());
        assert_eq!(generated.key_prefix.len(), 20);
        assert_eq!(generated.pepper_version, "v1");
    }

    /// The bound is **measured**, not asserted against arithmetic repeated from the
    /// constant's own definition.
    ///
    /// A test that recomputed `namespace.len() + 1 + MIN_RANDOM_PREFIX_CHARS` would agree
    /// with the constant however wrong both were. This one generates a real key at the
    /// floor and counts what is actually left after the namespace — which is the quantity
    /// the uniqueness index and the preview's cost bound both depend on.
    #[tokio::test]
    async fn the_minimum_prefix_length_leaves_enough_random_material_for_every_namespace() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", MIN_API_KEY_PREFIX_LENGTH);
        for namespace in KEY_NAMESPACES {
            let generated = hasher.generate(namespace).await.expect("generate a key");
            let random = generated
                .key_prefix
                .strip_prefix(&format!("{namespace}_"))
                .unwrap_or_else(|| {
                    panic!("{namespace}: the prefix must still contain the whole namespace")
                });
            assert!(
                random.chars().count() >= MIN_RANDOM_PREFIX_CHARS,
                "{namespace}: the prefix retains only {} random characters, below the \
                 {MIN_RANDOM_PREFIX_CHARS} the unique index and the anonymous preview's \
                 cost bound both need",
                random.chars().count()
            );
        }
    }

    /// **`is_registered_key_namespace` must say no to something.**
    ///
    /// Found by `cargo mutants`: replacing the whole function with `true` survived the suite,
    /// and so did replacing `const_str_eq` with `true`. The only caller is a `const` assertion
    /// that a namespace *is* registered, so nothing anywhere exercised the negative — a
    /// membership test that answers yes to everything passes an "is this a member" assertion
    /// perfectly, and would silently stop protecting `MIN_API_KEY_PREFIX_LENGTH` from an
    /// unregistered namespace.
    ///
    /// The equal-length case is separate and deliberate: `const_str_eq`'s loop bound is what
    /// a mutation turned from `<` into `==`, which makes every same-length pair compare equal.
    /// `"moira_xyz"` is exactly as long as `"moira_sys"` and `"moira_inv"`, so it is the input
    /// that distinguishes a real comparison from a length check.
    #[test]
    fn an_unregistered_namespace_is_not_registered() {
        for namespace in KEY_NAMESPACES {
            assert!(
                is_registered_key_namespace(namespace),
                "{namespace} is in KEY_NAMESPACES and must be recognised"
            );
        }
        // Same length as the registered nine-character namespaces, different content.
        assert!(!is_registered_key_namespace("moira_xyz"));
        // A prefix of a registered one, and a registered one extended.
        assert!(!is_registered_key_namespace("moira_in"));
        assert!(!is_registered_key_namespace("moira_invx"));
        assert!(!is_registered_key_namespace(""));
        assert!(!is_registered_key_namespace("moira_cons2"));
    }

    /// The defect the floor now prevents, stated as a measurement rather than as prose.
    ///
    /// At the old `.max(12)` a `moira_inv` prefix retained two characters. Asserting the
    /// *number* here is what makes the change verifiable: 64² = 4096 is a search space, not
    /// a secret.
    #[test]
    fn the_old_clamp_left_a_guessable_prefix_and_the_new_floor_does_not() {
        let old_clamp = 12;
        let namespace = "moira_inv";
        let random_at_old_clamp = old_clamp - (namespace.len() + 1);
        assert_eq!(
            random_at_old_clamp, 2,
            "the historical clamp left two base64url characters — 4096 distinct prefixes"
        );
        assert!(
            MIN_API_KEY_PREFIX_LENGTH > old_clamp,
            "the floor must exceed the clamp it replaced"
        );
    }

    // ===================================================================================
    // The verification gate (issue #176).
    // ===================================================================================

    /// **The regression test for the defect itself**, and the reason it is written as a
    /// liveness assertion rather than a latency one.
    ///
    /// #176 was not "authentication is slow". It was "authentication occupies the runtime's
    /// worker threads, so nothing else can be polled" — health probes, in-flight SSE bodies and
    /// the retention worker all stall together once concurrent authenticated requests reach the
    /// worker count. A test that measured auth latency would pass just as happily with the
    /// Argon2 call back on a worker thread, which is exactly the reintroduction this has to
    /// catch.
    ///
    /// So: **one** worker thread, a gate of 1 so the verifications serialise, and an unrelated
    /// timer future that must keep firing throughout. With the fix the Argon2 work is on the
    /// blocking pool and the lone worker stays free to poll the ticker; with `verify` synchronous
    /// on the worker the ticker cannot be polled at all until every verification has finished.
    ///
    /// # The threshold is derived from the window, not written down
    ///
    /// An absolute tick count would be a machine-speed constant in disguise: measured here the
    /// fixed arrangement fires 37 ticks and the broken one fires **0**, so a literal `20` reads
    /// like a wide margin and would in fact go red on any machine twice as fast, for no reason
    /// connected to the defect. So the floor is a *fraction of the window that was actually
    /// measured* — the ticker must manage at least one tick per 8 ms of a window it nominally
    /// ticks through every 1 ms. That is an eighth of its nominal rate against zero, and it holds
    /// whatever the hardware does to the numerator and denominator together.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_unrelated_future_keeps_running_while_verifications_are_in_flight() {
        use std::sync::{
            Arc as StdArc,
            atomic::{AtomicU64, Ordering},
        };

        let hasher = StdArc::new(
            ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20)
                // Bound of 1, so the eight verifications below run one at a time: the window this
                // measures over is eight service times rather than one, which keeps it long
                // enough to be a measurement on fast hardware as well as slow.
                .with_verification_gate(1, Duration::from_secs(30), test_metrics()),
        );
        let generated = hasher.generate("moira_sys").await.expect("generate a key");
        let hash = StdArc::new(generated.key_hash);
        let secret = StdArc::new(generated.raw_key.expose_secret().to_string());

        let ticks = StdArc::new(AtomicU64::new(0));
        let ticker = tokio::spawn({
            let ticks = StdArc::clone(&ticks);
            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    ticks.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        let started = Instant::now();
        let mut verifications = Vec::new();
        for _ in 0..8 {
            let hasher = StdArc::clone(&hasher);
            let hash = StdArc::clone(&hash);
            let secret = StdArc::clone(&secret);
            verifications.push(tokio::spawn(async move {
                hasher.verify(&secret, &hash).await.expect("verify")
            }));
        }
        for verification in verifications {
            assert!(verification.await.expect("verification task"));
        }
        let window = started.elapsed();
        ticker.abort();

        let observed = ticks.load(Ordering::Relaxed);
        let floor = (window.as_millis() as u64 / 8).max(4);
        assert!(
            observed >= floor,
            "the single runtime worker fired an unrelated 1 ms timer {observed} times across a \
             {window:?} window of eight serialised Argon2id verifications, below the {floor} that \
             window can carry — the credential work is back on the runtime thread and #176 has \
             regressed"
        );
    }

    /// Two clones of one hasher draw on **one** budget.
    ///
    /// This is the likeliest way to ship a non-fix: `AppState::new` clones the hasher into
    /// `AuthService`, and if each half owned its own semaphore the bound would silently double
    /// with nothing failing. Asserted by holding the only permit through one clone and requiring
    /// the other to shed, which is deterministic — the racing-Argon2 version of this test would
    /// be a timing measurement.
    #[tokio::test]
    async fn two_clones_of_one_hasher_share_a_single_verification_budget() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20).with_verification_gate(
            1,
            Duration::from_millis(20),
            test_metrics(),
        );
        let generated = hasher.generate("moira_sys").await.expect("generate a key");
        let clone = hasher.clone();

        let held = hasher.hold_one_permit_for_test().await;
        let error = clone
            .verify(generated.raw_key.expose_secret(), &generated.key_hash)
            .await
            .expect_err("the clone must contend for the original's permit, not its own");
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);

        // And the sharing is real in both directions: releasing the permit the *original* holds
        // is what lets the *clone* proceed.
        drop(held);
        assert!(
            clone
                .verify(generated.raw_key.expose_secret(), &generated.key_hash)
                .await
                .expect("the clone proceeds once the shared permit is free")
        );
    }

    /// The permit is held for the **whole** Argon2 computation, not merely taken before it.
    ///
    /// # Why the other gate tests cannot see this
    ///
    /// Every one of them holds a permit from *outside*, through
    /// [`ApiKeyHasher::hold_one_permit_for_test`]. That proves the acquire happens and proves the
    /// budget is shared, and it says nothing whatsoever about the *release*. Rewriting
    /// `let _permit = permit;` inside the blocking closure to `let _ = permit;` — a one-character
    /// edit that compiles, reads like a deliberate discard, and is a well-worn Rust footgun —
    /// drops the permit on the closure's first line. Every other test in this module stays green,
    /// `verification_concurrency` still reports the configured bound, the metrics still count
    /// admissions, and the gate no longer bounds anything: concurrent Argon2 arenas go back to
    /// being limited only by tokio's 512-thread blocking pool, which is 9.7 GiB against a 2 GiB
    /// container. That is #176 restored while wearing a semaphore.
    ///
    /// # How this one sees it
    ///
    /// It contends against a verification that is genuinely *in flight*. The gate is waited down
    /// to zero free permits — bounded by a deadline, so a permit that is never taken fails with a
    /// message rather than hanging — and only then is the second verification issued. It must
    /// shed.
    ///
    /// Under the broken variant the test reds either way: the free-permit count returns to 1
    /// immediately, so either the wait never observes zero and trips its deadline, or it observes
    /// the sliver between acquire and closure entry and the second verification then succeeds
    /// where a shed was required.
    ///
    /// # The one inequality this rests on
    ///
    /// A 5 ms shed timeout against an Argon2id operation at `m=19456`, `t=2`, which costs tens of
    /// milliseconds. That is three orders of margin on the wrong side of nothing, and it is not a
    /// hardware assumption: if Argon2 here ever completes inside 5 ms the credential parameters
    /// have been weakened, and this test going red is the correct alarm rather than a flake.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_verification_holds_its_permit_for_the_whole_computation() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20).with_verification_gate(
            1,
            Duration::from_millis(5),
            test_metrics(),
        );
        // Minted before the contention starts: `generate` draws on the same single permit, so
        // doing this later would deadlock against the verification below rather than test it.
        let generated = hasher.generate("moira_sys").await.expect("generate a key");
        let hash = generated.key_hash.clone();
        let secret = generated.raw_key.expose_secret().to_string();

        let in_flight = tokio::spawn({
            let hasher = hasher.clone();
            let hash = hash.clone();
            let secret = secret.clone();
            async move { hasher.verify(&secret, &hash).await }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while hasher.free_permits_for_test() > 0 {
            assert!(
                Instant::now() < deadline,
                "the in-flight verification never held the gate's only permit for as long as it \
                 took to observe — the permit is being released before the Argon2 work it \
                 accounts for, so the bound is not a bound"
            );
            tokio::task::yield_now().await;
        }

        let error = hasher
            .verify(&secret, &hash)
            .await
            .expect_err("a verification running under the only permit must make the next one shed");
        assert_eq!(
            error.error_response(None).error.code,
            "auth_verification_overloaded",
            "the second verification was admitted while the first was still computing, so the \
             permit does not span the Argon2 work and the gate bounds nothing"
        );

        assert!(
            in_flight
                .await
                .expect("the in-flight verification task")
                .expect("the in-flight verification"),
            "the verification that held the permit must still succeed"
        );
    }

    /// A saturated gate returns the coded `503` and **never** `Ok`.
    ///
    /// `Ok(false)` is the tempting shortcut and is the dangerous one: it answers "invalid
    /// credential" about a credential that was never checked, so an overload reaches the operator
    /// as a `401` they debug as a client bug. The code is asserted too, because the status alone
    /// does not distinguish this from `DatabaseUnavailable`.
    #[tokio::test]
    async fn a_saturated_gate_sheds_with_the_coded_503_rather_than_answering() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20).with_verification_gate(
            1,
            Duration::from_millis(20),
            test_metrics(),
        );
        let generated = hasher.generate("moira_sys").await.expect("generate a key");

        let _held = hasher.hold_one_permit_for_test().await;
        let error = hasher
            .verify(generated.raw_key.expose_secret(), &generated.key_hash)
            .await
            .expect_err("a saturated gate must refuse, never answer");
        let rendered = error.error_response(None);
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(rendered.error.code, "auth_verification_overloaded");
        assert_eq!(
            rendered.error.message_key,
            "moira.error.auth_verification_overloaded"
        );

        // The mint path takes the same permit from the same gate, and sheds the same way.
        let error = hasher
            .generate("moira_sys")
            .await
            .expect_err("a saturated gate must refuse a mint too");
        assert_eq!(
            error.error_response(None).error.code,
            "auth_verification_overloaded"
        );
    }

    /// A stored hash asking for more memory than the bound allows is refused **before** a permit
    /// is spent.
    ///
    /// The arena size comes from the stored PHC string, not from `Argon2::default()`, so this is
    /// the one input that can make the gate's memory arithmetic fiction. Two properties are
    /// asserted together, and the second is what makes it more than a parser test: the refusal
    /// happens with the gate's only permit already held, so it cannot have queued for one.
    #[tokio::test]
    async fn a_stored_hash_above_the_memory_ceiling_is_refused_without_spending_a_permit() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20).with_verification_gate(
            1,
            Duration::from_millis(20),
            test_metrics(),
        );
        let oversized = oversized_hash();

        let _held = hasher.hold_one_permit_for_test().await;
        let error = hasher
            .verify("moira_sys_whatever", &oversized)
            .await
            .expect_err("a hash above the ceiling must be refused");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            error.to_string().contains("above the"),
            "the refusal must name the ceiling it enforced: {error}"
        );

        // Fail closed: it is an error, not a silent `Ok(false)` that would read as "wrong key".
        assert_ne!(error.error_response(None).error.code, "unauthorized");
    }

    /// Every hash this process writes verifies inside the ceiling the previous test enforces.
    ///
    /// The ceiling is only safe if the writer stays under it. Asserted against a freshly minted
    /// hash rather than against `Argon2::default()`'s constants, so a future change to
    /// `ApiKeyHasher::hash` that raised `m` would make every existing row unverifiable and be
    /// caught here rather than in production.
    #[tokio::test]
    async fn the_hashes_this_process_writes_stay_inside_the_verification_memory_ceiling() {
        let hasher = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20);
        let generated = hasher.generate("moira_sys").await.expect("generate a key");

        assert_eq!(
            stored_m_cost_kib(&generated.key_hash).expect("read the stored parameters"),
            MAX_VERIFY_M_COST_KIB,
            "hash and verify must agree on the arena size, or the ceiling refuses this \
             process's own rows"
        );
        assert_eq!(ARGON2_ARENA_BYTES, 19_922_944);
    }

    /// The derived bound is a *CPU* decision, and its clamps are the memory backstop.
    #[test]
    fn the_derived_verification_bound_stays_inside_its_clamps() {
        let derived = default_verification_concurrency();
        assert!(
            (1..=MAX_DERIVED_VERIFICATION_CONCURRENCY).contains(&derived),
            "the derived bound {derived} escaped [1, {MAX_DERIVED_VERIFICATION_CONCURRENCY}]"
        );

        // A zero from a direct library construction must not produce a semaphore nothing can
        // ever acquire from — that would wedge every request in the process rather than fail.
        let wedged = ApiKeyHasher::new(b"pepper".to_vec(), "v1", 20).with_verification_gate(
            0,
            Duration::from_millis(1),
            test_metrics(),
        );
        assert_eq!(wedged.verification_concurrency(), 1);

        // And the upper clamp is the memory budget, not a taste: at the ceiling the peak arena
        // must still fit inside the 2 GiB the shipped chart limits the container to.
        let peak = MAX_VERIFICATION_CONCURRENCY * ARGON2_ARENA_BYTES;
        assert!(
            peak < 2 * 1024 * 1024 * 1024,
            "{MAX_VERIFICATION_CONCURRENCY} x {ARGON2_ARENA_BYTES} B is {peak} B, at or above \
             the chart's resources.limits.memory of 2Gi"
        );
    }

    /// The pepper must stay out of `Debug` now that the struct carries more fields, and the two
    /// new ones must be *in* it — an operator diagnosing a shed needs the bound and the timeout.
    #[test]
    fn debug_renders_the_gate_and_still_redacts_the_pepper() {
        let hasher = ApiKeyHasher::new(b"pepper-that-must-not-appear".to_vec(), "v1", 20)
            .with_verification_gate(3, Duration::from_millis(175), test_metrics());
        let rendered = format!("{hasher:?}");

        assert!(
            !rendered.contains("pepper-that-must-not-appear"),
            "{rendered}"
        );
        assert!(
            rendered.contains("verification_concurrency: 3"),
            "{rendered}"
        );
        assert!(rendered.contains("175ms"), "{rendered}");
    }

    fn test_metrics() -> MetricsRegistry {
        MetricsRegistry::new("moira-test", None)
    }

    /// A syntactically valid Argon2id PHC string whose `m` is above [`MAX_VERIFY_M_COST_KIB`].
    ///
    /// Built by hashing at a *small* `m` and rewriting the parameter, rather than by actually
    /// hashing at 64 MiB: the point under test is that the ceiling is read and refused before any
    /// arena is allocated, so allocating one to build the fixture would be self-defeating.
    fn oversized_hash() -> String {
        let params = Params::new(8, 1, 1, None).expect("small test parameters");
        let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
        let salt = SaltString::generate(&mut PasswordOsRng);
        let hash = argon2
            .hash_password(b"whatever", &salt)
            .expect("hash at the small parameters")
            .to_string();
        let oversized = MAX_VERIFY_M_COST_KIB + 1;
        let rewritten = hash.replace("m=8,", &format!("m={oversized},"));
        assert_ne!(rewritten, hash, "the fixture failed to rewrite `m`: {hash}");
        rewritten
    }

    /// Every `generate` call site in `src/` must name a registered namespace.
    ///
    /// [`MIN_API_KEY_PREFIX_LENGTH`] is derived from the longest entry in
    /// [`KEY_NAMESPACES`], so an *unregistered* namespace longer than `moira_cons` would
    /// silently push the random tail below [`MIN_RANDOM_PREFIX_CHARS`] with every gate
    /// still green — the constant would be right about the list and wrong about the tree.
    /// This is the same walking technique, and the same class of blind spot, as
    /// `every_coded_error_literal_in_src_has_a_catalog_entry`.
    #[test]
    fn every_generate_call_site_names_a_registered_namespace() {
        // Assembled with `concat!` because this file is itself walked: a literal spelled
        // out here would be found in its own source and parsed as a call site.
        const NEEDLE: &str = concat!(".generate", "(\"");

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_sources_under(&manifest.join("src"), &mut files);
        assert!(
            files.len() > 20,
            "the source walker found only {} files under src/ — a broken walker asserts nothing",
            files.len()
        );

        let mut found = Vec::new();
        for file in &files {
            let source = std::fs::read_to_string(file)
                .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
            let relative = file
                .strip_prefix(manifest)
                .unwrap_or(file)
                .display()
                .to_string();
            let mut cursor = 0usize;
            while let Some(offset) = source[cursor..].find(NEEDLE) {
                let start = cursor + offset + NEEDLE.len();
                cursor = start;
                let Some(end) = source[start..].find('"') else {
                    continue;
                };
                found.push((relative.clone(), source[start..start + end].to_string()));
            }
        }

        // Vacuity guard: the namespaces reach `generate` through a `const` at one of the
        // three sites, so this counts the ones spelled inline. Zero means the needle
        // stopped matching, which would make the assertion below prove nothing.
        assert!(
            !found.is_empty(),
            "no `generate(\"…\")` call site was found in src/ — the needle has drifted"
        );

        let unregistered: Vec<String> = found
            .iter()
            .filter(|(_, namespace)| !KEY_NAMESPACES.contains(&namespace.as_str()))
            .map(|(file, namespace)| format!("{namespace:?} in {file}"))
            .collect();
        assert!(
            unregistered.is_empty(),
            "these key namespaces are not in KEY_NAMESPACES, so MIN_API_KEY_PREFIX_LENGTH \
             was not computed with them in mind: {unregistered:?}"
        );
    }

    /// Every `.rs` file under `src/`, sorted and depth-first so a failure names the same
    /// file on every machine.
    fn rust_sources_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                rust_sources_under(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
}
