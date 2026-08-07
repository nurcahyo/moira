use std::{collections::HashMap, net::SocketAddr, path::Path};

use axum::http::HeaderValue;
use base64::{Engine, engine::general_purpose::STANDARD};
use config::{Config, Environment, File};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::error::AppError;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub deployment: DeploymentSettings,
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub database: DatabaseSettings,
    #[serde(default)]
    pub secrets: SecretSettings,
    #[serde(default)]
    pub content_encryption: ContentEncryptionSettings,
    #[serde(default)]
    pub auth: AuthSettings,
    #[serde(default)]
    pub api_keys: ApiKeySettings,
    #[serde(default)]
    pub idempotency: IdempotencySettings,
    #[serde(default)]
    pub provider_security: ProviderSecuritySettings,
    #[serde(default)]
    pub cors: CorsSettings,
    #[serde(default)]
    pub docs: DocsSettings,
    #[serde(default)]
    pub cache: CacheSettings,
    #[serde(default)]
    pub runtime: RuntimeSettings,
    #[serde(default)]
    pub public_api: PublicApiSettings,
    #[serde(default)]
    pub redis: RedisSettings,
    #[serde(default)]
    pub cluster: ClusterSettings,
    #[serde(default)]
    pub workers: WorkerSettings,
    #[serde(default)]
    pub telemetry: TelemetrySettings,
    #[serde(default)]
    pub rag: RagSettings,
}

/// Bounds on RAG document ingestion (plan 11, Sub-Phase A).
///
/// Deployment-wide rather than per-application: these are resource ceilings that protect the
/// database and the embedding provider from a single oversized document, not a product policy
/// an application should be able to raise for itself.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct RagSettings {
    /// Maximum characters in one chunk.
    pub max_chunk_chars: usize,
    /// Maximum chunks one document version may produce. Exceeding it is a `422`
    /// `rag_document_too_large`, never a silent truncation — a truncated document is a
    /// retrieval index that is quietly incomplete.
    pub max_chunks_per_document: usize,
}

