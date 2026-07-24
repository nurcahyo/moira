#![allow(dead_code)]

pub mod mock_openai;

use std::{env, sync::LazyLock, time::Duration};

use moira::{
    app::AppState,
    application::{
        AdminService, ConversationService, MoiraExecutionService, PublicExecutionService,
        RequestContext,
    },
    config::Settings,
    domain::{
        ApplicationCreateRequest, ApplicationExecutionPolicyPutRequest, ConsumerKeyCreateRequest,
        ConversationPolicyPutRequest, CredentialCreateRequest, CredentialScope, CredentialSecret,
        CredentialType, DiagnosticExecutionRequest, ExecutionOptions, ProviderCreateRequest,
        ProviderModelCreateRequest, ProviderRuntimePolicyPutRequest, ProviderType,
        ResponsePersistenceMode, RouteDefinitionCreateRequest, RouteSelectionStrategy,
        RoutingPolicyCreateRequest, RuntimePolicyStatus,
    },
    security::{Actor, ActorType},
};
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgPoolOptions, PgRow},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, OnceCell, OwnedMutexGuard},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(10);
static TEST_DATABASE_URL: OnceCell<Option<String>> = OnceCell::const_new();
static TEST_SERIAL: LazyLock<std::sync::Arc<Mutex<()>>> =
    LazyLock::new(|| std::sync::Arc::new(Mutex::new(())));

#[derive(Debug, Clone, Copy)]
pub struct RuntimePolicy {
    pub request_timeout_ms: i32,
    pub stream_idle_timeout_ms: i32,
    pub max_concurrent_requests: i32,
    pub max_concurrent_streams: i32,
    pub retry_limit: i32,
    pub retry_base_delay_ms: i32,
    pub retry_max_delay_ms: i32,
    pub circuit_failure_threshold: i32,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            request_timeout_ms: 2_000,
            stream_idle_timeout_ms: 2_000,
            max_concurrent_requests: 8,
            max_concurrent_streams: 8,
            retry_limit: 0,
            retry_base_delay_ms: 0,
            retry_max_delay_ms: 0,
            circuit_failure_threshold: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderFixture {
    pub provider_id: Uuid,
    pub model_id: Uuid,
    pub credential_id: Uuid,
}

pub struct MoiraHttpServer {
    pub base_url: String,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl MoiraHttpServer {
    pub async fn start(state: AppState) -> Self {
        let app = moira::build_router(state).expect("build Moira test router");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Moira test server");
        let address = listener.local_addr().expect("Moira test server address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve Moira test router");
        });
        Self {
            base_url: format!("http://{address}"),
            shutdown,
            task,
        }
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        timeout(DATABASE_TIMEOUT, self.task)
            .await
            .expect("Moira test server shutdown timed out")
            .expect("Moira test server task panicked");
    }
}

pub struct LifecycleFixture {
    _serial: OwnedMutexGuard<()>,
    pub state: AppState,
    pub pool: PgPool,
    pub actor: Actor,
    pub application_id: Uuid,
    pub route_id: Uuid,
    pub route_key: String,
}

