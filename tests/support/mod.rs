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
        CredentialType, DiagnosticExecutionRequest, ExecutionOptions, ProviderCreateRequest,
        ProviderModelCreateRequest, ProviderRuntimePolicyPutRequest, ProviderType,
        ResponsePersistenceMode, RouteDefinitionCreateRequest, RouteSelectionStrategy,
        RoutingPolicyCreateRequest, RuntimePolicyStatus,
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
// What this does **not** cover: the four suites that build their own pool straight
// against `MOIRA_TEST_DATABASE_URL` rather than through a fixture —
// `tests/http_middleware_contract.rs`, `tests/jwks_hardening.rs`,
// `tests/retention_worker.rs` and `tests/security_foundation.rs`. The last already
// creates and force-drops its own database; the other three assert only on rows they
// created, keyed by a per-run `Uuid`. They are left on the shared database
// deliberately, and none of them mixes its own pool with a `LifecycleFixture` in the
// same test.

const MAINTENANCE_DATABASE: &str = "postgres";
const TEMPLATE_PREFIX: &str = "moira_test_template_";

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Session advisory lock guarding the template database.
///
/// Held **exclusively** while the template is created or migrated, because an open
/// connection to it makes another process's `CREATE DATABASE … TEMPLATE` fail with
/// *"source database is being accessed by other users"*. Held **shared** while a
/// fixture clones it, so clones run in parallel with each other but never during
/// maintenance. Advisory locks are database-scoped, and this one is always taken on
/// the maintenance database, so every test process agrees on it.
const TEMPLATE_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiratdb");

/// A fixture database this old with no live connection is a leak from a killed test
/// process. Sweeping is bounded by age *and* by connection count because a database
/// is briefly connectionless between `CREATE DATABASE` and the fixture pool's first
/// connection; an hour is several orders of magnitude beyond that window.
const LEAKED_DATABASE_GRACE_SECONDS: u64 = 3_600;

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
        sqlx::raw_sql(&format!(
            "create database \"{name}\" template \"{}\"",
            origin.template
        ))
        .execute(&mut maintenance)
        .await
        .expect("clone the migrated template database");
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

/// Resolves `MOIRA_TEST_DATABASE_URL` and readies the template, once per process.
///
/// The skip/fail-closed behaviour is carried over verbatim, including the `CI`
/// **value** check required by `plans/CONVENTIONS.md` §3: absent locally it skips,
/// absent under `CI=true` it panics.
async fn database_origin() -> Option<DatabaseOrigin> {
    DATABASE_ORIGIN
        .get_or_init(|| async {
            let database_url = match env::var("MOIRA_TEST_DATABASE_URL") {
                Ok(value) if !value.trim().is_empty() => value,
                _ if env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
                    panic!(
                        "MOIRA_TEST_DATABASE_URL is required when CI=true for database-backed tests"
                    )
                }
                _ => {
                    eprintln!("skipping database-backed tests: MOIRA_TEST_DATABASE_URL is not set");
                    return None;
                }
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
/// * `moira_test_template_<digest>` — a template for a migration set that is no longer
///   current, i.e. left over from before a migration was added.
async fn sweep_leaked_databases(maintenance: &mut PgConnection, current_template: &str) {
    let idle: Vec<String> = sqlx::query_scalar(
        "select d.datname from pg_database d \
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

    for name in idle {
        let sweepable =
            if name.starts_with(TEMPLATE_PREFIX) || name.starts_with("moira_test_building_") {
                true
            } else {
                fixture_database_age_seconds(&name)
                    .is_some_and(|age| age >= LEAKED_DATABASE_GRACE_SECONDS)
            };
        if !sweepable {
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