impl Default for RagSettings {
    fn default() -> Self {
        Self {
            // ~1000 characters is roughly 250 estimated tokens, comfortably inside every
            // embedding model's per-input ceiling while still being large enough that a
            // paragraph usually survives intact.
            max_chunk_chars: 1_000,
            // 2000 chunks * 1000 chars caps one version at ~2 MB of chunk text, which is the
            // same order as `ADMIN_BODY_LIMIT_BYTES` (2 MiB) — the largest body that can reach
            // the admin ingest route in the first place.
            max_chunks_per_document: 2_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentEnvironment {
    #[default]
    Development,
    Test,
    Production,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMode {
    Serve,
    Migrate,
    BootstrapSystemKey,
    ExecuteTest,
    /// `moira keyring …` — the content data key rotation verbs
    /// (`docs/decision-encryption-at-rest.md` §9).
    ///
    /// A process mode rather than an admin HTTP route, deliberately: these verbs need the
    /// database and key custody and nothing else, so an endpoint would add an authorization
    /// design, an OpenAPI contract and a public error taxonomy to an operation run by a human
    /// with shell access at a rate of single digits per year.
    Keyring,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeploymentSettings {
    pub environment: DeploymentEnvironment,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    pub url: Option<String>,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout_seconds: u64,
    pub require: bool,
    pub migrate_on_startup: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecretSettings {
    pub master_key_base64: Option<String>,
    pub key_id: String,
    pub allow_insecure_dev_key: bool,
}

/// Where the master key that wraps a content data key is held.
///
/// `Kms` and `Vault` are deliberately absent rather than present-and-unimplemented: an operator
/// who sets a variant this build cannot serve gets a deserialisation refusal naming the accepted
/// values, not a process that boots and then cannot wrap.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentEncryptionCustody {
    #[default]
    Environment,
}

/// Envelope-encryption configuration for the content columns
/// (`docs/decision-encryption-at-rest.md` §5).
///
/// **A new block, deliberately not reusing `secrets.master_key_base64`.** Provider credentials
/// and user content have different retention and different blast radius, [`SecretSettings`]
/// cannot express a key *list*, and changing it would change `provider_credentials` semantics.
/// **An implicit fallback — "if `keys` is unset, use `secrets.master_key_base64`" — is rejected
/// outright**: that is the silently-inert-configuration class this release exists to avoid.
///
/// `Debug` is hand-written for the same reason [`ApiKeySettings`]'s is: `keys` holds base64
/// master-key material and this struct is reachable from [`Settings`], which derives `Debug`.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct ContentEncryptionSettings {
    pub custody: ContentEncryptionCustody,
    /// `"id:base64,id:base64"`. Neither `,` nor `:` is in the base64 alphabet, so
    /// `split_once(':')` is unambiguous.
    ///
    /// Every key an existing row might have been sealed under stays listed here through a
    /// rotation; that is what makes previously-wrapped data keys readable with no re-encryption.
    pub keys: String,
    /// The id new data keys are wrapped under. Must be one of [`Self::keys`].
    pub active_key_id: String,
    /// Development-only fallback to the built-in sentinel when [`Self::keys`] is empty.
    /// Production requires it `false`, and registers as `insecure_content_encryption_key` in
    /// [`Settings::unsafe_development_features`] while it is on.
    pub allow_insecure_dev_key: bool,
    /// How often the keyring is re-read from the database, once there is a keyring to re-read.
    pub refresh_seconds: u64,
    /// Floor under [`Self::refresh_seconds`]. It exists to stop a refresh interval being
    /// configured into a hot loop against `content_data_keys`, and `Settings::validate`
    /// enforces it — an unenforced floor is an inert setting.
    pub min_refresh_seconds: u64,
    /// Age past which the active data key is reported as overdue for rotation.
    pub data_key_rotation_days: u32,
}

impl std::fmt::Debug for ContentEncryptionSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The *ids* are rendered and the material never is: which keys are loaded is exactly
        // what an operator reads a startup line for, and it is not a secret.
        let key_ids: Vec<&str> = self
            .keys
            .split(',')
            .filter_map(|entry| entry.split_once(':'))
            .map(|(id, _)| id.trim())
            .collect();
        f.debug_struct("ContentEncryptionSettings")
            .field("custody", &self.custody)
            .field("key_ids", &key_ids)
            .field("active_key_id", &self.active_key_id)
            .field("allow_insecure_dev_key", &self.allow_insecure_dev_key)
            .field("refresh_seconds", &self.refresh_seconds)
            .field("min_refresh_seconds", &self.min_refresh_seconds)
            .field("data_key_rotation_days", &self.data_key_rotation_days)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthSettings {
    #[serde(default)]
    pub admin: JwtAuthSettings,
    #[serde(default)]
    pub caller: CallerAuthSettings,
    /// Shared JWKS fetch policy. One instance, deliberately: `AdminAuthenticator`,
    /// `CallerAuthenticator` and the trusted-issuer path must not be able to drift
    /// apart into three divergent SSRF postures.
    #[serde(default)]
    pub jwks: JwksFetchSettings,
    /// Backstop staleness bound on the in-process auth-provider settings cache
    /// (`MOIRA_AUTH__PROVIDER_SETTINGS_CACHE_TTL_SECONDS`).
    ///
    /// It is **not** the invalidation mechanism: the `moira_runtime_config` NOTIFY trigger
    /// on `auth_provider_settings` clears the cache on every instance the moment a write
    /// commits (CONVENTIONS §7.2). This TTL only bounds how long a stale entry could
    /// survive a *lost* notification — for instance across a listener reconnect — so it is
    /// a safety net rather than a tuning knob, and it is set well above the interval any
    /// operator would notice.
    ///
    /// This is an infrastructure knob and correctly lives in settings. The auth-method
    /// policy itself — including `allowed_email_domains` — lives in the **database**, per
    /// CONVENTIONS §7.2, so that plan 08's setup wizard can configure it.
    #[serde(default = "default_auth_provider_settings_cache_ttl_seconds")]
    pub provider_settings_cache_ttl_seconds: u64,
}

fn default_auth_provider_settings_cache_ttl_seconds() -> u64 {
    300
}

/// SSRF / resource limits applied to every outbound JWKS fetch.
///
/// `#[serde(default)]` on the container so an operator may override a single knob
/// (e.g. `MOIRA_AUTH__JWKS__TIMEOUT_MS`) without having to restate the others.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JwksFetchSettings {
    /// Hard cap on the number of response body bytes read from a JWKS endpoint.
    /// Enforced by streaming, not by trusting `Content-Length`.
    pub max_response_bytes: usize,
    /// Per-request timeout for a JWKS fetch. Deliberately per-request: the shared
    /// `reqwest::Client` also serves provider execution calls whose timeout
    /// semantics belong to the execution-deadline system.
    pub timeout_ms: u64,
    /// Dev-only escape hatch permitting `http://` and private/loopback/link-local
    /// JWKS URLs. MUST stay `false` outside development; `Settings::validate`
    /// hard-fails production when it is `true`.
    pub allow_insecure_dev_urls: bool,
    /// Hard ceiling on how long a cached JWKS may keep authenticating callers after it
    /// stopped being refreshable.
    ///
    /// The cache serves a last-known-good key set when a refresh fails, so a transient IdP
    /// outage does not break authentication. Without a ceiling that fallback has no end: an
    /// IdP that stays unreachable — or a `jwks_url` that has been permanently decommissioned
    /// — leaves a key set verifying tokens for as long as the process lives. That converts
    /// key rotation and key *revocation* at the IdP into advisory operations, because the
    /// retired key keeps being accepted here.
    ///
    /// Past this bound the entry is evicted and authentication fails closed rather than on a
    /// key nobody can vouch for any more. The default trades availability for that bound
    /// deliberately: 24 hours is far longer than any transient outage the retention exists to
    /// absorb, and far shorter than "forever".
    pub max_stale_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwtAuthSettings {
    pub enabled: bool,
    pub jwks_url: Option<String>,
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub required_scope: Option<String>,
    pub leeway_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallerAuthSettings {
    pub enabled: bool,
    pub jwks_url: Option<String>,
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub leeway_seconds: u64,
    pub dev_trust_headers: bool,
}

/// `Debug` is hand-written: `pepper_base64` is the API-key pepper exactly as configured, and
/// this struct is reachable from `Settings`, which does derive `Debug`. Redacting the hasher
/// alone would leave the same secret printable one field earlier in its life.
#[derive(Clone, Deserialize)]
pub struct ApiKeySettings {
    pub pepper_base64: Option<String>,
    pub pepper_version: String,
    pub allow_insecure_dev_pepper: bool,
    pub prefix_length: usize,
}

impl std::fmt::Debug for ApiKeySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeySettings")
            .field(
                "pepper_base64",
                &redacted_optional_pepper(&self.pepper_base64),
            )
            .field("pepper_version", &self.pepper_version)
            .field("allow_insecure_dev_pepper", &self.allow_insecure_dev_pepper)
            .field("prefix_length", &self.prefix_length)
            .finish()
    }
}

/// Renders a configured pepper for `Debug` without disclosing it.
///
/// Whether a pepper is *set* is operationally important — the dev-fallback warnings and
/// `Settings::validate` both turn on it — and is not itself a secret, so `None` and `Some`
/// stay distinguishable while the value never is. The length is not rendered either: it
/// bounds a brute force.
fn redacted_optional_pepper(pepper: &Option<String>) -> &'static str {
    match pepper {
        Some(_) => "[redacted]",
        None => "None",
    }
}

/// How long a successfully fetched JWKS stays *fresh*, in seconds.
///
/// Lives here rather than in `security::auth` because `Settings::validate` has to reason
/// about it: `auth.jwks.max_stale_seconds` is a ceiling on *retention past freshness*, so a
/// ceiling below this constant is not a stricter policy, it is an unenforceable one. The
/// fresh read path (`JwksCache::fresh`) serves any entry inside this window on a read lock
/// without consulting the ceiling at all, so an entry aged between `max_stale_seconds` and
/// this value would keep authenticating callers past the bound an operator thought they set.
///
/// `security::auth::JWKS_CACHE_TTL` is derived from this value, and
/// `the_jwks_freshness_window_matches_the_cache_ttl` fails if the two ever drift apart.
pub const JWKS_FRESHNESS_SECONDS: u64 = 300;

/// Widest value the idempotency ledger accepts: `idempotency_records.request_hash`,
/// `idempotency_records.idempotency_key_hash` and every `content_hash` column are
/// `varchar(128)` (`migrations/0003_security_foundation.sql`). A stored hash wider than
/// this is rejected by PostgreSQL with `value too long for type character varying(128)`,
/// which surfaces as a `500` on every idempotent write.
pub const IDEMPOTENCY_HASH_MAX_LENGTH: usize = 128;

/// Everything `IdempotencyHasher::hash` appends after the version tag:
/// `":" + base64url_no_pad(hmac_sha256(...))` = 1 + 43 characters, fixed-width because
/// HMAC-SHA-256 always produces 32 bytes.
///
/// This arithmetic is not trusted on its own —
/// `idempotency_pepper_version_bound_is_driven_by_the_hasher` derives the real width from
/// `IdempotencyHasher::hash` and fails if this constant ever drifts from it.
const IDEMPOTENCY_HASH_SUFFIX_LENGTH: usize = 44;

/// Longest `idempotency.pepper_version` whose hashes still fit the `varchar(128)` ledger
/// columns (plan 03 finding F1).
///
/// Measured in **bytes**, which is deliberately stricter than PostgreSQL's `varchar(128)`
/// (that counts characters): for any non-ASCII version tag the byte length is the larger
/// of the two, so satisfying this bound satisfies the column too.
pub const IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH: usize =
    IDEMPOTENCY_HASH_MAX_LENGTH - IDEMPOTENCY_HASH_SUFFIX_LENGTH;

/// Dedicated pepper for the keyed (HMAC-SHA-256) idempotency / request-body hash.
///
/// Mirrors `ApiKeySettings`'s pepper contract exactly. `#[serde(default)]` on the
/// container so setting only `MOIRA_IDEMPOTENCY__PEPPER_BASE64` is a valid
/// configuration — the remaining fields fall back to `Default`.
///
/// `Debug` is hand-written for the same reason as [`ApiKeySettings`]: `pepper_base64` is the
/// idempotency pepper as configured, and `Settings` derives `Debug`.
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct IdempotencySettings {
    pub pepper_base64: Option<String>,
    /// Version tag prefixed to every produced hash (`"{pepper_version}:{digest}"`).
    ///
    /// Must be non-empty, must not contain `:` (the format separator), and must be at
    /// most [`IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH`] bytes so the produced hash fits the
    /// `varchar(128)` ledger columns. All three are *correctness* invariants of the hash
    /// format, so `Settings::validate` enforces them in **every** environment, not only in
    /// production.
    pub pepper_version: String,
    pub allow_insecure_dev_pepper: bool,
    /// Whether ledger values written **before** the HMAC switch (plan 03, P1-1) — plain,
    /// unkeyed SHA-256 with no `"{version}:"` prefix — are still accepted on read.
    ///
    /// # Why this exists
    ///
    /// The dual-read path is a *migration* affordance, not a permanent feature: while it
    /// is on, an attacker who can write a row (or who holds a pre-switch digest) is
    /// matched against an unkeyed hash, and every idempotent read pays an extra lookup by
    /// the legacy key hash. Nothing in the code ends that window on its own, so it is a
    /// setting rather than a comment.
    ///
    /// # Operational procedure
    ///
    /// 1. Deploy the HMAC switch with this left at its default `true`. From that moment
    ///    every **new** row is written in the versioned format; legacy rows keep replaying.
    /// 2. Wait one full idempotency retention period — 24 hours
    ///    (`IDEMPOTENCY_RETENTION_HOURS`) — after that deploy is fully rolled out. Every
    ///    pre-switch row has then passed its `expires_at` and is swept.
    /// 3. Set `MOIRA_IDEMPOTENCY__ACCEPT_LEGACY_HASHES=false` and redeploy. The unkeyed
    ///    verification arm and the extra legacy lookup are both skipped from then on.
    ///
    /// Flipping it to `false` early is safe in the security direction and unsafe only in
    /// the duplicate-processing direction: an unexpired pre-switch claim stops
    /// replay-matching and falls through to normal, non-idempotent processing — the same
    /// bounded window documented for pepper rotation. Flipping it back to `true` restores
    /// the old behaviour with no data change.
    pub accept_legacy_hashes: bool,
}

impl std::fmt::Debug for IdempotencySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdempotencySettings")
            .field(
                "pepper_base64",
                &redacted_optional_pepper(&self.pepper_base64),
            )
            .field("pepper_version", &self.pepper_version)
            .field("allow_insecure_dev_pepper", &self.allow_insecure_dev_pepper)
            .field("accept_legacy_hashes", &self.accept_legacy_hashes)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderSecuritySettings {
    pub allow_private_provider_urls: bool,
    pub allow_http_provider_urls: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorsSettings {
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DocsSettings {
    pub expose_admin: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CacheSettings {
    pub runtime_config_ttl_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSettings {
    pub diagnostic_endpoint_enabled: bool,
    pub maximum_eligible_model_candidates: usize,
    pub maximum_provider_fallback_candidates: usize,
    pub maximum_retries_per_candidate: usize,
    pub maximum_total_upstream_attempts: usize,
    pub default_execution_timeout_seconds: u64,
    pub maximum_execution_timeout_seconds: u64,
    pub global_execution_concurrency: usize,
    pub application_execution_concurrency: usize,
    pub external_user_execution_concurrency: usize,
    pub runtime_cache_max_entries: usize,
    pub runtime_cache_ttl_seconds: u64,
    pub internal_stream_queue_capacity: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicApiSettings {
    pub openai_responses_compat_enabled: bool,
    pub chat_completions_compat_enabled: bool,
    pub default_persistence_mode: String,
    pub maximum_request_bytes: i64,
    pub maximum_input_items: i32,
    pub maximum_messages: usize,
    pub maximum_content_parts_per_message: usize,
    pub maximum_text_part_bytes: usize,
    pub maximum_image_count: usize,
    pub maximum_tool_count: usize,
    pub maximum_metadata_bytes: usize,
    pub maximum_metadata_keys: usize,
    pub maximum_metadata_depth: usize,
    pub maximum_metadata_key_bytes: usize,
    pub maximum_metadata_string_bytes: usize,
    pub maximum_schema_bytes: usize,
    pub heartbeat_seconds: u64,
    pub rate_limiter_max_entries: usize,
    /// SSRF policy for caller-supplied image URLs.
    ///
    /// `#[serde(default)]` on [`ImageUrlSettings`] itself, so a config file written
    /// before this section existed still deserializes into the hardened defaults.
    #[serde(default)]
    pub image_urls: ImageUrlSettings,
}

/// SSRF policy applied to every `input_image.image_url` in a public request.
///
/// **This path validates a URL it does not fetch.** The image URL is handed to the
/// provider (`UserContent::image_url`), and the provider is what performs the request.
/// That makes the guard *admission control*, not a fetch policy, and it is why this
/// section has no content-type or response-size knob: there is no response for Moira to
/// inspect. The controls that survive the hand-off are the address-space rules and the
/// allow-list.
///
/// `#[serde(default)]` on the container so an operator may override one knob without
/// restating the others.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ImageUrlSettings {
    /// Budget for resolving a single image hostname.
    ///
    /// Tighter than the JWKS budget by default and deliberately so: a JWKS URL is
    /// admin-configured, resolved behind a singleflight lock and cached, whereas these
    /// are caller-supplied, uncached, and there can be `maximum_image_count` of them in
    /// one request.
    pub dns_timeout_ms: u64,
    /// Ceiling on the time spent resolving *all* image hosts in one request.
    ///
    /// Without it the worst case is `maximum_image_count * dns_timeout_ms`, which turns a
    /// single request into a cheap way to occupy a connection and a blocking-pool thread
    /// per image. The JWKS path needs no equivalent because it validates exactly one URL.
    pub total_validation_timeout_ms: u64,
    /// Optional egress allow-list of exact hostnames. Empty (the default) means any host
    /// that passes the address-range rules is accepted.
    ///
    /// A deployment that knows which origins its images come from should set this: it is
    /// the only control that still binds when the provider follows a redirect Moira never
    /// sees. See `OutboundUrlPolicy::allowed_hosts`.
    ///
    /// Matching is on the host alone — not the port or path — and against the URL crate's
    /// normalised host, so an internationalised domain must be listed in its punycode form
    /// (`xn--mnchen-3ya.example`, not `münchen.example`). Case is ignored. A subdomain is
    /// **not** implied by its parent: list every host you intend to permit.
    pub allowed_hosts: Vec<String>,
    /// Dev-only escape hatch permitting `http://` and private/loopback/link-local image
    /// URLs. MUST stay `false` outside development; `Settings::validate` hard-fails
    /// production when it is `true`.
    ///
    /// Separate from `auth.jwks.allow_insecure_dev_urls` on purpose — see
    /// `OutboundUrlPolicy::allow_insecure`.
    pub allow_insecure_dev_urls: bool,
}

impl Default for ImageUrlSettings {
    fn default() -> Self {
        Self {
            dns_timeout_ms: 1_000,
            total_validation_timeout_ms: 3_000,
            allowed_hosts: Vec::new(),
            allow_insecure_dev_urls: false,
        }
    }
}

/// The optional Redis coordination client's configuration.
///
/// `#[serde(default)]` on the container, same convention as [`WorkerSettings`]
/// and [`ClusterSettings`]: an operator turns Redis on with a single
/// `MOIRA_REDIS__ENABLED` without restating the timing knobs, and a config file
/// written before `permit_ttl_seconds` existed still deserializes.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RedisSettings {
    /// **Default `false`, and plan 10 §0.4b makes that a decision rather than a
    /// starting point.** Postgres and per-process memory are the coordination
    /// path Moira ships with; Redis is additive and buys exactly two things —
    /// cluster-wide rate-limit windows and cluster-wide concurrency counters.
    ///
    /// The honest cost of leaving it off is recorded on
    /// [`RuntimeSettings::global_execution_concurrency`] and
    /// [`PublicApiSettings::rate_limit_per_minute`]: with Redis off those limits
    /// are **per replica**, so N replicas admit up to N times the configured
    /// value. That is tolerable only because `cluster.max_replicas` bounds N.
    pub enabled: bool,
    pub url: Option<String>,
    pub namespace: String,
    pub connect_timeout_seconds: u64,
    pub invalidation_channel: String,
    /// How long an unrefreshed cluster-wide concurrency counter survives.
    ///
    /// This is a **leak bound**, not an expiry policy. An in-memory permit is
    /// released by `Drop`, and a process that dies releases everything it held by
    /// ceasing to exist; a Redis counter has no such guarantee, so a replica
    /// killed mid-request would hold its slot forever. The TTL is refreshed on
    /// every acquire against the same key, so a busy counter never expires under
    /// live work — but it **must exceed the longest execution**, or a long stream
    /// will have its slot reclaimed while it is still running and the ceiling
    /// will drift above the configured limit.
    ///
    /// Default 900s (15 minutes), comfortably above
    /// `runtime.maximum_execution_timeout_seconds`.
    pub permit_ttl_seconds: u64,
}

/// Multi-replica admission (plan 10, finding P3-2).
///
/// `#[serde(default)]` on the container for the same reason as
/// [`WorkerSettings`]: an operator sets `MOIRA_CLUSTER__MAX_REPLICAS` without
/// having to restate the timing knobs, and a config file written before this
/// section existed still deserializes.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClusterSettings {
    /// Master switch for the Postgres `cluster_replica_leases` admission gate.
    ///
    /// **Default `false`, and that is not timidity.** With the gate on and
    /// `max_replicas` at its default of 1, a Kubernetes rolling update that
    /// surges a second pod before terminating the first deadlocks: the surge pod
    /// is denied a lease and crash-loops, so the old pod — which still holds the
    /// lease — is never told to terminate. The chart pairs enabling this with
    /// `strategy.rollingUpdate.maxSurge: 0`, which is the only configuration
    /// where the two agree. Turning it on without that pairing is the failure
    /// mode this default exists to keep out of a default install.
    pub admission_enabled: bool,
    /// The ceiling the *database* admits, independent of what Helm allows and of
    /// what `kubectl scale` asks for.
    pub max_replicas: u32,
    /// How often a live replica renews `cluster_replica_leases.heartbeat_at`.
    pub lease_heartbeat_seconds: u64,
    /// How stale a heartbeat may get before another replica may reclaim the
    /// lease. Must exceed `lease_heartbeat_seconds` — see
    /// [`Settings::validate`]; an expiry at or below the heartbeat interval
    /// means every replica continuously reclaims every other replica's lease.
    pub lease_expiry_seconds: u64,
}

/// Background-worker tuning.
///
/// `#[serde(default)]` sits on the container (same convention as
/// [`JwksFetchSettings`]) so an operator may override a single knob — e.g.
/// `MOIRA_WORKERS__RETENTION_BATCH_SIZE` — without restating the others, and so
/// config files written before the retention knobs existed still deserialize.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkerSettings {
    pub enabled: bool,
    pub shutdown_grace_seconds: u64,
    pub max_concurrent_jobs: usize,
    pub retry_base_delay_seconds: u64,
    pub retry_max_delay_seconds: u64,
    pub dead_letter_retention_hours: u64,
    /// Rows deleted per batch by the retention worker
    /// (`src/infra/workers/retention.rs`). Small enough that a single batch
    /// holds row locks only briefly, large enough that a busy deployment drains
    /// its expired backlog. Clamped at runtime to
    /// `1..=RetentionPlan::MAX_BATCH_SIZE`.
    pub retention_batch_size: usize,
    /// Seconds between retention sweeps. Deliberately independent of
    /// `retry_base_delay_seconds`, which drives the base supervisor tick — a
    /// retention sweep is far more expensive than a tick and must not run at
    /// tick cadence. `0` is treated as `1` at runtime.
    pub retention_interval_seconds: u64,
    /// Whether the supervisor takes a leader lock before running a singleton
    /// job (today: the retention sweep).
    ///
    /// `None` — the default — **mirrors `cluster.admission_enabled`**, which is
    /// what makes "turn on multi-replica mode" one switch rather than two that
    /// can silently disagree. Set it explicitly to pin it either way; resolved
    /// by [`Settings::leader_election_enabled`], which is the only thing that
    /// should read this field.
    pub leader_election_enabled: Option<bool>,
    /// How often each replica polls `worker_jobs` for claimable work.
    ///
    /// Every replica polls — the claim is `for update skip locked` over a
    /// materialised victim set, so it is safe under any replica count and needs
    /// no leadership. Only the *enqueue* of a periodic singleton job would.
    pub queue_poll_interval_seconds: u64,
    /// How long a claimed job may stay `running` with no completion before
    /// another replica may take it back.
    ///
    /// This is what makes the queue survive a pod kill: a replica that dies
    /// mid-job never writes a terminal state, and without this threshold the row
    /// would stay `running` forever. It must exceed the longest job body, or a
    /// slow job gets a second executor while the first is still working.
    pub queue_stale_claim_seconds: u64,
    /// Pending-depth cap. `enqueue` refuses beyond it with
    /// `moira.error.worker_queue_capacity_exceeded`.
    ///
    /// An unbounded queue is not a queue, it is a slow memory leak in table
    /// form: a producer faster than the drain rate grows `worker_jobs` until the
    /// claim's index scan is the slowest query in the deployment, and the first
    /// symptom is unrelated latency. Refusing at a known depth converts that into
    /// an error with a name.
    pub queue_max_pending_jobs: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetrySettings {
    pub service_name: String,
    pub env_filter: String,
    pub json: bool,
    pub prometheus_enabled: bool,
    pub metrics_path: String,
    pub otel_enabled: bool,
    pub otel_endpoint: Option<String>,
}

impl Settings {
    pub fn load() -> Result<Self, AppError> {
        let mut builder = Config::builder();
        if Path::new("config/default.toml").exists() {
            builder = builder.add_source(File::with_name("config/default"));
        }
        if Path::new("config/local.toml").exists() {
            builder = builder.add_source(File::with_name("config/local").required(false));
        }

        builder
            .add_source(environment_source())
            .build()
            .map_err(|err| AppError::Config(err.to_string()))?
            .try_deserialize()
            .map_err(|err| AppError::Config(err.to_string()))
    }

    pub fn validate(&self, mode: ProcessMode) -> Result<(), AppError> {
        let mut violations = Vec::new();

        if matches!(
            self.deployment.environment,
            DeploymentEnvironment::Production
        ) {
            self.validate_production(mode, &mut violations);
        }

        // Structural invariants of the stored-hash format, not production hardening —
        // see `validate_idempotency_hash_format`. They run in every environment.
        self.validate_idempotency_hash_format(&mut violations);

        // Same category: a lease expiry at or below the heartbeat interval is not a
        // tuning choice, it is a configuration that cannot work. Rejected at startup
        // because the symptom otherwise is replicas continuously reclaiming each
        // other's leases — visible only as an unexplained restart loop.
        self.validate_cluster(&mut violations);

        // Same category again: a permit TTL below the longest possible execution
        // reclaims a slot that is still in use, and a queue whose stale-claim
        // threshold is zero hands every running job to a second executor
        // immediately. Neither is a tuning choice.
        self.validate_coordination(&mut violations);

        // And again: an API-key prefix shorter than its own namespace plus a usable
        // random tail is not a tuning choice either.
        self.validate_api_key_prefix(&mut violations);

        // And again: a stale-JWKS ceiling below the freshness window is a bound that cannot
        // be enforced, because the fresh read path never consults it.
        self.validate_jwks_stale_ceiling(&mut violations);

        // Structural, so every environment: a keyring the process cannot assemble is not a
        // production-hardening question. See the function.
        self.validate_content_encryption(&mut violations);

        // `rag_chunks.chunk_index`, `start_offset` and `end_offset` are all `integer`, and the
        // chunker casts into them. A ceiling at or above `i32::MAX` would make those casts
        // saturate silently, so it is rejected at startup rather than discovered as a chunk
        // stored at offset 2147483647.
        if self.rag.max_chunk_chars == 0 {
            violations.push("rag.max_chunk_chars must be greater than zero".to_string());
        }
        if self.rag.max_chunks_per_document == 0 {
            violations.push("rag.max_chunks_per_document must be greater than zero".to_string());
        }
        if self.rag.max_chunks_per_document >= i32::MAX as usize {
            violations.push(format!(
                "rag.max_chunks_per_document must be below {} (rag_chunks.chunk_index is an integer)",
                i32::MAX
            ));
        }

        for origin in &self.cors.allowed_origins {
            if origin != "*" && validate_cors_origin(origin).is_err() {
                violations.push(format!(
                    "cors.allowed_origins contains invalid origin {origin:?}"
                ));
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(AppError::Config(format!(
                "configuration validation failed: {}",
                violations.join("; ")
            )))
        }
    }

    pub fn unsafe_development_features(&self, mode: ProcessMode) -> Vec<&'static str> {
        if matches!(
            self.deployment.environment,
            DeploymentEnvironment::Production
        ) {
            return Vec::new();
        }

        let mut features = Vec::new();
        if !self.auth.admin.enabled {
            features.push("admin_auth_disabled");
        }
        if self.auth.caller.dev_trust_headers {
            features.push("trusted_caller_headers");
        }
        if self.secrets.allow_insecure_dev_key {
            features.push("insecure_master_key_fallback");
        }
        if self.api_keys.allow_insecure_dev_pepper {
            features.push("insecure_api_key_pepper_fallback");
        }
        if self.idempotency.allow_insecure_dev_pepper {
            features.push("insecure_idempotency_pepper_fallback");
        }
        // Registered here for the reason §15 of the encryption record gives: a switch nobody
        // could see was inert is precisely what produced the `accept_legacy_hashes` incident,
        // so leaving this on has to produce a greppable startup WARN naming the feature.
        if self.content_encryption.allow_insecure_dev_key {
            features.push("insecure_content_encryption_key");
        }
        if self.auth.jwks.allow_insecure_dev_urls {
            features.push("insecure_jwks_urls");
        }
        if self.public_api.image_urls.allow_insecure_dev_urls {
            features.push("insecure_image_urls");
        }
        if self.provider_security.allow_http_provider_urls {
            features.push("http_provider_urls");
        }
        if self.cors.allowed_origins.iter().any(|origin| origin == "*") {
            features.push("wildcard_cors");
        }
        if self.workers.enabled {
            features.push("placeholder_workers");
        }
        if mode == ProcessMode::Serve && self.database.migrate_on_startup {
            features.push("automatic_migrations");
        }
        features
    }

    /// Enforces the structural invariants of `"{pepper_version}:{digest}"` in **every**
    /// environment (plan 03 findings F1 and F2).
    ///
    /// These are correctness invariants of the stored-hash format, not production-hardening
    /// policy, so they deliberately do not live in `validate_production_crypto`:
    ///
    /// * **empty** — the produced hash would start with a bare `":"`, and a stored value
    ///   could no longer be attributed to a pepper.
    /// * **contains `':'`** — `IdempotencyHasher::verify` splits on the *first* separator,
    ///   so `"v1:extra"` yields the version `"v1"` and a digest of `"extra:<base64>"`,
    ///   which never decodes. Every replay of a new-format row then fails to match and is
    ///   rejected with `409 idempotency_conflict`, in development just as much as in
    ///   production.
    /// * **too long** — the ledger columns are `varchar(128)`; a longer version tag
    ///   overflows them and turns every idempotent write into a `500`.
    fn validate_idempotency_hash_format(&self, violations: &mut Vec<String>) {
        let version = &self.idempotency.pepper_version;
        if version.trim().is_empty() {
            violations.push("idempotency.pepper_version must be non-empty".to_string());
        } else if version.contains(':') {
            violations.push("idempotency.pepper_version must not contain ':'".to_string());
        } else if version.len() > IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH {
            violations.push(format!(
                "idempotency.pepper_version must be at most {IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH} bytes \
                 so \"{{version}}:{{digest}}\" fits the varchar({IDEMPOTENCY_HASH_MAX_LENGTH}) ledger columns, \
                 got {}",
                version.len()
            ));
        }
    }

    /// Rejects a stale-JWKS ceiling the cache cannot actually honour.
    ///
    /// `max_stale_seconds` bounds how long a key set may keep authenticating callers after it
    /// stopped being refreshable, and it is the headline control on the unbounded-retention
    /// finding. Two ways to configure it into meaninglessness are refused at startup rather
    /// than discovered as keys outliving their revocation:
    ///
    /// * **Zero** disables the last-known-good fallback entirely. That is not the fail-closed
    ///   reading it looks like — it is a different policy (no outage tolerance at all), and an
    ///   operator who wants it should say so by other means, not by a bound of zero that reads
    ///   like "no retention allowed" and silently becomes "every transient IdP blip is an
    ///   outage".
    /// * **Below [`JWKS_FRESHNESS_SECONDS`]** is worse, because it looks like it tightened the
    ///   bound and did not. `JwksCache::fresh` serves any entry inside the freshness window on
    ///   a read lock without ever consulting the ceiling, so an entry aged between the ceiling
    ///   and the freshness window keeps authenticating callers past the limit that was set.
    ///   The ceiling only starts to apply once freshness has lapsed, so it has to sit at or
    ///   above it to mean anything.
    ///
    /// No upper bound is imposed: the default is the policy, and an operator who deliberately
    /// widens the window is making a stated availability trade, not tripping over a default.
    fn validate_jwks_stale_ceiling(&self, violations: &mut Vec<String>) {
        let ceiling = self.auth.jwks.max_stale_seconds;
        if ceiling == 0 {
            violations.push(
                "auth.jwks.max_stale_seconds must be greater than zero; a ceiling of zero \
                 disables the last-known-good fallback rather than bounding it"
                    .to_string(),
            );
        } else if ceiling < JWKS_FRESHNESS_SECONDS {
            violations.push(format!(
                "auth.jwks.max_stale_seconds must be at least the JWKS freshness window \
                 ({JWKS_FRESHNESS_SECONDS}s), or the ceiling is unenforceable: a still-fresh \
                 cache entry is served without consulting it, got {ceiling}"
            ));
        }
    }

    /// Structural invariants of the content-encryption keyring, checked in **every**
    /// environment.
    ///
    /// These are not production hardening — they are the difference between a process that can
    /// assemble a keyring and one that cannot — so they run everywhere, and every one of them
    /// accumulates into the shared violation list rather than short-circuiting. A configuration
    /// with three problems has to report three, or an operator fixes one, restarts, and
    /// discovers the next.
    ///
    /// The dev sentinel is rejected here rather than only in production, and that is
    /// deliberately stricter than the three existing 32-byte secrets: those reach their
    /// sentinel through an *implicit* fallback that no operator types, whereas
    /// `content_encryption.keys` has no such path. So the sentinel is reachable only via
    /// `allow_insecure_dev_key`, never by pasting it, in any environment.
    fn validate_content_encryption(&self, violations: &mut Vec<String>) {
        let content = &self.content_encryption;
        let entries = content.raw_key_entries();

        if entries.is_empty() && !content.allow_insecure_dev_key {
            violations.push(
                "content_encryption.keys must be set (MOIRA_CONTENT_ENCRYPTION__KEYS=\
                 \"<id>:<base64>\"); there is deliberately no fallback to \
                 secrets.master_key_base64"
                    .to_string(),
            );
        }

        let mut seen: Vec<&str> = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let Some((id, encoded)) = entry.split_once(':') else {
                violations.push(format!(
                    "content_encryption.keys entry {index} is not \"<id>:<base64>\""
                ));
                continue;
            };
            let id = id.trim();

            if !crate::security::is_valid_master_key_id(id) {
                violations.push(format!(
                    "content_encryption.keys entry {index} has key id {id:?}, which must match \
                     [A-Za-z0-9._-]{{1,{}}}",
                    crate::security::MASTER_KEY_ID_MAX_LENGTH
                ));
                continue;
            }
            if seen.contains(&id) {
                violations.push(format!(
                    "content_encryption.keys declares key id {id:?} more than once"
                ));
                continue;
            }
            seen.push(id);

            let field = format!("content_encryption.keys entry {id:?}");
            if encoded.trim().is_empty() {
                violations.push(format!("{field} has no key material"));
                continue;
            }
            // The shared 32-byte validator, so this fourth secret is checked by the same code
            // and reports the same wording as the three that already exist.
            validate_32_byte_secret(
                Some(encoded.trim()),
                CONTENT_ENCRYPTION_DEVELOPMENT_KEY,
                &field,
                violations,
            );
        }

        if !crate::security::is_valid_master_key_id(content.active_key_id.trim()) {
            violations.push(format!(
                "content_encryption.active_key_id {:?} must match [A-Za-z0-9._-]{{1,{}}}",
                content.active_key_id,
                crate::security::MASTER_KEY_ID_MAX_LENGTH
            ));
        } else if !entries.is_empty() && !seen.contains(&content.active_key_id.trim()) {
            // Named alongside what *is* configured, because mid-rotation the question an
            // operator is actually asking is "which ids did this process load?".
            violations.push(format!(
                "content_encryption.active_key_id {:?} is not among the configured key ids {:?}",
                content.active_key_id, seen
            ));
        }

        if content.min_refresh_seconds == 0 {
            violations
                .push("content_encryption.min_refresh_seconds must be at least 1".to_string());
        }
        if content.refresh_seconds == 0 {
            violations.push("content_encryption.refresh_seconds must be at least 1".to_string());
        } else if content.refresh_seconds < content.min_refresh_seconds {
            violations.push(format!(
                "content_encryption.refresh_seconds ({}) must be at least \
                 content_encryption.min_refresh_seconds ({})",
                content.refresh_seconds, content.min_refresh_seconds
            ));
        }
    }

    /// Whether the worker supervisor takes a leader lock before a singleton job.
    ///
    /// The single place `workers.leader_election_enabled`'s `None` is resolved.
    /// Reading the raw `Option` anywhere else reintroduces the two-switches-that-
    /// disagree problem the mirroring default exists to remove.
    pub fn leader_election_enabled(&self) -> bool {
        self.workers
            .leader_election_enabled
            .unwrap_or(self.cluster.admission_enabled)
    }

    /// The API-key prefix must stay long enough to be a lookup key, in every environment.
    ///
    /// The prefix is *plaintext* and indexed: `AuthService::verify_api_key` and the
    /// anonymous invite preview both select a candidate row by it before any Argon2 work
    /// happens. Two things scale with how much of it is random — the collision rate against
    /// the unique index over live keys, and the preview endpoint's documented "a caller
    /// without a valid prefix causes zero Argon2 work" bound.
    ///
    /// `ApiKeyHasher::new` used to absorb this with a bare `.max(12)`, which knew nothing
    /// about the namespace being prefixed: `moira_inv_` is ten characters, so twelve left
    /// **two** random base64url characters — 4096 distinct prefixes. The shipped default is
    /// 20, so this was only ever reachable by configuring it, which is exactly why it is
    /// caught here.
    ///
    /// **Refused, not clamped.** A clamp makes the misconfiguration invisible: the operator
    /// believes they set one value, the process runs another, and nothing says so. The same
    /// reasoning `validated_invite_lifetime` applies to an invitation's lifetime.
    fn validate_api_key_prefix(&self, violations: &mut Vec<String>) {
        use crate::security::{MIN_API_KEY_PREFIX_LENGTH, MIN_RANDOM_PREFIX_CHARS};

        if self.api_keys.prefix_length < MIN_API_KEY_PREFIX_LENGTH {
            violations.push(format!(
                "api_keys.prefix_length ({}) must be at least {MIN_API_KEY_PREFIX_LENGTH}, so \
                 every key namespace keeps {MIN_RANDOM_PREFIX_CHARS} random characters after \
                 it — a shorter prefix collides on the unique index over live keys and shrinks \
                 the anonymous invite preview's guessing space to a search",
                self.api_keys.prefix_length
            ));
        }
    }

    /// Structural invariants of the admission lease, checked in every environment.
    ///
    /// `lease_expiry_seconds <= lease_heartbeat_seconds` is the one that matters:
    /// a replica's lease would be reclaimable before — or exactly when — it is due
    /// to renew, so every replica would continuously steal every other replica's
    /// lease and the admission count would oscillate. The symptom in production is
    /// pods restarting for no visible reason, which is why this is a startup
    /// rejection and not a runtime warning.
    fn validate_cluster(&self, violations: &mut Vec<String>) {
        if self.cluster.max_replicas == 0 {
            violations.push("cluster.max_replicas must be at least 1".to_string());
        }
        if self.cluster.lease_heartbeat_seconds == 0 {
            violations.push("cluster.lease_heartbeat_seconds must be at least 1".to_string());
        }
        if self.cluster.lease_expiry_seconds <= self.cluster.lease_heartbeat_seconds {
            violations.push(format!(
                "cluster.lease_expiry_seconds ({}) must exceed cluster.lease_heartbeat_seconds \
                 ({}), or a live replica's lease is reclaimable before it renews",
                self.cluster.lease_expiry_seconds, self.cluster.lease_heartbeat_seconds
            ));
        }
    }

    /// Invariants of the Redis-backed permits and the durable worker queue.
    ///
    /// The Redis half is checked **only when Redis is enabled**: an operator who
    /// never turns Redis on should not be made to reason about a TTL that governs
    /// nothing. The queue half is checked unconditionally, because the queue rides
    /// `workers.enabled` and a nonsensical value there is nonsensical whether or
    /// not the supervisor is currently running.
    fn validate_coordination(&self, violations: &mut Vec<String>) {
        if self.redis.enabled
            && self.redis.permit_ttl_seconds <= self.runtime.maximum_execution_timeout_seconds
        {
            violations.push(format!(
                "redis.permit_ttl_seconds ({}) must exceed \
                 runtime.maximum_execution_timeout_seconds ({}), or a cluster-wide \
                 concurrency slot is reclaimed while the execution holding it is still \
                 running and the effective ceiling drifts above the configured limit",
                self.redis.permit_ttl_seconds, self.runtime.maximum_execution_timeout_seconds
            ));
        }
        if self.workers.queue_stale_claim_seconds == 0 {
            violations.push(
                "workers.queue_stale_claim_seconds must be at least 1, or every claimed \
                 job is immediately reclaimable by another replica"
                    .to_string(),
            );
        }
        if self.workers.queue_max_pending_jobs < 1 {
            violations.push(
                "workers.queue_max_pending_jobs must be at least 1, or no job can ever be \
                 enqueued"
                    .to_string(),
            );
        }
    }

    fn validate_production(&self, mode: ProcessMode, violations: &mut Vec<String>) {
        if !self.database.require {
            violations.push("database.require must be true in production".to_string());
        }
        if self
            .database
            .url
            .as_deref()
            .is_none_or(|url| url.trim().is_empty())
        {
            violations.push("database.url must be set in production".to_string());
        }

        if mode == ProcessMode::Migrate {
            return;
        }

        if !self.auth.admin.enabled {
            violations.push("auth.admin.enabled must be true in production".to_string());
        }
        if self.auth.caller.dev_trust_headers {
            violations
                .push("auth.caller.dev_trust_headers must be false in production".to_string());
        }
        if self.provider_security.allow_http_provider_urls {
            violations.push(
                "provider_security.allow_http_provider_urls must be false in production"
                    .to_string(),
            );
        }
        if self.auth.jwks.allow_insecure_dev_urls {
            violations
                .push("auth.jwks.allow_insecure_dev_urls must be false in production".to_string());
        }
        if self.public_api.image_urls.allow_insecure_dev_urls {
            violations.push(
                "public_api.image_urls.allow_insecure_dev_urls must be false in production"
                    .to_string(),
            );
        }
        if self.workers.enabled {
            violations.push("workers.enabled must be false until workers are implemented".into());
        }
        if self.cors.allowed_origins.iter().any(|origin| origin == "*") {
            violations.push("cors.allowed_origins cannot contain '*' in production".to_string());
        }
        if mode == ProcessMode::Serve && self.database.migrate_on_startup {
            violations.push("database.migrate_on_startup must be false in production".to_string());
        }

        self.validate_production_crypto(violations);
    }

    fn validate_production_crypto(&self, violations: &mut Vec<String>) {
        if self.secrets.allow_insecure_dev_key {
            violations
                .push("secrets.allow_insecure_dev_key must be false in production".to_string());
        }
        validate_32_byte_secret(
            self.secrets.master_key_base64.as_deref(),
            [7; 32],
            "secrets.master_key_base64",
            violations,
        );
        if self.secrets.key_id.trim().is_empty() || self.secrets.key_id == "dev-local" {
            violations.push("secrets.key_id must be non-empty and non-development".to_string());
        }

        // The master key is required in production **unconditionally**, even where no
        // application uses `encrypted_content` today, because the persistence policy is
        // flippable at runtime through the admin API without a restart. Requiring the key only
        // when some policy row selects it would make a boot invariant depend on mutable
        // database state, so a replica that booted yesterday would fail today because someone
        // changed a policy on another replica. See `docs/decision-encryption-at-rest.md` §10.
        if self.content_encryption.allow_insecure_dev_key {
            violations.push(
                "content_encryption.allow_insecure_dev_key must be false in production".to_string(),
            );
        }
        if self.content_encryption.active_key_id.trim().is_empty()
            || self.content_encryption.active_key_id == DEVELOPMENT_KEY_ID
        {
            violations.push(
                "content_encryption.active_key_id must be non-empty and non-development"
                    .to_string(),
            );
        }

        if self.api_keys.allow_insecure_dev_pepper {
            violations
                .push("api_keys.allow_insecure_dev_pepper must be false in production".to_string());
        }
        validate_32_byte_secret(
            self.api_keys.pepper_base64.as_deref(),
            [11; 32],
            "api_keys.pepper_base64",
            violations,
        );
        if self.api_keys.pepper_version.trim().is_empty()
            || self.api_keys.pepper_version == "dev-local"
        {
            violations
                .push("api_keys.pepper_version must be non-empty and non-development".to_string());
        }

        if self.idempotency.allow_insecure_dev_pepper {
            violations.push(
                "idempotency.allow_insecure_dev_pepper must be false in production".to_string(),
            );
        }
        validate_32_byte_secret(
            self.idempotency.pepper_base64.as_deref(),
            [13; 32],
            "idempotency.pepper_base64",
            violations,
        );
        // `pepper_version` is the hash *format* version ("v1"), not a deployment stage, so
        // it is never rejected here as a development sentinel the way
        // `api_keys.pepper_version` is. Its structural checks (non-empty, no ':', length
        // bound) are correctness invariants and therefore live in
        // `validate_idempotency_hash_format`, which runs in every environment.
    }
}

fn environment_source() -> Environment {
    Environment::with_prefix("MOIRA")
        .prefix_separator("_")
        .separator("__")
        .try_parsing(true)
        .list_separator(",")
        .with_list_parse_key("cors.allowed_origins")
        .ignore_empty(true)
}

impl CorsSettings {
    pub fn allowed_origin_headers(&self) -> Result<Vec<HeaderValue>, AppError> {
        self.allowed_origins
            .iter()
            .filter(|origin| origin.as_str() != "*")
            .map(|origin| {
                validate_cors_origin(origin).map_err(|_| {
                    AppError::Config(format!("invalid CORS allowed origin {origin:?}"))
                })
            })
            .collect()
    }
}

impl ProcessMode {
    pub fn parse(value: Option<&str>) -> Result<Self, AppError> {
        match value {
            None | Some("serve") => Ok(Self::Serve),
            Some("migrate") => Ok(Self::Migrate),
            Some("bootstrap-system-key") => Ok(Self::BootstrapSystemKey),
            Some("execute-test") => Ok(Self::ExecuteTest),
            Some("keyring") => Ok(Self::Keyring),
            Some(other) => Err(AppError::Config(format!(
                "unknown command {other:?}; expected serve, migrate, bootstrap-system-key, \
                 execute-test, or keyring"
            ))),
        }
    }
}

fn validate_cors_origin(origin: &str) -> Result<HeaderValue, ()> {
    let url = reqwest::Url::parse(origin).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(());
    }
    origin.parse().map_err(|_| ())
}

/// The fourth development sentinel, after `secrets`' `7`, `api_keys`' `11` and `idempotency`'s
/// `13`.
///
/// It exists so that *shipping* a development key is impossible: the value is well known, and
/// `validate_content_encryption` refuses it whenever it appears in
/// `MOIRA_CONTENT_ENCRYPTION__KEYS`. A developer's local key, minted by `scripts/dev-env.sh`, is
/// real and random and is never this.
pub const CONTENT_ENCRYPTION_DEVELOPMENT_KEY: [u8; 32] = [17; 32];

/// The key id `config/default.toml` ships, and the one production refuses.
const DEVELOPMENT_KEY_ID: &str = "dev-local";

fn validate_32_byte_secret(
    encoded: Option<&str>,
    development_value: [u8; 32],
    field: &str,
    violations: &mut Vec<String>,
) {
    let Some(encoded) = encoded.filter(|value| !value.trim().is_empty()) else {
        violations.push(format!("{field} must be set in production"));
        return;
    };

    match STANDARD.decode(encoded) {
        Ok(value) if value.len() != 32 => {
            violations.push(format!("{field} must decode to exactly 32 bytes"));
        }
        Ok(value) if value.as_slice() == development_value => {
            violations.push(format!("{field} cannot use the development sentinel"));
        }
        Ok(_) => {}
        Err(_) => violations.push(format!("{field} must be valid base64")),
    }
}

impl ServerSettings {
    pub fn bind_addr(&self) -> Result<SocketAddr, AppError> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(|err| AppError::Config(format!("invalid bind address: {err}")))
    }
}

impl SecretSettings {
    pub fn master_key_bytes(&self) -> Result<[u8; 32], AppError> {
        let key = match self
            .master_key_base64
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(value) => STANDARD
                .decode(value)
                .map_err(|err| AppError::Config(format!("invalid master key base64: {err}")))?,
            None if self.allow_insecure_dev_key => vec![7; 32],
            None => {
                return Err(AppError::Config(
                    "MOIRA_SECRETS__MASTER_KEY_BASE64 must be set".to_string(),
                ));
            }
        };