impl LifecycleFixture {
    pub async fn new() -> Option<Self> {
        let serial = TEST_SERIAL.clone().lock_owned().await;
        let pool = test_pool().await?;
        let mut settings = Settings::default();
        settings.provider_security.allow_http_provider_urls = true;
        settings.provider_security.allow_private_provider_urls = true;
        settings.runtime.default_execution_timeout_seconds = 5;
        settings.runtime.maximum_execution_timeout_seconds = 10;
        settings.runtime.global_execution_concurrency = 64;
        settings.runtime.application_execution_concurrency = 64;
        settings.runtime.external_user_execution_concurrency = 64;
        settings.runtime.internal_stream_queue_capacity = 64;
        let state = AppState::new(settings, Some(pool.clone())).expect("test app state");
        let actor = admin_actor();
        let suffix = Uuid::now_v7().simple().to_string();
        let ctx = request_context();
        let admin = AdminService::new(&state).expect("admin service");
        let application = admin
            .create_application(
                &actor,
                &ctx,
                ApplicationCreateRequest {
                    external_application_id: Some(format!("lifecycle-{suffix}")),
                    application_slug: Some(format!("lifecycle-{suffix}")),
                    display_name: format!("Lifecycle {suffix}"),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create lifecycle application");
        let route_key = format!("route_{suffix}");
        let route = moira::application::RuntimeAdminService::new(&state)
            .expect("runtime admin service")
            .create_route_definition(
                &actor,
                &request_context(),
                RouteDefinitionCreateRequest {
                    route_key: route_key.clone(),
                    display_name: format!("Lifecycle route {suffix}"),
                    description: None,
                    selection_strategy: RouteSelectionStrategy::Default,
                    agent_profile_id: None,
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create lifecycle route");

        Some(Self {
            _serial: serial,
            state,
            pool,
            actor,
            application_id: application.id,
            route_id: route.id,
            route_key,
        })
    }

    pub async fn add_provider(
        &self,
        base_url: String,
        priority: i32,
        policy: RuntimePolicy,
    ) -> ProviderFixture {
        let suffix = Uuid::now_v7().simple().to_string();
        let admin = AdminService::new(&self.state).expect("admin service");
        let provider = admin
            .create_provider(
                &self.actor,
                &request_context(),
                ProviderCreateRequest {
                    provider_type: ProviderType::OpenAiCompatible,
                    display_name: format!("Lifecycle provider {suffix}"),
                    base_url: Some(base_url),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create lifecycle provider");
        let model = admin
            .create_provider_model(
                &self.actor,
                &request_context(),
                provider.id,
                ProviderModelCreateRequest {
                    model_key: "test-model".to_string(),
                    display_name: Some("Lifecycle model".to_string()),
                    capabilities: json!({ "streaming": true }),
                },
            )
            .await
            .expect("create lifecycle provider model");
        let credential = admin
            .create_credential(
                &self.actor,
                &request_context(),
                CredentialCreateRequest {
                    provider_id: provider.id,
                    credential_type: CredentialType::ApiKey,
                    scope: CredentialScope::Global,
                    secret: CredentialSecret::ApiKey {
                        api_key: "sk-lifecycle-secret".to_string(),
                    },
                    display_name: Some("Lifecycle credential".to_string()),
                    priority: 100,
                    expires_at: None,
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create encrypted lifecycle credential");

        let runtime = moira::application::RuntimeAdminService::new(&self.state)
            .expect("runtime admin service");
        let current_runtime_policy = runtime
            .get_provider_runtime_policy(&self.actor, provider.id)
            .await
            .expect("get default provider runtime policy");
        runtime
            .put_provider_runtime_policy(
                &self.actor,
                &request_context(),
                provider.id,
                Some(current_runtime_policy.version),
                ProviderRuntimePolicyPutRequest {
                    connect_timeout_ms: Some(policy.request_timeout_ms),
                    request_timeout_ms: Some(policy.request_timeout_ms),
                    stream_idle_timeout_ms: Some(policy.stream_idle_timeout_ms),
                    max_concurrent_requests: Some(policy.max_concurrent_requests),
                    max_concurrent_streams: Some(policy.max_concurrent_streams),
                    retry_limit: Some(policy.retry_limit),
                    retry_base_delay_ms: Some(policy.retry_base_delay_ms),
                    retry_max_delay_ms: Some(policy.retry_max_delay_ms),
                    circuit_failure_threshold: Some(policy.circuit_failure_threshold),
                    circuit_open_duration_ms: Some(30_000),
                    status: Some(RuntimePolicyStatus::Active),
                },
            )
            .await
            .expect("create provider runtime policy");
        runtime
            .create_routing_policy(
                &self.actor,
                &request_context(),
                RoutingPolicyCreateRequest {
                    application_id: Some(self.application_id),
                    external_tenant_id: None,
                    route_id: self.route_id,
                    provider_id: provider.id,
                    provider_model_id: model.id,
                    priority,
                    weight: 1,
                    cost_weight: 0.0,
                    latency_weight: 0.0,
                    quality_weight: 0.0,
                    privacy_class: None,
                    required_capabilities: Vec::new(),
                    maximum_cost_per_request: None,
                    maximum_input_tokens: None,
                    maximum_output_tokens: None,
                    timeout_ms: Some(policy.request_timeout_ms),
                    retry_policy: json!({}),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create lifecycle routing policy");

        ProviderFixture {
            provider_id: provider.id,
            model_id: model.id,
            credential_id: credential.id,
        }
    }

    pub fn execution_service(&self) -> MoiraExecutionService {
        MoiraExecutionService::new(self.state.clone()).expect("execution service")
    }

    pub fn command(&self, stream: bool) -> moira::domain::ExecutionCommand {
        let request = DiagnosticExecutionRequest {
            application_id: Some(self.application_id),
            external_tenant_id: None,
            external_user_id: Some(format!("user-{}", Uuid::now_v7().simple())),
            route: Some(self.route_key.clone()),
            provider_id: None,
            provider_model_id: None,
            credential_id: None,
            prompt: "test lifecycle".to_string(),
            stream,
            options: ExecutionOptions {
                stream,
                timeout_ms: Some(5_000),
                max_retries: Some(2),
                max_fallbacks: Some(2),
                ..ExecutionOptions::default()
            },
            metadata: json!({ "test_fixture": true }),
        };
        MoiraExecutionService::command_from_diagnostic(&self.actor, &request_context(), request)
    }

    pub async fn enable_public_streaming(&self) -> String {
        PublicExecutionService::new(&self.state)
            .expect("public service")
            .put_application_execution_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                None,
                ApplicationExecutionPolicyPutRequest {
                    responses_enabled: Some(true),
                    streaming_enabled: Some(true),
                    route_overrides_allowed: Some(true),
                    persistence_mode: Some(ResponsePersistenceMode::MetadataOnly),
                    rate_limit_requests_per_minute: Some(1_000),
                    rate_limit_streams_per_minute: Some(1_000),
                    ..ApplicationExecutionPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable public execution policy");
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_conversation_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                ConversationPolicyPutRequest {
                    conversations_enabled: Some(true),
                    caller_can_create_conversations: Some(true),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable lifecycle conversation policy");
        AdminService::new(&self.state)
            .expect("admin service")
            .create_consumer_key(
                &self.actor,
                &request_context(),
                ConsumerKeyCreateRequest {
                    application_id: self.application_id,
                    display_name: "Lifecycle public client".to_string(),
                    scopes: vec![
                        "moira:responses:create".to_string(),
                        "moira:responses:stream".to_string(),
                        "moira:responses:read".to_string(),
                        "moira:execution:override-route".to_string(),
                        "moira:conversations:create".to_string(),
                    ],
                    expires_at: None,
                },
            )
            .await
            .expect("create lifecycle consumer key")
            .secret
            .expect("consumer key secret")
    }

    pub async fn scalar_i64(&self, query: &str, execution_id: Uuid) -> i64 {
        timeout(
            DATABASE_TIMEOUT,
            sqlx::query_scalar::<_, i64>(query)
                .bind(execution_id)
                .fetch_one(&self.pool),
        )
        .await
        .expect("database scalar query timed out")
        .expect("database scalar query failed")
    }

    pub async fn row(&self, query: &str, execution_id: Uuid) -> PgRow {
        timeout(
            DATABASE_TIMEOUT,
            sqlx::query(query).bind(execution_id).fetch_one(&self.pool),
        )
        .await
        .expect("database row query timed out")
        .expect("database row query failed")
    }
}

pub fn request_context() -> RequestContext {
    RequestContext {
        request_id: format!("test-{}", Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("moira-lifecycle-test".to_string()),
        idempotency_key: None,
    }
}

pub fn admin_actor() -> Actor {
    Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some("lifecycle-admin".to_string()),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    }
}

async fn test_pool() -> Option<PgPool> {
    let database_url = TEST_DATABASE_URL
        .get_or_init(|| async {
            let database_url = match env::var("MOIRA_TEST_DATABASE_URL") {
                Ok(value) if !value.trim().is_empty() => value,
                _ if env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
                    panic!("MOIRA_TEST_DATABASE_URL is required when CI=true for lifecycle tests")
                }
                _ => {
                    eprintln!(
                        "skipping execution lifecycle tests: MOIRA_TEST_DATABASE_URL is not set"
                    );
                    return None;
                }
            };
            let pool = timeout(
                DATABASE_TIMEOUT,
                PgPoolOptions::new()
                    .max_connections(2)
                    .connect(&database_url),
            )
            .await
            .expect("lifecycle database connection timed out")
            .expect("connect lifecycle test database");
            timeout(DATABASE_TIMEOUT, sqlx::migrate!().run(&pool))
                .await
                .expect("lifecycle migrations timed out")
                .expect("run lifecycle migrations");
            pool.close().await;
            Some(database_url)
        })
        .await
        .clone()?;
    Some(
        timeout(
            DATABASE_TIMEOUT,
            PgPoolOptions::new()
                .max_connections(8)
                .connect(&database_url),
        )
        .await
        .expect("lifecycle fixture database connection timed out")
        .expect("connect lifecycle fixture database"),
    )
}

pub fn public_response_request(route: &str) -> moira::domain::PublicResponseRequest {
    moira::domain::PublicResponseRequest {
        input: vec![moira::domain::PublicInputMessage {
            role: moira::domain::PublicMessageRole::User,
            content: vec![moira::domain::PublicContentPart::InputText {
                text: "stream lifecycle".to_string(),
            }],
        }],
        route: Some(route.to_string()),
        model: None,
        provider: None,
        credential_id: None,
        conversation: None,
        temperature: None,
        top_p: None,
        max_output_tokens: Some(64),
        timeout_ms: None,
        response_format: moira::domain::PublicResponseFormat::Text,
        tools: Vec::new(),
        tool_choice: None,
        metadata: Value::Object(Default::default()),
        seed: None,
    }
}
