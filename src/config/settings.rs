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

#[derive(Debug, Clone, Deserialize)]
pub struct WorkerSettings {
    pub enabled: bool,
    pub shutdown_grace_seconds: u64,
    pub max_concurrent_jobs: usize,
    pub retry_base_delay_seconds: u64,
    pub retry_max_delay_seconds: u64,
    pub dead_letter_retention_hours: u64,
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