        key.try_into()
            .map_err(|_| AppError::Config("master key must decode to 32 bytes".to_string()))
    }
}

impl ContentEncryptionSettings {
    /// The non-empty, whitespace-trimmed entries of [`Self::keys`].
    ///
    /// A trailing comma is not a second key, so empty entries are dropped rather than reported:
    /// `"a:b,"` is what a shell heredoc produces and refusing it teaches nothing.
    fn raw_key_entries(&self) -> Vec<&str> {
        self.keys
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .collect()
    }

    /// The loaded master keys and the resolved active id.
    ///
    /// `Settings::validate` has already rejected every shape this can refuse, so an error here
    /// means the process was built from settings that never went through validation. It is
    /// still an error rather than a fallback: silently substituting a key is the failure this
    /// whole subsystem exists to refuse.
    pub fn master_keys(&self) -> Result<(crate::security::MasterKeyRing, String), AppError> {
        let entries = self.raw_key_entries();
        let active = self.active_key_id.trim().to_string();

        if entries.is_empty() {
            if !self.allow_insecure_dev_key {
                return Err(AppError::Config(
                    "MOIRA_CONTENT_ENCRYPTION__KEYS must be set".to_string(),
                ));
            }
            let active = if active.is_empty() {
                DEVELOPMENT_KEY_ID.to_string()
            } else {
                active
            };
            // The only path that ever yields the sentinel. It is never reachable by configuring
            // a key, in any environment — see `validate_content_encryption`.
            return Ok((
                HashMap::from([(
                    active.clone(),
                    Zeroizing::new(CONTENT_ENCRYPTION_DEVELOPMENT_KEY),
                )]),
                active,
            ));
        }

        let mut keys = HashMap::with_capacity(entries.len());
        for entry in entries {
            let (id, encoded) = entry.split_once(':').ok_or_else(|| {
                AppError::Config(format!(
                    "content_encryption.keys entry {entry:?} is not \"<id>:<base64>\""
                ))
            })?;
            let decoded = STANDARD.decode(encoded.trim()).map_err(|_| {
                AppError::Config(format!(
                    "content_encryption.keys entry {:?} is not valid base64",
                    id.trim()
                ))
            })?;
            let decoded: [u8; 32] = decoded.try_into().map_err(|_| {
                AppError::Config(format!(
                    "content_encryption.keys entry {:?} must decode to exactly 32 bytes",
                    id.trim()
                ))
            })?;
            keys.insert(id.trim().to_string(), Zeroizing::new(decoded));
        }

        if !keys.contains_key(&active) {
            return Err(AppError::Config(format!(
                "content_encryption.active_key_id {active:?} is not among the configured key ids"
            )));
        }
        Ok((keys, active))
    }
}

