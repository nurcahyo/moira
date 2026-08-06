#![allow(dead_code)]

pub mod mock_openai;

use std::{
    env,
    sync::{Arc, LazyLock, Mutex as StdMutex, Once},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
        CredentialType, DiagnosticExecutionRequest, EmbeddingPolicyPutRequest, ExecutionOptions,
        MemoryConsentMode, MemoryPolicyPutRequest, ProviderCreateRequest,
        ProviderModelCreateRequest, ProviderRuntimePolicyPutRequest, ProviderType,
        ResponsePersistenceMode, RetrievalPolicyPutRequest, RouteDefinitionCreateRequest,
        RouteSelectionStrategy, RoutingPolicyCreateRequest, RuntimePolicyStatus,
    },
    security::{Actor, ActorType},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Connection, PgConnection, PgPool,
    postgres::{PgPoolOptions, PgRow},
};
use tokio::{
    net::TcpListener,
    sync::{OnceCell, Semaphore, SemaphorePermit},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// The shared fixture: an `AppState` on a private, migrated database, with one
/// application and one route already created.
///
/// Since plan 06 module 13 every instance owns its own database (see
/// [`TestDatabase`]), so two fixtures — in the same binary or in different ones —
/// cannot see each other's rows. Tests that used to reason about "rows a neighbouring
/// suite happened to leave behind" no longer have to.
pub struct LifecycleFixture {
    pub state: AppState,
    pub pool: PgPool,
    pub actor: Actor,
    pub application_id: Uuid,
    pub route_id: Uuid,
    pub route_key: String,
    /// Declared last so it is dropped last: the database outlives every field that
    /// still holds a connection to it.
    database: TestDatabase,
}

impl LifecycleFixture {
    pub async fn new() -> Option<Self> {
        Self::with_settings(|_| {}).await
    }

    /// As [`Self::new`], letting the caller adjust `Settings` before the `AppState` is built.
    ///
    /// Some settings are read once at construction — `public_api.openai_responses_compat_enabled`
    /// gates whether `POST /v1/responses` exists at all — so they cannot be turned on after the
    /// fact. Building a *second* `AppState` on the same pool is not an equivalent workaround:
    /// the two would not share this state's caches, and a test would silently exercise a
    /// different object graph from the one that wrote its fixtures.
    pub async fn with_settings(customize: impl FnOnce(&mut Settings)) -> Option<Self> {
        let database = TestDatabase::create().await?;
        let pool = database.pool.clone();
        let mut settings = Settings::default();
        settings.provider_security.allow_http_provider_urls = true;
        settings.provider_security.allow_private_provider_urls = true;
        // The fixtures' stub IdPs live on `http://127.0.0.1:0`, which the JWKS SSRF
        // policy denies by design. This is the same dev-only escape hatch as the two
        // `provider_security` flags above; `Settings::validate` rejects it in
        // production, and the hardened default remains `false`.
        settings.auth.jwks.allow_insecure_dev_urls = true;
        settings.runtime.default_execution_timeout_seconds = 5;
        settings.runtime.maximum_execution_timeout_seconds = 10;
        settings.runtime.global_execution_concurrency = 64;
        settings.runtime.application_execution_concurrency = 64;
        settings.runtime.external_user_execution_concurrency = 64;
        settings.runtime.internal_stream_queue_capacity = 64;
        customize(&mut settings);
        let state = AppState::new(settings, Some(pool.clone()))
            .await
            .expect("test app state");
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
            state,
            pool,
            actor,
            application_id: application.id,
            route_id: route.id,
            route_key,
            database,
        })
    }

    /// The name of this fixture's private database (module 13, P2-13).
    pub fn database_name(&self) -> &str {
        self.database.name()
    }

    pub async fn add_provider(
        &self,
        base_url: String,
        priority: i32,
        policy: RuntimePolicy,
    ) -> ProviderFixture {
        self.add_provider_with_capabilities(
            base_url,
            priority,
            policy,
            json!({ "streaming": true }),
        )
        .await
    }

    /// As [`Self::add_provider`], with the provider model's advertised capabilities under the
    /// caller's control.
    ///
    /// Routing filters candidates through `capabilities_match`, so an execution that requires
    /// `structured_output` — which every non-`text` response format does — cannot resolve
    /// against the default model. Without this the request fails at routing and a test would
    /// never reach the assertion it was written for.
    pub async fn add_provider_with_capabilities(
        &self,
        base_url: String,
        priority: i32,
        policy: RuntimePolicy,
        capabilities: Value,
    ) -> ProviderFixture {
        self.add_typed_provider_with_capabilities(
            ProviderType::OpenAiCompatible,
            base_url,
            priority,
            policy,
            capabilities,
        )
        .await
    }

    /// As [`Self::add_provider_with_capabilities`], with the **provider type** also under the
    /// caller's control (finding F39).
    ///
    /// Provider type is not cosmetic: it selects the `build_completion_model` arm, and therefore
    /// which Rig client encodes the request. `DeepSeek` in particular is
    /// `GenericCompletionModel<DeepSeekExt, _>` — OpenAI-compatible on the wire, so it drives
    /// [`mock_openai::MockOpenAiServer`] unchanged, while carrying
    /// `SUPPORTS_RESPONSE_FORMAT = false`, which is the only way to observe Rig dropping a
    /// schema without reaching a real provider.
    ///
    /// Every arm this is used with must accept `CredentialType::ApiKey`, which is what this
    /// fixture creates.
    pub async fn add_typed_provider_with_capabilities(
        &self,
        provider_type: ProviderType,
        base_url: String,
        priority: i32,
        policy: RuntimePolicy,
        capabilities: Value,
    ) -> ProviderFixture {
        let suffix = Uuid::now_v7().simple().to_string();
        let admin = AdminService::new(&self.state).expect("admin service");
        let provider = admin
            .create_provider(
                &self.actor,
                &request_context(),
                ProviderCreateRequest {
                    provider_type,
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
                    capabilities,
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

    /// Points this fixture's application at an embedding-capable provider and turns RAG
    /// embeddings on.
    ///
    /// Deliberately does **not** reuse [`Self::add_provider`]: that one also writes a routing
    /// policy, which would make the embedding provider a completion candidate for this
    /// fixture's route and quietly change what an unrelated execution test resolves.
    ///
    /// `base_url` is normally a [`mock_openai::MockOpenAiServer::base_url`]. There is no live
    /// embedding credential anywhere in this repository and none is required: the mock serves
    /// `/v1/embeddings` with deterministic vectors, so what is proven end to end is the
    /// pipeline — chunk, call, persist, derive the status — not any particular provider's
    /// embedding quality.
    pub async fn enable_rag_embeddings(
        &self,
        base_url: String,
        model_key: &str,
    ) -> ProviderFixture {
        let suffix = Uuid::now_v7().simple().to_string();
        let admin = AdminService::new(&self.state).expect("admin service");
        let provider = admin
            .create_provider(
                &self.actor,
                &request_context(),
                ProviderCreateRequest {
                    provider_type: ProviderType::OpenAiCompatible,
                    display_name: format!("Embedding provider {suffix}"),
                    base_url: Some(base_url),
                    metadata: json!({ "test_fixture": true, "purpose": "embeddings" }),
                },
            )
            .await
            .expect("create embedding provider");
        let model = admin
            .create_provider_model(
                &self.actor,
                &request_context(),
                provider.id,
                ProviderModelCreateRequest {
                    model_key: model_key.to_string(),
                    display_name: Some("Embedding model".to_string()),
                    capabilities: json!({ "embeddings": true }),
                },
            )
            .await
            .expect("create embedding provider model");
        let credential = admin
            .create_credential(
                &self.actor,
                &request_context(),
                CredentialCreateRequest {
                    provider_id: provider.id,
                    credential_type: CredentialType::ApiKey,
                    scope: CredentialScope::Global,
                    secret: CredentialSecret::ApiKey {
                        api_key: "sk-embedding-secret".to_string(),
                    },
                    display_name: Some("Embedding credential".to_string()),
                    priority: 100,
                    expires_at: None,
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create embedding credential");

        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_embedding_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                EmbeddingPolicyPutRequest {
                    embedding_provider_id: Some(provider.id),
                    embedding_model_id: Some(model.id),
                    embedding_dimension: Some(1536),
                    batch_size: Some(2),
                    rag_embeddings_enabled: Some(true),
                    ..EmbeddingPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable RAG embeddings");

        ProviderFixture {
            provider_id: provider.id,
            model_id: model.id,
            credential_id: credential.id,
        }
    }

    /// Turns RAG embeddings on with a policy that names no provider or model — the
    /// misconfiguration whose honest outcome is a `'failed'` version, not an `'indexed'` one.
    pub async fn enable_rag_embeddings_without_a_provider(&self) {
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_embedding_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                EmbeddingPolicyPutRequest {
                    rag_embeddings_enabled: Some(true),
                    ..EmbeddingPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable RAG embeddings without a provider");
    }

    /// Turns retrieval on for this fixture's application (plan 11 Sub-Phase C).
    ///
    /// Thresholds default to `0.0` rather than the schema's `0.5`, and the weights to
    /// semantic-only, because a retrieval test's subject is *which rows come back*, not the
    /// blend. A test that wants to prove the threshold or the blend sets them explicitly; every
    /// other test would otherwise be silently coupled to the default weighting and would start
    /// failing for reasons unrelated to what it asserts.
    pub async fn enable_retrieval(&self, overrides: RetrievalPolicyPutRequest) {
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_retrieval_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                RetrievalPolicyPutRequest {
                    enabled: overrides.enabled.or(Some(true)),
                    memory_retrieval_enabled: overrides.memory_retrieval_enabled.or(Some(true)),
                    rag_retrieval_enabled: overrides.rag_retrieval_enabled.or(Some(true)),
                    semantic_weight: overrides.semantic_weight.or(Some(1.0)),
                    keyword_weight: overrides.keyword_weight.or(Some(0.0)),
                    recency_weight: overrides.recency_weight.or(Some(0.0)),
                    importance_weight: overrides.importance_weight.or(Some(0.0)),
                    minimum_memory_score: overrides.minimum_memory_score.or(Some(0.0)),
                    minimum_chunk_score: overrides.minimum_chunk_score.or(Some(0.0)),
                    ..overrides
                },
            )
            .await
            .expect("enable retrieval policy");
        // The conversation policy gates memory retrieval independently of the retrieval
        // policy — both have to say yes, which is the intended belt-and-braces and is easy to
        // forget when writing a fixture.
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_conversation_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                ConversationPolicyPutRequest {
                    conversations_enabled: Some(true),
                    caller_can_create_conversations: Some(true),
                    memory_enabled: Some(true),
                    memory_retrieval_enabled: Some(true),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable conversation retrieval policy");
    }

    /// Merges an embedding-policy change onto whatever is already stored.
    ///
    /// `put_embedding_policy` is a `coalesce` merge, so omitted fields are preserved — which is
    /// what lets a test turn on `memory_embeddings_enabled` or switch `failure_behavior`
    /// without restating the provider wiring `enable_rag_embeddings` put there.
    pub async fn patch_embedding_policy(&self, request: EmbeddingPolicyPutRequest) {
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_embedding_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                request,
            )
            .await
            .expect("patch embedding policy");
    }

    /// Turns manual memory writes on, so `create_memory` is reachable.
    pub async fn enable_manual_memory(&self) {
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_memory_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                MemoryPolicyPutRequest {
                    enabled: Some(true),
                    manual_memory_enabled: Some(true),
                    consent_mode: Some(MemoryConsentMode::ApplicationManaged),
                    ..MemoryPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable manual memory policy");
    }

    /// Turns automatic memory extraction on — plan 11 Sub-Phase F.
    ///
    /// **Both consent columns are set explicitly**, and that is the whole point of the
    /// signature. `application_memory_policies.consent_mode` and
    /// `application_conversation_policies.memory_consent_mode` are independent columns that both
    /// default to `'explicit_only'`, and extraction takes the more restrictive of the two. A
    /// fixture that set only one would leave the other at its default and every "this mode
    /// produces active memories" case would fail for a reason unrelated to what it asserts —
    /// or, worse, a fixture that set only the memory policy would make a test for the
    /// conversation policy's branch pass without exercising it.
    pub async fn enable_memory_extraction(
        &self,
        conversation_mode: MemoryConsentMode,
        memory_mode: MemoryConsentMode,
        overrides: MemoryPolicyPutRequest,
    ) {
        let conversations = ConversationService::new(&self.state).expect("conversation service");
        conversations
            .put_memory_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                MemoryPolicyPutRequest {
                    enabled: Some(true),
                    automatic_extraction_enabled: Some(true),
                    consent_mode: Some(memory_mode),
                    ..overrides
                },
            )
            .await
            .expect("enable memory extraction policy");
        conversations
            .put_conversation_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                ConversationPolicyPutRequest {
                    conversations_enabled: Some(true),
                    caller_can_create_conversations: Some(true),
                    memory_enabled: Some(true),
                    memory_extraction_enabled: Some(true),
                    memory_consent_mode: Some(conversation_mode),
                    ..ConversationPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable conversation extraction policy");
    }

    /// Turns conversation summarization on — plan 11 Sub-Phase E.
    ///
    /// Call this **after** `enable_public_streaming`, which writes its own conversation policy
    /// and would otherwise reset `summarization_enabled` back to its `false` default. The same
    /// ordering trap `enable_memory_extraction` documents.
    ///
    /// The thresholds default low here (one message, one token) so a case that is about
    /// *summarizing* does not have to manufacture eight turns of conversation first. A case that
    /// is about the **thresholds** passes its own values, which is the only way to be sure the
    /// threshold branch is exercised rather than assumed.
    pub async fn enable_summarization(&self, overrides: ConversationPolicyPutRequest) {
        ConversationService::new(&self.state)
            .expect("conversation service")
            .put_conversation_policy(
                &self.actor,
                &request_context(),
                self.application_id,
                ConversationPolicyPutRequest {
                    conversations_enabled: Some(true),
                    caller_can_create_conversations: Some(true),
                    // `.or(…)`, not a literal. Struct-update syntax makes an explicit field win
                    // over `..overrides`, so writing `summarization_enabled: Some(true)` here
                    // would make the parameter *unusable* for the three fields a caller most
                    // needs to set — and a case passing `Some(false)` would silently get `true`
                    // and assert against a premise that never held.
                    summarization_enabled: overrides.summarization_enabled.or(Some(true)),
                    summary_trigger_tokens: overrides.summary_trigger_tokens.or(Some(1)),
                    minimum_messages_since_summary: overrides
                        .minimum_messages_since_summary
                        .or(Some(1)),
                    ..overrides
                },
            )
            .await
            .expect("enable summarization policy");
    }

    /// A consumer key carrying exactly the scopes given.
    ///
    /// `enable_public_streaming`'s key deliberately does **not** hold
    /// `moira:conversations:write`, which is the scope
    /// `POST /api/v1/conversations/{id}/summarize` requires — and that omission is load-bearing,
    /// not an oversight: it is what a real caller-plane key looks like, and it is why
    /// summarization's automatic path must not sit behind that scope check. A case that drives
    /// the *manual* endpoint needs a key that has it, so it asks for one here rather than
    /// widening the shared fixture and destroying the distinction.
    pub async fn consumer_key_with_scopes(&self, scopes: &[&str]) -> String {
        AdminService::new(&self.state)
            .expect("admin service")
            .create_consumer_key(
                &self.actor,
                &request_context(),
                ConsumerKeyCreateRequest {
                    application_id: self.application_id,
                    display_name: format!("scoped client {}", Uuid::now_v7().simple()),
                    scopes: scopes.iter().map(|scope| scope.to_string()).collect(),
                    expires_at: None,
                },
            )
            .await
            .expect("create scoped consumer key")
            .secret
            .expect("consumer key secret")
    }

    /// A caller-plane actor bound to this fixture's application, with an explicit scope.
    ///
    /// Built directly rather than issued as a consumer key because a consumer-key actor carries
    /// no `external_tenant_id`/`external_user_id` — `authenticate_caller` returns from
    /// `verify_api_key` before the `x-moira-*` header path is reached — and the tenant/user
    /// isolation cases have nothing to assert without them. The scope under test is a bound
    /// parameter derived from these three fields, so constructing them directly tests exactly
    /// the thing, and the HTTP e2e in `tests/rag_retrieval_end_to_end.rs` covers the
    /// authentication path that populates them.
    pub fn caller_actor(&self, tenant: Option<&str>, user: Option<&str>) -> Actor {
        Actor {
            actor_type: ActorType::ConsumerKey,
            subject: user.map(str::to_string),
            tenant_id: tenant.map(str::to_string),
            application_id: Some(self.application_id.to_string()),
            external_user_id: user.map(str::to_string),
            external_tenant_id: tenant.map(str::to_string),
            internal_application_id: Some(self.application_id),
            scopes: vec![
                "moira:conversations:create".to_string(),
                "moira:conversations:read".to_string(),
                "moira:memories:create".to_string(),
                "moira:memories:read".to_string(),
            ],
            ..Actor::default()
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
                        // Needed by any second turn: `prepare_response_conversation` reads the
                        // existing conversation rather than creating one, and that read is
                        // scope-checked. Without it a multi-turn conversation gets a 403 on
                        // turn two, which is the shape plan 11's history replay depends on.
                        "moira:conversations:read".to_string(),
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
    request_context_with_idempotency_key(None)
}

/// A `RequestContext` carrying an `Idempotency-Key`, as `RequestContext::from_headers`
/// would build it for a request that sent the header.
///
/// `request_context()` hard-codes `idempotency_key: None`, which silently made every
/// fixture-driven service call non-idempotent — the reason no test exercised the
/// `/v1/responses` or runtime-admin idempotency ledgers at all before plan 03's review.
/// Tests that drive those paths over real HTTP send the header instead; this exists for
/// direct service-level calls.
pub fn request_context_with_idempotency_key(key: Option<&str>) -> RequestContext {
    RequestContext {
        request_id: format!("test-{}", Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("moira-lifecycle-test".to_string()),
        idempotency_key: key.map(str::to_string),
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

// ---------------------------------------------------------------------------
// Per-fixture database isolation (plan 06, module 13 — P2-13)
// ---------------------------------------------------------------------------
//
// Every DB-backed suite used to share one `moira` database, guarded only by a
// process-global mutex. That mutex is per test *binary*, so it never constrained two
// suites against each other at all, and even inside one binary it only ordered
// fixtures — it could not stop a test from observing rows a neighbour had committed.
// `tests/admin_query_contract.rs`'s
// `defined_but_unsupported_page_query_field_is_accepted_and_ignored` failed under
// `cargo test --workspace` and passed in isolation for exactly that reason, and said
// so in its own comment.
//
// The fix is one database per fixture, so there is no shared mutable state left to
// serialise.
//
// **Schema isolation — the cheaper, more obvious option — is not available here, and
// this was verified rather than assumed.** `migrations/0003_security_foundation.sql`
// hard-codes `public.` in eleven places (`to_regclass('public.…')`,
// `table_schema = 'public'`) to detect and rename the legacy tables migration 0002
// creates. Run under a non-`public` `search_path`, those guards inspect the wrong
// schema, decline to rename, and the `create table if not exists` statements that
// follow then bind to the un-renamed legacy tables; 0003 dies on
// `column "deleted_at" does not exist`. Editing 0003 is not an option either — it is
// already applied to every existing database and `sqlx` checksums migration files.
//
// So: one database per fixture, cloned from a migrated template with
// `CREATE DATABASE … TEMPLATE …`. That is a file-level copy — measured at roughly
// 50 ms for this 10 MB schema, an order of magnitude cheaper than replaying all eleven
// migrations — which is what makes per-test databases affordable at all.
// `tests/security_foundation.rs` already creates and force-drops a whole database per
// run; this is the same strategy, made cheap enough to use per test rather than per
// suite. It needs `CREATEDB` on the role in `MOIRA_TEST_DATABASE_URL`, which that
// suite already required.
//
// Two properties come free with a private database, and tests now rely on both:
// PostgreSQL advisory locks are database-scoped, so `tests/admin_idempotency.rs`'s
// advisory-lock races can no longer collide with another suite's; and `LISTEN`/`NOTIFY`
// channels are database-wide but not cluster-wide, so the `moira_runtime_config`
// notifications the runtime-config triggers emit stay inside the fixture that caused
// them (module 13 item 4 — no fixture spawns `spawn_runtime_config_listener` today,
// and with per-fixture databases one that did would still only hear itself).
//
// This once excluded four suites that built their own pool straight against
// `MOIRA_TEST_DATABASE_URL`, on the grounds that they "assert only on rows they created,
// keyed by a per-run `Uuid`". That was true and beside the point: asserting *safely* on a
// shared database is not the same as leaving nothing behind on it. Two of them —
// `tests/jwks_hardening.rs` and `tests/http_middleware_contract.rs` — were leaking every
// row they wrote, on the happy path, for as long as they had existed: 160
// `trusted_jwt_issuers` rows and 42 `active` `moira:admin` API keys by the time anyone
// counted (findings F27 and F10 item 2). Both now take a `TestDatabase`.
//
// One suite remains, and `tests/test_database_isolation.rs` is the guard that keeps that
// list from growing silently:
//
// * `tests/security_foundation.rs` — asserts the migration contract itself, so it must
//   migrate from nothing rather than clone an already-migrated template. It creates and
//   force-drops its own database per run, or migrates a pre-provisioned empty one named by
//   `MOIRA_TEST_MIGRATION_DATABASE_URL` when the role has no `CREATEDB` (issue #77).
//
// `tests/retention_worker.rs` was the second entry until finding F10 item 1 was closed: the
// sweep it asserts on is database-wide, never cluster-wide, so a private clone made its
// counts exactly assertable rather than weakening them.
//
// **The library crate's own `#[cfg(test)]` modules were the last hole** and are closed the
// same way by `src/test_support.rs` (issue #77): they used to run `MIGRATOR` against
// `MOIRA_TEST_DATABASE_URL` itself, so one checkout's unmerged migration broke every other
// checkout's lib tests. They now get one private database per test process.
//
// No test mixes its own pool with a `LifecycleFixture`, and none takes two fixtures at
// once — `FIXTURE_BUDGET` is a bounded semaphore, so a test holding two permits while
// others wait for their first would be a deadlock rather than a slowdown.

const MAINTENANCE_DATABASE: &str = "postgres";
pub const TEMPLATE_PREFIX: &str = "moira_test_template_";

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Session advisory lock guarding the template database.
///
/// Held **exclusively** while the template is created or migrated, because an open
/// connection to it makes another process's `CREATE DATABASE … TEMPLATE` fail with
/// *"source database is being accessed by other users"*. Held **shared** while a
/// fixture clones it, so clones run in parallel with each other but never during
/// maintenance. Advisory locks are database-scoped, and this one is always taken on
/// the maintenance database, so every test process agrees on it.
pub const TEMPLATE_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiratdb");

/// A fixture database this old with no live connection is a leak from a killed test
/// process. Sweeping is bounded by age *and* by connection count because a database
/// is briefly connectionless between `CREATE DATABASE` and the fixture pool's first
/// connection; an hour is several orders of magnitude beyond that window.
pub const LEAKED_DATABASE_GRACE_SECONDS: u64 = 3_600;

/// How long a template may go unclaimed before another worktree may reclaim it.
///
/// Deliberately the same order as [`LEAKED_DATABASE_GRACE_SECONDS`] and for the same
/// reason: it has to be far longer than the thing it must not interrupt. Every test
/// binary refreshes the marker on the template it is about to clone from
/// ([`prepare_template`]), and a `cargo test --workspace` run starts dozens of binaries
/// over a few minutes, so a live neighbour re-stamps its template one to two orders of
/// magnitude more often than this.
pub const TEMPLATE_IDLE_GRACE_SECONDS: u64 = 3_600;

/// The comment written on every template database this harness builds or clones from.
///
/// This is the *owning-run marker* `plans/reports/EXECUTION-LEDGER.md` asks for. Before
/// it, [`sweep_leaked_databases`] dropped every `moira_test_template_*` that was not the
/// calling process's own, on the theory that a template with no client backend is idle —
/// but an idle-between-fixtures neighbour looks exactly like that, so two worktrees whose
/// `migrations/` differ by one file destroyed each other's templates mid-run and produced
/// a `3D000` indistinguishable from a real regression.
///
/// A PostgreSQL database carries no creation or access timestamp, and `pg_stat_activity`
/// answers a different question, so the signal has to be written down. A shared comment
/// (`COMMENT ON DATABASE`, read back through `shobj_description`) is the cheapest place
/// that survives a process death, needs no extra table, and is owned by the same role
/// that owns the database.
pub const TEMPLATE_MARKER_PREFIX: &str = "moira-test-template last-used=";

/// The marker text a template carries when it was last claimed at `epoch_seconds`.
pub fn template_marker(epoch_seconds: u64) -> String {
    format!("{TEMPLATE_MARKER_PREFIX}{epoch_seconds}")
}

/// The current wall-clock second, as the marker and the fixture-name grammar encode it.
pub fn now_epoch_seconds() -> u64 {
    unix_seconds()
}

/// A connection to the maintenance database this harness performs `CREATE`/`DROP DATABASE`
/// on, plus the template name the calling tree's migration set resolves to.
///
/// `None` only when the [`ALLOW_NO_DATABASE`] opt-out is in force — otherwise resolving the
/// origin panics, as every other entry point does.
///
/// Exists so `tests/test_database_sweep.rs` can drive [`sweep_leaked_databases`] against
/// fabricated leaks without resolving `MOIRA_TEST_DATABASE_URL` itself, which
/// `tests/test_database_isolation.rs` allows in exactly two files.
pub async fn maintenance_connection() -> Option<(PgConnection, String)> {
    let origin = database_origin().await?;
    let connection = connect_maintenance(&origin.url_for(MAINTENANCE_DATABASE)).await;
    Some((connection, origin.template.clone()))
}

/// Concurrent fixtures per test binary.
///
/// This is **not** an isolation device — the database is. It exists only to bound
/// PostgreSQL connections: the server ships with `max_connections = 100`, and each
/// fixture pool may open eight. Raising it trades headroom on the server for test
/// wall-clock; lowering it to 1 restores the old serialised behaviour without
/// changing any assertion.
const CONCURRENT_FIXTURES: usize = 4;

static FIXTURE_BUDGET: Semaphore = Semaphore::const_new(CONCURRENT_FIXTURES);
static DATABASE_ORIGIN: OnceCell<Option<DatabaseOrigin>> = OnceCell::const_new();

/// The template's name, derived from the migration set it was built from.
///
/// Naming the template after a digest of every migration's version and checksum makes
/// "is this template current?" answerable from `pg_database` alone, so a process that
/// finds it present never has to **connect** to it. That matters: a connection to the
/// template is the one thing that makes another process's
/// `CREATE DATABASE … TEMPLATE` fail, and the first version of this harness opened one
/// per test process purely to re-run an already-applied migration set. It also removes
/// the failure mode where a template built from an older migration set is silently
/// reused — adding a migration changes the digest, so a fresh template is built instead.
fn template_database() -> String {
    let mut digest = Sha256::new();
    for migration in MIGRATOR.iter() {
        digest.update(migration.version.to_be_bytes());
        digest.update(migration.checksum.as_ref());
    }
    let digest = digest.finalize();
    let mut name = String::from(TEMPLATE_PREFIX);
    for byte in &digest[..8] {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

/// The database `MOIRA_TEST_DATABASE_URL` names — the one every suite would share if it
/// opened its own pool on it.
///
/// Exposed so a suite can assert it is *not* connected to that database without resolving
/// the variable itself. `tests/test_database_isolation.rs` permits exactly two files to
/// make that lookup, and this module is the one that owns the mechanism; a suite doing it
/// inline would be a new entry on that allowlist for no reason beyond a diagnostic.
pub fn shared_database_name() -> Option<String> {
    let url = env::var("MOIRA_TEST_DATABASE_URL").ok()?;
    Some(
        Url::parse(&url)
            .ok()?
            .path()
            .trim_start_matches('/')
            .to_string(),
    )
}

/// `MOIRA_TEST_DATABASE_URL` with the database name factored out, so a URL can be
/// built for the maintenance database, the template, or any fixture clone.
#[derive(Clone, Debug)]
struct DatabaseOrigin {
    base: Url,
    template: String,
}

impl DatabaseOrigin {
    fn url_for(&self, database: &str) -> String {
        let mut url = self.base.clone();
        url.set_path(database);
        url.to_string()
    }
}

/// A private PostgreSQL database owned by exactly one fixture, dropped when the
/// fixture is — including when the test panics.
pub struct TestDatabase {
    pub pool: PgPool,
    name: String,
    maintenance_url: String,
    /// Released after [`Drop::drop`] has torn the database down, so the budget
    /// reflects databases that actually exist.
    _budget: SemaphorePermit<'static>,
}

impl TestDatabase {
    /// A fixture database with the pool size `LifecycleFixture` needs.
    pub async fn create() -> Option<Self> {
        Self::create_with_max_connections(8).await
    }

    /// As [`TestDatabase::create`], for a suite whose concurrency needs a larger pool
    /// (`tests/admin_idempotency.rs` fires barrier-released requests in batches).
    pub async fn create_with_max_connections(max_connections: u32) -> Option<Self> {
        // Taken before anything else so the budget covers the clone itself, not just
        // the fixture's lifetime.
        let budget = FIXTURE_BUDGET
            .acquire()
            .await
            .expect("the fixture budget semaphore is never closed");
        let origin = database_origin().await?;
        let name = format!("moira_test_{}_{}", unix_seconds(), Uuid::now_v7().simple());
        let maintenance_url = origin.url_for(MAINTENANCE_DATABASE);
        let mut maintenance = connect_maintenance(&maintenance_url).await;
        sqlx::query("select pg_advisory_lock_shared($1)")
            .bind(TEMPLATE_LOCK_KEY)
            .execute(&mut maintenance)
            .await
            .expect("take a shared lock on the template database");
        clone_template(&mut maintenance, &maintenance_url, &origin, &name).await;
        let pool = timeout(
            DATABASE_TIMEOUT,
            PgPoolOptions::new()
                .max_connections(max_connections)
                .connect(&origin.url_for(&name)),
        )
        .await
        .expect("fixture database connection timed out")
        .expect("connect the fixture database");
        // Only now: closing the connection releases the shared lock, and holding it
        // until the pool is live keeps the leak sweep from seeing a connectionless
        // database that is about to be used.
        let _ = maintenance.close().await;

        Some(Self {
            pool,
            name,
            maintenance_url,
            _budget: budget,
        })
    }

    /// The database's name, for diagnostics — a failing test that names its database
    /// can be reproduced against the surviving copy if teardown is disabled.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let maintenance_url = self.maintenance_url.clone();
        let name = self.name.clone();
        // `Drop` is synchronous and `#[tokio::test]` defaults to a current-thread
        // runtime, which rules out `block_in_place`. A dedicated thread with its own
        // runtime is therefore the only way to make teardown unconditional — and
        // unconditional is the entire point: this must run when the test panics, which
        // is precisely when a leaked database would otherwise be left behind.
        //
        // `with (force)` terminates the fixture pool's own sessions; the pool is never
        // used again after this point.
        let outcome = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| err.to_string())?;
            runtime.block_on(async move {
                let mut connection = PgConnection::connect(&maintenance_url)
                    .await
                    .map_err(|err| err.to_string())?;
                sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
                    .execute(&mut connection)
                    .await
                    .map_err(|err| err.to_string())?;
                connection.close().await.map_err(|err| err.to_string())
            })
        })
        .join();

        let failure = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(err)) => err,
            Err(_) => "the teardown thread panicked".to_string(),
        };
        let message = format!(
            "failed to drop fixture database {}: {failure}. It has leaked and will be \
             swept on a later run once it is {LEAKED_DATABASE_GRACE_SECONDS}s old.",
            self.name
        );
        eprintln!("{message}");
        // Panicking while already unwinding aborts the process and would replace a
        // real test failure with an unreadable abort, so fail loudly only when this is
        // the sole problem.
        if !std::thread::panicking() {
            panic!("{message}");
        }
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is after the Unix epoch")
        .as_secs()
}

/// The creation time encoded in a fixture database's name, if it has the shape this
/// harness generates. Anything else is left alone by the sweep.
fn fixture_database_age_seconds(name: &str) -> Option<u64> {
    let created = name
        .strip_prefix("moira_test_")?
        .split('_')
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(unix_seconds().saturating_sub(created))
}

async fn connect_maintenance(maintenance_url: &str) -> PgConnection {
    timeout(DATABASE_TIMEOUT, PgConnection::connect(maintenance_url))
        .await
        .expect("maintenance database connection timed out")
        .expect("connect the maintenance database")
}

/// The one deliberately-named way to run this tree's tests with no database at all.
///
/// Spelled out rather than inferred, and checked by value, because everything this
/// variable disables is a *database-backed assertion* — the suites that carry almost all
/// of Moira's real coverage. See [`refuse_to_run_without_a_database`] for why the default
/// is now a failure.
pub const ALLOW_NO_DATABASE: &str = "MOIRA_TEST_ALLOW_NO_DATABASE";

/// Whether the opt-out is in force.
///
/// `CI=true` overrides it unconditionally: a CI run that cannot reach a database has a
/// broken service container, and letting a workflow file set the opt-out would put the
/// whole gate one YAML line away from testing nothing.
pub fn no_database_opt_out_is_set() -> bool {
    if env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) {
        return false;
    }
    env::var(ALLOW_NO_DATABASE)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
}

/// Announces a skip on the **process's** stderr, not the test harness's captured one.
///
/// # This is not a stylistic preference
///
/// `libtest` captures `eprintln!` per test and prints it only for tests that **fail**, so a
/// skip announced with `eprintln!` from a passing test never reaches the log at all.
/// Measured on this branch: with the opt-out in force, `cargo test --test retention_worker`
/// redirected to a file contained **zero** occurrences of `skipping`, while all eight tests
/// reported `ok`. `scripts/gates.sh`'s zero-skip assertion greps that file — so for every
/// suite whose skip is announced from inside a passing test on the test's own thread, the
/// assertion could not have fired.
///
/// `libtest`'s capture is installed on `std::io::_print`/`_eprint`, which the `print!`
/// family routes through. Writing to `std::io::stderr()` directly bypasses it and reaches
/// the real file descriptor, which is what the gate reads.
pub fn announce_skip(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// The message a suite dies with when no database is configured and the opt-out is absent.
///
/// # Why the absence of a database is a failure and not a skip
///
/// It used to print one `eprintln!` and return `None`, and every database-backed test in
/// the tree then returned early **and reported success**. A whole gate run could be green
/// while asserting nothing: `plans/reports/EXECUTION-LEDGER.md` records a round of results
/// invalidated exactly this way, by a `MOIRA_TEST_DATABASE_URL` that pointed at a database
/// which was not there.
///
/// `scripts/gates.sh` grew an assertion that no suite printed a skip line, which caught it
/// — but only for people who run the gates, and only as long as the skip text keeps
/// matching the grep. That is the wrong place for the guarantee to live. The suite itself
/// now refuses, so the gate's check is a second line of defence rather than the only one.
pub fn refuse_to_run_without_a_database(reason: &str) -> ! {
    panic!(
        "{reason}\n\n\
         Database-backed tests refuse to run without a database. Start PostgreSQL and set \
         MOIRA_TEST_DATABASE_URL, for example:\n\n    \
         export MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/moira\n\n\
         The role it names needs CREATEDB: every fixture clones its own private database \
         from a migrated template and drops it again.\n\n\
         If you genuinely mean to run the suite with no database — knowing that this \
         silently removes almost all of Moira's coverage — set {ALLOW_NO_DATABASE}=1. It is \
         ignored when CI=true, and the skip line it prints still reds `scripts/gates.sh`."
    )
}

/// Resolves `MOIRA_TEST_DATABASE_URL` and readies the template, once per process.
///
/// Fail-closed by default now, not only under `CI=true`: see
/// [`refuse_to_run_without_a_database`].
async fn database_origin() -> Option<DatabaseOrigin> {
    DATABASE_ORIGIN
        .get_or_init(|| async {
            let database_url = match env::var("MOIRA_TEST_DATABASE_URL") {
                Ok(value) if !value.trim().is_empty() => value,
                _ if no_database_opt_out_is_set() => {
                    announce_skip(&format!(
                        "skipping database-backed tests: MOIRA_TEST_DATABASE_URL is not set \
                         and {ALLOW_NO_DATABASE} is in force"
                    ));
                    return None;
                }
                _ => refuse_to_run_without_a_database(
                    "MOIRA_TEST_DATABASE_URL is not set (or is empty).",
                ),
            };
            let origin = DatabaseOrigin {
                base: Url::parse(&database_url)
                    .expect("MOIRA_TEST_DATABASE_URL must be a valid URL"),
                template: template_database(),
            };
            prepare_template(&origin).await;
            Some(origin)
        })
        .await
        .clone()
}

/// Builds the template if this migration set has never been templated, and sweeps
/// databases earlier runs leaked. Once per process, under the exclusive lock.
///
/// The common case is that the template already exists, and then this opens **no**
/// connection to it at all — the digest in its name is the proof that it is current,
/// so there is nothing to verify by connecting. That is deliberate: an open connection
/// to the template is the one condition that makes another process's
/// `CREATE DATABASE … TEMPLATE` fail, and a per-process re-migration created that
/// window on every single test binary for no benefit.
/// Clones the template, rebuilding it once if a neighbour dropped it mid-run.
///
/// # Why the retry is not paranoia
///
/// [`sweep_leaked_databases`] drops **every** `moira_test_template_*` that is not the calling
/// process's own, and [`template_database`] names the template after a SHA-256 of the migration
/// set. Two worktrees whose `migrations/` differ by one file therefore have different template
/// names and each one's [`prepare_template`] destroys the other's.
///
/// The shared advisory lock does not close that window. `prepare_template` runs **once** per
/// process; every fixture afterwards holds the shared lock only for the length of its own
/// `create database … template …`. Between the prepare and the last fixture there is nothing
/// keeping the template alive, so a neighbour sweeping in that gap leaves this suite cloning from
/// a database that no longer exists:
///
/// ```text
/// clone the migrated template database: … code: "3D000",
/// message: "template database \"moira_test_template_…\" does not exist"
/// ```
///
/// Observed twice on one branch on 2026-08-01, reddening `scripts/gates.sh` with different tests
/// each time and no code change between the runs — which is the dangerous part, because it is
/// indistinguishable from a real regression until you read the panic.
///
/// Rebuilding and retrying **once** is deliberate: a second `3D000` means the neighbour is
/// sweeping faster than a template can be built, which is a real environmental problem and should
/// fail loudly rather than spin. Nothing about the sweep's semantics changes here — stale
/// templates are still reclaimed — so this cannot mask a genuine schema drift.
async fn clone_template(
    maintenance: &mut PgConnection,
    maintenance_url: &str,
    origin: &DatabaseOrigin,
    name: &str,
) {
    let sql = format!(
        "create database \"{name}\" template \"{}\"",
        origin.template
    );

    let Err(error) = sqlx::raw_sql(&sql).execute(&mut *maintenance).await else {
        return;
    };
    if !is_undefined_database(&error) {
        panic!("clone the migrated template database: {error:?}");
    }

    // Rebuild under the *exclusive* lock, on its own connection: this one still holds the shared
    // lock, and `build_template` must not run while another process is cloning.
    let mut rebuild = connect_maintenance(maintenance_url).await;
    sqlx::query("select pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut rebuild)
        .await
        .expect("take the exclusive template maintenance lock for a rebuild");
    build_template(&mut rebuild, origin).await;
    mark_template_in_use(&mut rebuild, &origin.template).await;
    sqlx::query("select pg_advisory_unlock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut rebuild)
        .await
        .expect("release the template maintenance lock after a rebuild");
    let _ = rebuild.close().await;

    sqlx::raw_sql(&sql)
        .execute(maintenance)
        .await
        .expect("clone the migrated template database after rebuilding it");
}

/// `3D000` — the object the statement named is not there. Here: the template a neighbouring
/// worktree swept out from under this clone.
fn is_undefined_database(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code == "3D000")
}

async fn prepare_template(origin: &DatabaseOrigin) {
    let maintenance_url = origin.url_for(MAINTENANCE_DATABASE);
    let mut maintenance = connect_maintenance(&maintenance_url).await;
    sqlx::query("select pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut maintenance)
        .await
        .expect("take the exclusive template maintenance lock");

    sweep_leaked_databases(&mut maintenance, &origin.template).await;
    build_template(&mut maintenance, origin).await;
    // The claim, refreshed once per test binary. This is what keeps a neighbouring
    // worktree's sweep off a template this process is about to clone from dozens of
    // times: the marker says "in use as of now", and `template_sweep_verdict` spares it
    // for a whole grace period afterwards.
    mark_template_in_use(&mut maintenance, &origin.template).await;

    sqlx::query("select pg_advisory_unlock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut maintenance)
        .await
        .expect("release the template maintenance lock");
    let _ = maintenance.close().await;
}

async fn build_template(maintenance: &mut PgConnection, origin: &DatabaseOrigin) {
    let template = &origin.template;
    let present: Option<i32> = sqlx::query_scalar("select 1 from pg_database where datname = $1")
        .bind(template)
        .fetch_optional(&mut *maintenance)
        .await
        .expect("look up the template database");
    if present.is_some() {
        return;
    }

    // Built under a name nobody clones from, and renamed only once the migrations have
    // succeeded and the connection is gone. A crash midway therefore leaves a partial
    // database that no fixture can ever be cloned from, rather than a template that
    // looks current and is missing tables.
    let scratch = format!("moira_test_building_{}", Uuid::now_v7().simple());
    sqlx::raw_sql(&format!("create database \"{scratch}\""))
        .execute(&mut *maintenance)
        .await
        .expect("create the scratch database for the template");
    let pool = timeout(
        DATABASE_TIMEOUT,
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&origin.url_for(&scratch)),
    )
    .await
    .expect("template database connection timed out")
    .expect("connect the template database");
    timeout(DATABASE_TIMEOUT, MIGRATOR.run(&pool))
        .await
        .expect("template migrations timed out")
        .expect("migrate the template database");
    pool.close().await;
    // `Pool::close` returns once sqlx has sent `Terminate`; PostgreSQL reaps the backend
    // a moment later, and `ALTER DATABASE … RENAME` refuses while any session remains.
    // Wait for the observable condition rather than assuming the reap has happened.
    wait_for_no_connections(&mut *maintenance, &scratch).await;
    sqlx::raw_sql(&format!(
        "alter database \"{scratch}\" rename to \"{template}\""
    ))
    .execute(&mut *maintenance)
    .await
    .expect("publish the migrated template database");
}

/// Waits until no session other than this one is attached to `database`.
///
/// `pg_backend_pid()` excludes the caller, and `backend_type` excludes autovacuum
/// workers and background writers — neither is a client session, and neither blocks
/// `CREATE DATABASE … TEMPLATE` or `ALTER DATABASE … RENAME`.
async fn wait_for_no_connections(maintenance: &mut PgConnection, database: &str) {
    const CONNECTIONS: &str = "select coalesce(string_agg(\
             pid || ' ' || coalesce(state, '?') || ' ' || left(coalesce(query, '?'), 60), '; '\
         ), '') \
         from pg_stat_activity \
         where datname = $1 and pid <> pg_backend_pid() and backend_type = 'client backend'";

    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    let mut last = String::new();
    let settled = timeout(DATABASE_TIMEOUT, async {
        loop {
            ticker.tick().await;
            last = sqlx::query_scalar(CONNECTIONS)
                .bind(database)
                .fetch_one(&mut *maintenance)
                .await
                .expect("inspect connections to the template database");
            if last.is_empty() {
                return;
            }
        }
    })
    .await
    .is_ok();
    assert!(
        settled,
        "sessions on {database} were never released; still attached after \
         {DATABASE_TIMEOUT:?}: {last}"
    );
}

/// What the sweep should do with a template database that is not this process's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateVerdict {
    /// Someone stamped it recently. Leave it alone — it belongs to a live neighbour.
    Spare,
    /// Marked, and untouched for longer than [`TEMPLATE_IDLE_GRACE_SECONDS`]. Reclaim it.
    Drop,
    /// No marker this harness recognises. Spare it *and* stamp it now, so it becomes
    /// reclaimable one grace period from here instead of never.
    ///
    /// Without this arm the sweep would trade a flake for a disk leak: templates built
    /// before the marker existed, or by a checkout that predates it, would survive
    /// forever at ~10 MB each.
    Adopt,
}

/// The epoch second encoded in a template's marker comment, if it carries one.
fn template_last_used(comment: Option<&str>) -> Option<u64> {
    comment?
        .trim()
        .strip_prefix(TEMPLATE_MARKER_PREFIX)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

/// The sweep's decision for one foreign template, as a pure function of its marker.
///
/// Exposed — and exercised directly by `tests/test_database_sweep.rs` — because the
/// property that matters ("a live neighbour's template survives, a genuinely leaked one
/// does not") is a property of this decision, and asserting it through a whole
/// `CREATE DATABASE`/sweep cycle only is slower and proves less about the boundaries.
pub fn template_sweep_verdict(comment: Option<&str>, now: u64) -> TemplateVerdict {
    match template_last_used(comment) {
        None => TemplateVerdict::Adopt,
        Some(last_used) if now.saturating_sub(last_used) >= TEMPLATE_IDLE_GRACE_SECONDS => {
            TemplateVerdict::Drop
        }
        Some(_) => TemplateVerdict::Spare,
    }
}

/// Stamps `template` with the current time, claiming it for however long the grace period
/// lasts.
///
/// Best effort on purpose: the marker is an optimisation over "never reclaim anything",
/// and a role that cannot comment on a database it does not own must not turn that into a
/// failed test run. A template that stays unmarked is simply adopted by whoever sweeps next.
async fn mark_template_in_use(maintenance: &mut PgConnection, template: &str) {
    let marker = format!("{TEMPLATE_MARKER_PREFIX}{}", unix_seconds());
    let _ = sqlx::raw_sql(&format!("comment on database \"{template}\" is '{marker}'"))
        .execute(&mut *maintenance)
        .await;
}

/// Drops databases left behind by a test process that died before `Drop` could run.
///
/// Runs under the exclusive template lock, so no other process can be mid-clone, and
/// only ever considers databases with **no** session attached. Three shapes are swept:
///
/// * `moira_test_<epoch>_<uuid>` — a fixture database. Also required to be older than
///   the grace period, because a fixture database is briefly connectionless between
///   `CREATE DATABASE` and its pool's first connection.
/// * `moira_test_building_<uuid>` — a template build that crashed before its rename.
///   No grace period: nothing ever connects to one of these except the builder, which
///   holds the same exclusive lock this does.
/// * `moira_test_template_<digest>` — a template for a migration set this cluster has
///   not seen used for [`TEMPLATE_IDLE_GRACE_SECONDS`], judged by its marker comment
///   rather than by its connection count. See [`TEMPLATE_MARKER_PREFIX`] and
///   [`template_sweep_verdict`]: "no client backend" cannot tell an idle neighbour from
///   a leak, which is what made this the cross-worktree hazard the ledger recorded.
///
/// `pub` so `tests/test_database_sweep.rs` can drive it against fabricated leaks. Callers
/// must hold the exclusive [`TEMPLATE_LOCK_KEY`] on `maintenance`.
pub async fn sweep_leaked_databases(maintenance: &mut PgConnection, current_template: &str) {
    let idle: Vec<(String, Option<String>)> = sqlx::query_as(
        "select d.datname, shobj_description(d.oid, 'pg_database') from pg_database d \
         where d.datname like 'moira\\_test\\_%' \
           and d.datname <> $1 \
           and not exists (\
                 select 1 from pg_stat_activity a \
                 where a.datname = d.datname and a.backend_type = 'client backend')",
    )
    .bind(current_template)
    .fetch_all(&mut *maintenance)
    .await
    .expect("list candidate fixture databases");

    let now = unix_seconds();
    for (name, comment) in idle {
        if name.starts_with(TEMPLATE_PREFIX) {
            match template_sweep_verdict(comment.as_deref(), now) {
                TemplateVerdict::Spare => continue,
                TemplateVerdict::Adopt => {
                    mark_template_in_use(&mut *maintenance, &name).await;
                    continue;
                }
                TemplateVerdict::Drop => {}
            }
        } else if !name.starts_with("moira_test_building_")
            && !fixture_database_age_seconds(&name)
                .is_some_and(|age| age >= LEAKED_DATABASE_GRACE_SECONDS)
        {
            continue;
        }
        // Best effort: another process may have taken the database between the query
        // and the drop, and a leaked database is not worth failing a test run over.
        let _ = sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
            .execute(&mut *maintenance)
            .await;
    }
}

// ---------------------------------------------------------------------------
// Log capture (added by plan 05, Module 5 — the leak snapshot suites)
// ---------------------------------------------------------------------------
//
// There was no log-capture precedent anywhere under `tests/` before this: nothing
// in the repository installed a `tracing` subscriber from a test, so "does a
// secret reach the logs?" was an unasked question. This is the one shared piece
// the two leak suites need, so it lives here rather than being duplicated.
//
// It installs a **process-global** subscriber, deliberately. The thread-local
// alternative (`tracing::subscriber::with_default`) sees nothing emitted from a
// `tokio::spawn`ed task, which is where a large share of Moira's logging happens
// (the worker supervisor, the JWKS refresher, the streaming supervisor) — a leak
// suite that silently cannot observe those paths is worse than none. A global
// default may only be installed once per process; `Once` plus the ignored
// `set_global_default` error makes a second call a no-op instead of a panic, and
// each `tests/*.rs` file is its own process.
//
// The buffer is cumulative and shared by every test in the binary. That is the
// intended behaviour for an *absence* assertion: a canary must appear in no
// event emitted by any test, not merely in the ones a given test triggered.

/// Event targets Moira's production subscriber drops at verbose levels.
///
/// Must stay in step with `PAYLOAD_BEARING_LOG_TARGET_PREFIXES` in `src/config/telemetry.rs`;
/// the unit tests there pin the production behaviour, and this list is what lets the e2e leak
/// suites assert against the same reality rather than against a subscriber Moira never installs.
const SUPPRESSED_LOG_TARGETS: &[&str] = &["rig::", " rig:"];

static LOG_BUFFER: LazyLock<Arc<StdMutex<Vec<u8>>>> =
    LazyLock::new(|| Arc::new(StdMutex::new(Vec::new())));
static LOG_CAPTURE_INIT: Once = Once::new();

/// A handle onto the process-global captured `tracing` output.
#[derive(Clone)]
pub struct CapturedLogs {
    buffer: Arc<StdMutex<Vec<u8>>>,
}

impl CapturedLogs {
    /// Everything formatted by the subscriber so far, as text.
    pub fn contents(&self) -> String {
        let guard = self.buffer.lock().expect("captured log buffer poisoned");
        String::from_utf8_lossy(guard.as_slice()).into_owned()
    }

    /// Everything the capture holds **except** events from targets Moira's shipped subscriber
    /// hard-suppresses at verbose levels.
    ///
    /// # Why this exists, stated plainly
    ///
    /// `install_log_capture` installs a bare `TRACE` subscriber with no filter, deliberately —
    /// a leak that only shows up under a verbose operator filter is still a leak. But that
    /// subscriber is **not** the one Moira ships. `src/config/telemetry.rs` layers a
    /// `filter_fn` that drops verbose `rig::*` events below the `EnvFilter`, because
    /// `rig-core` 0.40 logs the entire completion request body — including, after plan 11, the
    /// retrieved RAG and memory text — on `rig::completions` at `TRACE`.
    ///
    /// So a raw-capture assertion would fail for an upstream reason Moira has already
    /// mitigated, and asserting on the raw capture *without* naming that would be
    /// misrepresenting what is proven. This method draws the line explicitly: everything below
    /// is Moira's own output, and it must be canary-free. What is filtered out is named, has a
    /// production suppression behind it, and is documented as a residual risk in
    /// `docs/rag-security.md`.
    pub fn contents_excluding_suppressed_targets(&self) -> String {
        // Events are separated by the fmt layer's RFC3339 timestamp at the start of a line; a
        // pretty-printed JSON body spills onto continuation lines that carry no target, so
        // filtering line-by-line would keep the payload while dropping its header.
        let raw = self.contents();
        let mut kept = String::new();
        let mut suppressed = false;
        for line in raw.lines() {
            let starts_event = line
                .split_once(' ')
                .is_some_and(|(first, _)| first.len() >= 20 && first.ends_with('Z'));
            if starts_event {
                suppressed = SUPPRESSED_LOG_TARGETS
                    .iter()
                    .any(|target| line.contains(target));
            }
            if !suppressed {
                kept.push_str(line);
                kept.push('\n');
            }
        }
        kept
    }

    /// Byte length of the capture buffer.
    ///
    /// Used by the suites to assert the capture is **non-empty** before asserting
    /// a canary is absent from it. An empty buffer makes every absence assertion
    /// pass vacuously, which is exactly the failure mode a leak suite must not
    /// have.
    pub fn len(&self) -> usize {
        self.buffer
            .lock()
            .expect("captured log buffer poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Emits a marker event so a suite can prove the capture pipeline is live
    /// even if the flows it drives happen to log nothing.
    pub fn emit_probe(&self, marker: &str) {
        tracing::info!(probe = %marker, "leak-suite log capture probe");
    }
}

struct BufferWriter {
    buffer: Arc<StdMutex<Vec<u8>>>,
}

impl std::io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut guard = self.buffer.lock().expect("captured log buffer poisoned");
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct BufferMakeWriter {
    buffer: Arc<StdMutex<Vec<u8>>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufferMakeWriter {
    type Writer = BufferWriter;

    fn make_writer(&'a self) -> Self::Writer {
        BufferWriter {
            buffer: self.buffer.clone(),
        }
    }
}

/// Installs (once per process) a `tracing` subscriber that appends every
/// formatted event to a shared buffer, and returns a handle onto that buffer.
///
/// Call this at the top of a test, before any flow is driven. `TRACE` is the
/// level on purpose — `tower_http`'s `TraceLayer` logs its request/response
/// events at `DEBUG`, and a leak that only shows up under a verbose operator
/// filter is still a leak.
pub fn install_log_capture() -> CapturedLogs {
    LOG_CAPTURE_INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(BufferMakeWriter {
                buffer: LOG_BUFFER.clone(),
            })
            .with_ansi(false)
            .with_target(true)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        // A second install in the same process is a no-op, not a failure.
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    CapturedLogs {
        buffer: LOG_BUFFER.clone(),
    }
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

// ---------------------------------------------------------------------------
// Redis harness (plan 10 wave 2).
//
// Redis is **off by default** in Moira (plan 10 §0.4b), so nothing above needs
// it and no fixture creates one. These helpers exist for the suites that
// deliberately turn it on to exercise the cluster-wide arm of rate limiting and
// concurrency — everything else must keep passing with Redis absent, which is the
// property `tests/coordination_default_path.rs` pins.
// ---------------------------------------------------------------------------

/// A Redis client on a namespace private to one test, or `None` when Redis is not
/// configured outside CI.
///
/// Fail-closed in CI, matching `database_origin` exactly: when `CI=true` and no
/// URL is set this panics rather than silently skipping, because a skipped Redis
/// test in CI is indistinguishable from a passing one in the summary. The value
/// check is `eq_ignore_ascii_case("true")`, never `var_os(..).is_some()` —
/// GitHub Actions sets `CI` on every runner, and `is_some()` would make the
/// distinction meaningless.
///
/// The namespace is per-call and UUID-suffixed, so parallel tests cannot see each
/// other's keys and **no test ever needs `FLUSHDB`** — which would break every
/// other suite running against the same instance.
pub fn test_redis() -> Option<moira::infra::redis::RedisClient> {
    let url = match env::var("MOIRA_TEST_REDIS_URL").or_else(|_| env::var("MOIRA_REDIS__URL")) {
        Ok(value) if !value.trim().is_empty() => value,
        _ if env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => panic!(
            "MOIRA_TEST_REDIS_URL (or MOIRA_REDIS__URL) is required when CI=true for \
             Redis-backed tests"
        ),
        _ => {
            // Deliberately `announce_skip`, not `eprintln!`: the latter is captured by
            // `libtest` and never reaches the log `scripts/gates.sh` greps. See
            // [`announce_skip`] — this suite's skip line is one of the three the gate's
            // pattern was written for, and it could not have been seen.
            announce_skip("skipping Redis-backed test: MOIRA_TEST_REDIS_URL is not set");
            return None;
        }
    };
    let settings = moira::config::RedisSettings {
        enabled: true,
        url: Some(url),
        namespace: format!("moira-test-{}", Uuid::now_v7().simple()),
        ..moira::config::RedisSettings::default()
    };
    Some(
        moira::infra::redis::RedisClient::from_settings(&settings)
            .expect("build the test Redis client")
            .expect("Redis is enabled in the test settings"),
    )
}
