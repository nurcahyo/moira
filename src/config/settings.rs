use std::{net::SocketAddr, path::Path};

use base64::{Engine, engine::general_purpose::STANDARD};
use config::{Config, Environment, File};
use serde::Deserialize;

use crate::error::AppError;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Settings {
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
            .add_source(
                Environment::with_prefix("MOIRA")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .map_err(|err| AppError::Config(err.to_string()))?
            .try_deserialize()
            .map_err(|err| AppError::Config(err.to_string()))
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