impl ApiKeySettings {
    pub fn pepper_bytes(&self) -> Result<Vec<u8>, AppError> {
        match self
            .pepper_base64
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(value) => STANDARD
                .decode(value)
                .map_err(|err| AppError::Config(format!("invalid api key pepper base64: {err}"))),
            None if self.allow_insecure_dev_pepper => Ok(vec![11; 32]),
            None => Err(AppError::Config(
                "MOIRA_API_KEYS__PEPPER_BASE64 must be set".to_string(),
            )),
        }
    }
}

impl IdempotencySettings {
    pub fn pepper_bytes(&self) -> Result<Vec<u8>, AppError> {
        match self
            .pepper_base64
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            Some(value) => STANDARD.decode(value).map_err(|err| {
                AppError::Config(format!("invalid idempotency pepper base64: {err}"))
            }),
            None if self.allow_insecure_dev_pepper => Ok(vec![13; 32]),
            None => Err(AppError::Config(
                "MOIRA_IDEMPOTENCY__PEPPER_BASE64 must be set".to_string(),
            )),
        }
    }
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
        }
    }
}

impl Default for DatabaseSettings {
    fn default() -> Self {
        Self {
            url: None,
            max_connections: 10,
            min_connections: 1,
            connect_timeout_seconds: 5,
            require: false,
            migrate_on_startup: true,
        }
    }
}

