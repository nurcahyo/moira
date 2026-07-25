use std::{net::SocketAddr, path::Path};

use axum::http::HeaderValue;
use base64::{Engine, engine::general_purpose::STANDARD};
use config::{Config, Environment, File};
use serde::Deserialize;

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
    pub workers: WorkerSettings,
    #[serde(default)]
    pub telemetry: TelemetrySettings,
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

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeySettings {
    pub pepper_base64: Option<String>,
    pub pepper_version: String,
    pub allow_insecure_dev_pepper: bool,
    pub prefix_length: usize,
}

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
#[derive(Debug, Clone, Deserialize)]
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedisSettings {
    pub enabled: bool,
    pub url: Option<String>,
    pub namespace: String,
    pub connect_timeout_seconds: u64,
    pub invalidation_channel: String,
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
        if self.auth.jwks.allow_insecure_dev_urls {
            features.push("insecure_jwks_urls");
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
            Some(other) => Err(AppError::Config(format!(
                "unknown command {other:?}; expected serve, migrate, bootstrap-system-key, or execute-test"
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
            "cors.allowed_origins",
            "database.migrate_on_startup",
        ] {
            assert!(error.contains(expected), "missing {expected} in {error}");
        }
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

    #[test]
    fn jwks_fetch_settings_defaults_fail_closed() {
        let settings = JwksFetchSettings::default();

        assert_eq!(settings.max_response_bytes, 262_144);
        assert_eq!(settings.timeout_ms, 3_000);
        assert!(
            !settings.allow_insecure_dev_urls,
            "the SSRF escape hatch must default to disabled"
        );
        assert_eq!(
            AuthSettings::default().jwks.max_response_bytes,
            settings.max_response_bytes,
            "AuthSettings must embed the shared JwksFetchSettings default"
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