impl Default for SecretSettings {
    fn default() -> Self {
        Self {
            master_key_base64: None,
            key_id: "dev-local".to_string(),
            allow_insecure_dev_key: true,
        }
    }
}

impl Default for ContentEncryptionSettings {
    fn default() -> Self {
        Self {
            custody: ContentEncryptionCustody::Environment,
            keys: String::new(),
            active_key_id: DEVELOPMENT_KEY_ID.to_string(),
            allow_insecure_dev_key: true,
            refresh_seconds: 300,
            min_refresh_seconds: 5,
            data_key_rotation_days: 30,
        }
    }
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            admin: JwtAuthSettings {
                enabled: false,
                jwks_url: None,
                issuer: None,
                audience: None,
                required_scope: Some("moira.admin".to_string()),
                leeway_seconds: 60,
            },
            caller: CallerAuthSettings::default(),
            jwks: JwksFetchSettings::default(),
            provider_settings_cache_ttl_seconds: default_auth_provider_settings_cache_ttl_seconds(),
        }
    }
}

impl Default for JwksFetchSettings {
    fn default() -> Self {
        Self {
            // JWKS documents are small; 256KiB is generous for even the largest
            // real-world key set.
            max_response_bytes: 262_144,
            timeout_ms: 3_000,
            // Fails closed by default, unlike the pepper dev fallbacks: an
            // unhardened JWKS fetch is an SSRF primitive, so the escape hatch must
            // be opted into explicitly even in development.
            allow_insecure_dev_urls: false,
            // 24 hours. See the field doc: long enough that no realistic transient IdP
            // outage reaches it, short enough that a revoked signing key stops being
            // honoured within a day even if the IdP never comes back.
            max_stale_seconds: 86_400,
        }
    }
}

impl Default for JwtAuthSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            jwks_url: None,
            issuer: None,
            audience: None,
            required_scope: Some("moira.admin".to_string()),
            leeway_seconds: 60,
        }
    }
}

impl Default for CallerAuthSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            jwks_url: None,
            issuer: None,
            audience: None,
            leeway_seconds: 60,
            dev_trust_headers: true,
        }
    }
}

impl Default for ApiKeySettings {
    fn default() -> Self {
        Self {
            pepper_base64: None,
            pepper_version: "dev-local".to_string(),
            allow_insecure_dev_pepper: true,
            prefix_length: 20,
        }
    }
}

impl Default for IdempotencySettings {
    fn default() -> Self {
        Self {
            pepper_base64: None,
            // Format version of the produced hash, not a deployment stage marker:
            // "v1" is the correct value in production too. Unlike
            // `ApiKeySettings::pepper_version` it is therefore NOT rejected as a
            // development sentinel by `validate_production_crypto`.
            pepper_version: "v1".to_string(),
            allow_insecure_dev_pepper: true,
            // Defaults to the migration-compatible behaviour: pre-switch rows keep
            // replaying. See the field's doc comment for how and when to turn it off.
            accept_legacy_hashes: true,
        }
    }
}

impl Default for CorsSettings {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["*".to_string()],
        }
    }
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            runtime_config_ttl_seconds: 10,
        }
    }
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            diagnostic_endpoint_enabled: false,
            maximum_eligible_model_candidates: 20,
            maximum_provider_fallback_candidates: 5,
            maximum_retries_per_candidate: 2,
            maximum_total_upstream_attempts: 6,
            default_execution_timeout_seconds: 120,
            maximum_execution_timeout_seconds: 600,
            global_execution_concurrency: 100,
            application_execution_concurrency: 100,
            external_user_execution_concurrency: 5,
            runtime_cache_max_entries: 1_000,
            runtime_cache_ttl_seconds: 300,
            internal_stream_queue_capacity: 64,
        }
    }
}

impl Default for PublicApiSettings {
    fn default() -> Self {
        Self {
            openai_responses_compat_enabled: false,
            chat_completions_compat_enabled: false,
            default_persistence_mode: "metadata_only".to_string(),
            maximum_request_bytes: 1_048_576,
            maximum_input_items: 128,
            maximum_messages: 128,
            maximum_content_parts_per_message: 32,
            maximum_text_part_bytes: 262_144,
            maximum_image_count: 8,
            maximum_tool_count: 32,
            maximum_metadata_bytes: 16 * 1024,
            maximum_metadata_keys: 64,
            maximum_metadata_depth: 4,
            maximum_metadata_key_bytes: 128,
            maximum_metadata_string_bytes: 2048,
            maximum_schema_bytes: 64 * 1024,
            heartbeat_seconds: 15,
            rate_limiter_max_entries: 10_000,
            image_urls: ImageUrlSettings::default(),
        }
    }
}

impl Default for RedisSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            namespace: "moira".to_string(),
            connect_timeout_seconds: 2,
            invalidation_channel: "moira:runtime-config".to_string(),
            permit_ttl_seconds: 900,
        }
    }
}

impl Default for ClusterSettings {
    fn default() -> Self {
        Self {
            // Off by default: see the field docs. The library default is what
            // every test, every CLI mode and every non-Helm deployment gets, and
            // none of them has more than one replica to admit.
            admission_enabled: false,
            max_replicas: 1,
            lease_heartbeat_seconds: 10,
            // 3x the heartbeat. Two consecutive renewals may be lost — a
            // failover, a brief pool starvation — before another replica is
            // entitled to the lease.
            lease_expiry_seconds: 30,
        }
    }
}

impl Default for WorkerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            shutdown_grace_seconds: 30,
            max_concurrent_jobs: 8,
            retry_base_delay_seconds: 5,
            retry_max_delay_seconds: 300,
            dead_letter_retention_hours: 168,
            // 500 rows/batch: a single-statement delete of 500 rows against an
            // index scan finishes in low milliseconds, so the row locks it takes
            // on `idempotency_records` are held far too briefly to stall a
            // concurrent idempotency claim (which additionally skips them via
            // `for update skip locked`).
            retention_batch_size: 500,
            // 5 minutes. With the default per-tick cap (batch_size * 20 = 10_000
            // rows per table per sweep) that sustains ~33 expired rows/second per
            // table, comfortably above any plausible steady-state mint rate, while
            // leaving the database idle between sweeps.
            retention_interval_seconds: 300,
            leader_election_enabled: None,
            queue_poll_interval_seconds: 5,
            // 5 minutes, matching the plan's `stale_claim_seconds`. Every job body
            // shipped today is a stub that completes immediately; the threshold is
            // sized for the real bodies plan 11 will add.
            queue_stale_claim_seconds: 300,
            // 10k pending rows is roughly where the claim's `(status, run_at)`
            // index scan stops being free on commodity hardware.
            queue_max_pending_jobs: 10_000,
        }
    }
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            service_name: "moira".to_string(),
            env_filter: "moira=info,tower_http=info".to_string(),
            json: false,
            prometheus_enabled: false,
            metrics_path: "/metrics".to_string(),
            otel_enabled: false,
            otel_endpoint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn valid_production_settings() -> Settings {
        let mut settings = Settings::default();
        settings.deployment.environment = DeploymentEnvironment::Production;
        settings.database.url = Some("postgres://postgres:postgres@localhost/moira".to_string());
        settings.database.require = true;
        settings.database.migrate_on_startup = false;
        settings.auth.admin.enabled = true;
        settings.auth.caller.dev_trust_headers = false;
        settings.secrets.master_key_base64 = Some(STANDARD.encode([1; 32]));
        settings.secrets.key_id = "prod-2026-07".to_string();
        settings.secrets.allow_insecure_dev_key = false;
        settings.api_keys.pepper_base64 = Some(STANDARD.encode([2; 32]));
        settings.api_keys.pepper_version = "prod-2026-07".to_string();
        settings.api_keys.allow_insecure_dev_pepper = false;
        settings.idempotency.pepper_base64 = Some(STANDARD.encode([3; 32]));
        settings.idempotency.allow_insecure_dev_pepper = false;
        settings.content_encryption.keys = format!("prod-2026-07:{}", STANDARD.encode([4; 32]));
        settings.content_encryption.active_key_id = "prod-2026-07".to_string();
        settings.content_encryption.allow_insecure_dev_key = false;
        settings.cors.allowed_origins = vec!["https://admin.example.com".to_string()];
        settings
    }

    #[test]
    fn development_defaults_remain_valid_and_report_unsafe_features() {
        let settings = Settings::default();
        settings.validate(ProcessMode::Serve).unwrap();

        let warnings = settings.unsafe_development_features(ProcessMode::Serve);
        assert!(warnings.contains(&"admin_auth_disabled"));
        assert!(warnings.contains(&"trusted_caller_headers"));
        assert!(warnings.contains(&"insecure_master_key_fallback"));
        assert!(warnings.contains(&"insecure_api_key_pepper_fallback"));
        assert!(warnings.contains(&"wildcard_cors"));
        assert!(warnings.contains(&"automatic_migrations"));
    }

    #[test]
    fn valid_production_serve_configuration_passes() {
        valid_production_settings()
            .validate(ProcessMode::Serve)
            .unwrap();
    }

    #[test]
    fn production_validation_aggregates_unsafe_defaults() {
        let mut settings = Settings::default();
        settings.deployment.environment = DeploymentEnvironment::Production;

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        for expected in [
            "database.require",
            "database.url",
            "auth.admin.enabled",
            "auth.caller.dev_trust_headers",
            "secrets.allow_insecure_dev_key",
            "secrets.master_key_base64",
            "secrets.key_id",
            "api_keys.allow_insecure_dev_pepper",
            "api_keys.pepper_base64",
            "api_keys.pepper_version",
            "idempotency.allow_insecure_dev_pepper",
            "idempotency.pepper_base64",
            "content_encryption.allow_insecure_dev_key",
            "content_encryption.active_key_id",
            "cors.allowed_origins",
            "database.migrate_on_startup",
        ] {
            assert!(error.contains(expected), "missing {expected} in {error}");
        }
    }

    /// Every content-encryption violation is asserted on its **message text**, because the
    /// remedy string is the deliverable: an operator meets this validator once, at 3 a.m., on a
    /// deployment that will not boot.
    #[test]
    fn content_encryption_accumulates_every_structural_violation_at_once() {
        let mut settings = Settings::default();
        settings.content_encryption.keys = format!(
            "no-colon,bad id:{},dup:{},dup:{},short:{},sentinel:{}",
            STANDARD.encode([1; 32]),
            STANDARD.encode([2; 32]),
            STANDARD.encode([2; 32]),
            STANDARD.encode([3; 16]),
            STANDARD.encode(CONTENT_ENCRYPTION_DEVELOPMENT_KEY),
        );
        settings.content_encryption.active_key_id = "absent".to_string();
        settings.content_encryption.refresh_seconds = 0;

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();

        // Never first-violation-wins: all seven are present in one message.
        for expected in [
            "content_encryption.keys entry 0 is not \"<id>:<base64>\"",
            "content_encryption.keys entry 1 has key id \"bad id\"",
            "content_encryption.keys declares key id \"dup\" more than once",
            "content_encryption.keys entry \"short\" must decode to exactly 32 bytes",
            "content_encryption.keys entry \"sentinel\" cannot use the development sentinel",
            "content_encryption.active_key_id \"absent\" is not among the configured key ids",
            "content_encryption.refresh_seconds must be at least 1",
        ] {
            assert!(error.contains(expected), "missing {expected:?} in {error}");
        }
        // The configured ids are named alongside the missing one, because mid-rotation the
        // question an operator is actually asking is "which ids did this process load?".
        assert!(
            error.contains(r#"is not among the configured key ids ["dup", "short", "sentinel"]"#),
            "{error}"
        );
    }

    /// The sentinel is refused in **development** too, unlike the three older secrets. It has no
    /// implicit-fallback path an operator could reach by accident, so typing it is always a
    /// mistake.
    #[test]
    fn the_development_sentinel_is_never_accepted_as_configured_key_material() {
        let mut settings = Settings::default();
        settings.content_encryption.keys = format!(
            "dev-local:{}",
            STANDARD.encode(CONTENT_ENCRYPTION_DEVELOPMENT_KEY)
        );

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "content_encryption.keys entry \"dev-local\" cannot use the development sentinel"
            ),
            "{error}"
        );

        // The same bytes reached through `allow_insecure_dev_key` are fine — that is the flag's
        // entire purpose, and this is what keeps the assertion above about *typing* the value.
        let dev = Settings::default();
        dev.validate(ProcessMode::Serve).unwrap();
        let (keys, active) = dev.content_encryption.master_keys().unwrap();
        assert_eq!(active, "dev-local");
        assert_eq!(
            keys["dev-local"].as_slice(),
            CONTENT_ENCRYPTION_DEVELOPMENT_KEY.as_slice()
        );
    }

    /// There is deliberately no implicit fallback to `secrets.master_key_base64`.
    #[test]
    fn missing_keys_without_the_dev_flag_are_refused_and_never_fall_back_to_secrets() {
        let mut settings = Settings::default();
        settings.content_encryption.allow_insecure_dev_key = false;
        settings.secrets.master_key_base64 = Some(STANDARD.encode([99; 32]));

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("content_encryption.keys must be set (MOIRA_CONTENT_ENCRYPTION__KEYS")
                && error.contains("no fallback to secrets.master_key_base64"),
            "{error}"
        );
        // And the loader agrees with the validator, rather than quietly borrowing the other key.
        let err = settings.content_encryption.master_keys().unwrap_err();
        assert!(err.to_string().contains("MOIRA_CONTENT_ENCRYPTION__KEYS"));
    }

    #[test]
    fn a_refresh_interval_below_its_own_floor_is_refused() {
        let mut settings = Settings::default();
        settings.content_encryption.min_refresh_seconds = 5;
        settings.content_encryption.refresh_seconds = 4;

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "content_encryption.refresh_seconds (4) must be at least \
                 content_encryption.min_refresh_seconds (5)"
            ),
            "{error}"
        );
    }

    /// Old master keys stay loaded through a rotation, and the active one is one of them.
    #[test]
    fn a_multi_key_ring_loads_every_key_and_resolves_the_active_id() {
        let mut settings = Settings::default();
        settings.content_encryption.keys = format!(
            "old-2026-06:{},new-2026-08:{}",
            STANDARD.encode([1; 32]),
            STANDARD.encode([2; 32])
        );
        settings.content_encryption.active_key_id = "new-2026-08".to_string();
        settings.validate(ProcessMode::Serve).unwrap();

        let (keys, active) = settings.content_encryption.master_keys().unwrap();
        assert_eq!(active, "new-2026-08");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys["old-2026-06"].as_slice(), [1u8; 32].as_slice());
        assert_eq!(keys["new-2026-08"].as_slice(), [2u8; 32].as_slice());
    }

    /// A `Debug` on `Settings` reaches this block, so the material must never render — the same
    /// reason `ApiKeySettings` hand-writes its own.
    #[test]
    fn content_encryption_debug_renders_ids_and_never_key_material() {
        let mut settings = Settings::default();
        let encoded = STANDARD.encode([0xab; 32]);
        settings.content_encryption.keys = format!("prod-2026-08:{encoded}");

        let rendered = format!("{:?}", settings.content_encryption);
        assert!(rendered.contains("prod-2026-08"));
        assert!(!rendered.contains(&encoded), "{rendered}");
        // The whole `Settings` tree is the surface that actually leaks, so assert there too.
        assert!(!format!("{settings:?}").contains(&encoded));
    }

    /// A custody backend this build cannot serve is refused at deserialisation, naming what is
    /// accepted — rather than booting and failing on the first wrap.
    #[test]
    fn an_unimplemented_custody_backend_is_refused_by_name() {
        serde_json::from_str::<ContentEncryptionSettings>(r#"{"custody":"environment"}"#)
            .expect("the shipped backend deserializes");

        let error = serde_json::from_str::<ContentEncryptionSettings>(r#"{"custody":"aws_kms"}"#)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("aws_kms") && error.contains("environment"),
            "{error}"
        );
    }

    #[test]
    fn a_left_on_dev_key_flag_is_a_named_startup_warning() {
        // The mechanism whose absence produced the `accept_legacy_hashes` incident: the flag has
        // to be greppable from a startup line, not merely present in a struct.
        assert!(
            Settings::default()
                .unsafe_development_features(ProcessMode::Serve)
                .contains(&"insecure_content_encryption_key")
        );

        let mut off = Settings::default();
        off.content_encryption.allow_insecure_dev_key = false;
        off.content_encryption.keys = format!("local:{}", STANDARD.encode([5; 32]));
        off.content_encryption.active_key_id = "local".to_string();
        assert!(
            !off.unsafe_development_features(ProcessMode::Serve)
                .contains(&"insecure_content_encryption_key")
        );
    }

    #[test]
    fn production_rejects_invalid_and_sentinel_crypto_material() {
        let mut settings = valid_production_settings();
        settings.secrets.master_key_base64 = Some("not-base64".to_string());
        settings.api_keys.pepper_base64 = Some(STANDARD.encode([11; 32]));

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(error.contains("secrets.master_key_base64 must be valid base64"));
        assert!(error.contains("api_keys.pepper_base64 cannot use the development sentinel"));
    }

    #[test]
    fn migrate_mode_requires_only_production_database_contract() {
        let mut settings = Settings::default();
        settings.deployment.environment = DeploymentEnvironment::Production;
        settings.database.require = true;
        settings.database.url = Some("postgres://postgres:postgres@localhost/moira".to_string());

        settings.validate(ProcessMode::Migrate).unwrap();
    }

    #[test]
    fn idempotency_pepper_bytes_decodes_base64() {
        let settings = IdempotencySettings {
            pepper_base64: Some(STANDARD.encode([9; 32])),
            pepper_version: "v1".to_string(),
            allow_insecure_dev_pepper: false,
            accept_legacy_hashes: true,
        };

        assert_eq!(settings.pepper_bytes().unwrap(), vec![9_u8; 32]);
    }

    #[test]
    fn idempotency_pepper_bytes_uses_the_dev_fallback_when_allowed() {
        let settings = IdempotencySettings::default();
        assert!(settings.allow_insecure_dev_pepper);
        assert_eq!(settings.pepper_version, "v1");
        assert_eq!(settings.pepper_bytes().unwrap(), vec![13_u8; 32]);
        assert!(
            settings.accept_legacy_hashes,
            "the dual-read window must stay open by default so the HMAC switch is a \
             behaviour-preserving deploy; operators close it explicitly"
        );
    }

    /// Plan 03 finding F1: the `varchar(128)` guarantee must not rest on the operator
    /// happening to pick a short `pepper_version`.
    ///
    /// The bound is *derived* from `IdempotencyHasher::hash` rather than asserted against a
    /// hardcoded `"v1"`, so it keeps holding if the digest encoding ever changes width.
    #[test]
    fn idempotency_pepper_version_bound_is_driven_by_the_hasher() {
        use crate::security::IdempotencyHasher;

        fn hash_length(version: &str) -> usize {
            IdempotencyHasher::new(b"pepper".to_vec(), version.to_string())
                // 4 KiB of input: HMAC-SHA-256 output width is independent of input size,
                // so a large body must not widen the stored value.
                .hash(&vec![7_u8; 4096])
                .len()
        }

        // Everything the hasher appends after the version tag, measured, not assumed.
        let measured_suffix = hash_length("v") - "v".len();
        assert_eq!(
            measured_suffix, IDEMPOTENCY_HASH_SUFFIX_LENGTH,
            "the hash suffix width drifted from the constant the bound is computed from"
        );

        let longest_allowed = "p".repeat(IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH);
        assert_eq!(
            hash_length(&longest_allowed),
            IDEMPOTENCY_HASH_MAX_LENGTH,
            "the longest accepted version must produce a hash that exactly fills varchar(128)"
        );

        let one_too_long = "p".repeat(IDEMPOTENCY_PEPPER_VERSION_MAX_LENGTH + 1);
        assert!(
            hash_length(&one_too_long) > IDEMPOTENCY_HASH_MAX_LENGTH,
            "the bound must be the exact point where the column overflows, not a guess"
        );

        // And validation must draw the line in the same place the column does.
        let mut settings = Settings::default();
        settings.idempotency.pepper_version = longest_allowed;
        settings.validate(ProcessMode::Serve).unwrap();

        settings.idempotency.pepper_version = one_too_long;
        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("idempotency.pepper_version must be at most"),
            "unexpected error: {error}"
        );
    }

    /// The API-key prefix bound, **measured against the hasher** rather than restated.
    ///
    /// Restating `MIN_API_KEY_PREFIX_LENGTH`'s own arithmetic here would agree with the
    /// constant however wrong both were. This generates a real key at the rejected value and
    /// counts what would actually be left after the namespace — the quantity the unique index
    /// over live keys and the anonymous invite preview's cost bound both depend on.
    ///
    /// It also pins **refusal, not clamping**: the shipped default must pass, and one below
    /// the floor must produce a startup error, in every environment.
    #[test]
    fn the_api_key_prefix_floor_is_the_point_where_a_namespace_stops_leaving_random_material() {
        use crate::security::{
            ApiKeyHasher, KEY_NAMESPACES, MIN_API_KEY_PREFIX_LENGTH, MIN_RANDOM_PREFIX_CHARS,
        };

        let shortest = KEY_NAMESPACES
            .iter()
            .min_by_key(|namespace| namespace.len())
            .expect("at least one key namespace");
        let longest = KEY_NAMESPACES
            .iter()
            .max_by_key(|namespace| namespace.len())
            .expect("at least one key namespace");

        // At the floor, even the *longest* namespace leaves the required random material.
        let at_floor = ApiKeyHasher::new(b"pepper".to_vec(), "v1", MIN_API_KEY_PREFIX_LENGTH);
        let generated = at_floor.generate(longest).expect("generate at the floor");
        let random = generated
            .key_prefix
            .strip_prefix(&format!("{longest}_"))
            .expect("the prefix still contains the whole namespace");
        assert_eq!(
            random.chars().count(),
            MIN_RANDOM_PREFIX_CHARS,
            "the floor must be exactly the point where the longest namespace still leaves \
             {MIN_RANDOM_PREFIX_CHARS} random characters, not a value with slack in it"
        );

        // One below it, the *shortest* namespace would have been the first to lose material
        // had the floor not been derived from the longest — which is the drift the derivation
        // exists to prevent.
        assert!(
            MIN_API_KEY_PREFIX_LENGTH - 1 < longest.len() + 1 + MIN_RANDOM_PREFIX_CHARS,
            "one below the floor must be below the requirement for at least one namespace"
        );
        assert!(shortest.len() <= longest.len());

        // Refused, not clamped: the shipped default passes, the value below the floor is a
        // startup error, and this holds outside production too.
        let mut settings = Settings::default();
        assert_eq!(
            settings.deployment.environment,
            DeploymentEnvironment::Development,
            "the check must fire outside production, so this must not be a production default"
        );
        settings.validate(ProcessMode::Serve).expect(
            "the shipped api_keys.prefix_length must satisfy the floor it is validated against",
        );

        // **Both sides of the boundary, not just the rejected side.** Found by `cargo mutants`:
        // with only "the default passes" and "one below is refused", turning `<` into `<=`
        // survived — the default is one *above* the floor, so a comparison that also rejected
        // the floor itself went unnoticed. The floor has to be an accepted value or it is not
        // the floor.
        settings.api_keys.prefix_length = MIN_API_KEY_PREFIX_LENGTH;
        settings
            .validate(ProcessMode::Serve)
            .expect("the floor itself must be an accepted configuration");

        settings.api_keys.prefix_length = MIN_API_KEY_PREFIX_LENGTH - 1;
        let error = settings
            .validate(ProcessMode::Serve)
            .expect_err("a prefix below the floor must be refused at startup")
            .to_string();
        assert!(
            error.contains("api_keys.prefix_length"),
            "unexpected error: {error}"
        );
    }

    /// Plan 03 finding F2: the structural checks are correctness invariants, so they must
    /// fire outside production too. A development `pepper_version` containing `':'` makes
    /// every new-format replay fail to match and return `409 idempotency_conflict`.
    #[test]
    fn idempotency_pepper_version_structure_is_validated_outside_production() {
        let mut settings = Settings::default();
        assert_eq!(
            settings.deployment.environment,
            DeploymentEnvironment::Development,
            "this test is only meaningful in a non-production environment"
        );

        for (version, expected) in [
            (
                "v1:extra",
                "idempotency.pepper_version must not contain ':'",
            ),
            ("", "idempotency.pepper_version must be non-empty"),
            ("   ", "idempotency.pepper_version must be non-empty"),
        ] {
            settings.idempotency.pepper_version = version.to_string();
            let error = settings
                .validate(ProcessMode::Serve)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(expected),
                "version {version:?} produced unexpected error: {error}"
            );
        }
    }

    #[test]
    fn idempotency_pepper_is_required_without_the_dev_fallback() {
        let settings = IdempotencySettings {
            pepper_base64: None,
            pepper_version: "v1".to_string(),
            allow_insecure_dev_pepper: false,
            accept_legacy_hashes: true,
        };

        let error = settings.pepper_bytes().unwrap_err().to_string();
        assert!(
            error.contains("MOIRA_IDEMPOTENCY__PEPPER_BASE64 must be set"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn production_rejects_allow_insecure_dev_idempotency_pepper() {
        let mut settings = valid_production_settings();
        settings.idempotency.allow_insecure_dev_pepper = true;
        settings.idempotency.pepper_base64 = None;

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("idempotency.allow_insecure_dev_pepper must be false in production"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("idempotency.pepper_base64 must be set in production"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn production_rejects_the_idempotency_pepper_development_sentinel() {
        let mut settings = valid_production_settings();
        settings.idempotency.pepper_base64 = Some(STANDARD.encode([13; 32]));

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("idempotency.pepper_base64 cannot use the development sentinel"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn production_rejects_an_idempotency_pepper_version_containing_the_separator() {
        let mut settings = valid_production_settings();
        settings.idempotency.pepper_version = "v1:extra".to_string();

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("idempotency.pepper_version must not contain ':'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn production_rejects_allow_insecure_dev_jwks_urls() {
        let mut settings = valid_production_settings();
        settings.auth.jwks.allow_insecure_dev_urls = true;

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("auth.jwks.allow_insecure_dev_urls must be false in production"),
            "unexpected error: {error}"
        );
    }

    /// The image-URL escape hatch is a *separate* flag from the JWKS one on purpose, so
    /// production must refuse it on its own account. If the two were ever collapsed into
    /// one knob this fails, which is the point.
    #[test]
    fn production_rejects_allow_insecure_dev_image_urls() {
        let mut settings = valid_production_settings();
        settings.public_api.image_urls.allow_insecure_dev_urls = true;
        assert!(
            !settings.auth.jwks.allow_insecure_dev_urls,
            "the JWKS flag stays off, so only the image flag can produce the violation"
        );

        let error = settings
            .validate(ProcessMode::Serve)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "public_api.image_urls.allow_insecure_dev_urls must be false in production"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn image_url_settings_defaults_fail_closed() {
        let settings = ImageUrlSettings::default();

        assert!(
            !settings.allow_insecure_dev_urls,
            "the SSRF escape hatch must default to disabled"
        );
        assert!(
            settings.allowed_hosts.is_empty(),
            "the egress allow-list is opt-in; the address rules carry the default posture"
        );
        assert!(
            settings.dns_timeout_ms > 0 && settings.total_validation_timeout_ms > 0,
            "a zero budget would make every image URL fail closed on timing alone"
        );
        assert!(
            settings.total_validation_timeout_ms >= settings.dns_timeout_ms,
            "a request-wide budget below the per-host one would cut short the first lookup"
        );
    }

    #[test]
    fn jwks_fetch_settings_defaults_fail_closed() {
        let settings = JwksFetchSettings::default();

        assert_eq!(settings.max_response_bytes, 262_144);
        assert_eq!(settings.timeout_ms, 3_000);
        assert!(
            !settings.allow_insecure_dev_urls,
            "the SSRF escape hatch must default to disabled"
        );
        // The headline control of the stale-retention finding, pinned at the value that
        // actually ships. The eviction test supplies its own short ceiling, so without this
        // assertion the default could be widened back to "effectively forever" — reopening
        // the finding — with the whole suite still green.
        assert_eq!(
            settings.max_stale_seconds, 86_400,
            "the stale-JWKS ceiling must default to 24h; a wider default reopens the \
             unbounded-retention finding, and no other test pins the shipped value"
        );
        assert!(
            settings.max_stale_seconds >= JWKS_FRESHNESS_SECONDS,
            "the shipped default must itself satisfy the bound Settings::validate imposes"
        );
        assert_eq!(
            AuthSettings::default().jwks.max_response_bytes,
            settings.max_response_bytes,
            "AuthSettings must embed the shared JwksFetchSettings default"
        );
        assert_eq!(
            AuthSettings::default().jwks.max_stale_seconds,
            settings.max_stale_seconds,
            "AuthSettings must embed the shared stale ceiling, not a second default"
        );
    }

    /// A stale ceiling below the freshness window is refused, in every environment.
    ///
    /// The failure it prevents is a silent one: such a ceiling reads like a *tightening* but
    /// `JwksCache::fresh` never consults it, so entries younger than the freshness window go
    /// on authenticating callers past the bound the operator believed they had set.
    #[test]
    fn a_stale_jwks_ceiling_below_the_freshness_window_is_refused() {
        // `Settings::default()` is a development profile, which is the point: this bound is
        // structural, not production hardening, so it must bite outside production too.
        let mut settings = Settings::default();
        settings.auth.jwks.max_stale_seconds = JWKS_FRESHNESS_SECONDS - 1;

        let error = settings
            .validate(ProcessMode::Serve)
            .expect_err("a ceiling the fresh read path ignores must not start the process")
            .to_string();
        assert!(
            error.contains("auth.jwks.max_stale_seconds"),
            "the violation must name the offending key, got: {error}"
        );

        settings.auth.jwks.max_stale_seconds = JWKS_FRESHNESS_SECONDS;
        settings
            .validate(ProcessMode::Serve)
            .expect("a ceiling exactly at the freshness window is enforceable and allowed");
    }

    /// A zero ceiling is refused too: it disables the last-known-good fallback rather than
    /// bounding it, which is a different policy wearing a fail-closed costume.
    #[test]
    fn a_zero_stale_jwks_ceiling_is_refused() {
        let mut settings = Settings::default();
        settings.auth.jwks.max_stale_seconds = 0;

        let error = settings
            .validate(ProcessMode::Serve)
            .expect_err("a zero stale ceiling must not start the process")
            .to_string();
        assert!(
            error.contains("auth.jwks.max_stale_seconds"),
            "the violation must name the offending key, got: {error}"
        );
    }

    #[test]
    fn process_mode_rejects_unknown_commands() {
        assert_eq!(ProcessMode::parse(None).unwrap(), ProcessMode::Serve);
        assert_eq!(
            ProcessMode::parse(Some("migrate")).unwrap(),
            ProcessMode::Migrate
        );
        assert!(ProcessMode::parse(Some("unknown")).is_err());
    }

    #[test]
    fn environment_uses_documented_prefix_and_parses_cors_list() {
        let source = environment_source().source(Some(HashMap::from([
            (
                "MOIRA_DEPLOYMENT__ENVIRONMENT".to_string(),
                "production".to_string(),
            ),
            ("MOIRA_SERVER__PORT".to_string(), "18080".to_string()),
            (
                "MOIRA_CORS__ALLOWED_ORIGINS".to_string(),
                "https://one.example.com,https://two.example.com".to_string(),
            ),
        ])));
        let settings = Config::builder()
            .add_source(File::with_name("config/default"))
            .add_source(source)
            .build()
            .unwrap()
            .try_deserialize::<Settings>()
            .unwrap();

        assert_eq!(
            settings.deployment.environment,
            DeploymentEnvironment::Production
        );
        assert_eq!(settings.server.port, 18080);
        assert_eq!(
            settings.cors.allowed_origins,
            vec![
                "https://one.example.com".to_string(),
                "https://two.example.com".to_string()
            ]
        );
    }
}
