use std::net::SocketAddr;

use anyhow::Context;
use moira::{
    app::{AppState, KeyringCommand, build_content_custody, cluster_lease, keyring_cli},
    application::{AdminService, MoiraExecutionService, RequestContext},
    build_router,
    config::{
        ProcessMode, Settings,
        telemetry::{self, TelemetryShutdown},
    },
    domain::{DiagnosticExecutionRequest, ExecutionOptions, SystemKeyCreateRequest},
    infra::db,
    security::{Actor, ActorType, ContentKeyring, KeyringAdmin},
};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Triaged against `rust.lang.security.args.args`, which is blocking in CI (see the
    // `sast` job in .github/workflows/ci.yml). The rule's concern is argv being trusted
    // for a security decision. It is not, here: argv selects which process mode this
    // binary runs in, and every privileged path derives its `Actor` and its scopes from
    // configuration and from the request identity, never from the command line. An
    // attacker who can set this process's argv already owns the process.
    //
    // The argv read gets its own `let` so the suppression lands on the line semgrep
    // reports: rustfmt splits the combined expression after the `=`, which would push
    // `std::env::args()` one line down and out of the comment's reach.
    // nosemgrep: rust.lang.security.args.args
    let mode_arg = std::env::args().nth(1);
    let mode = ProcessMode::parse(mode_arg.as_deref()).context("parse process mode")?;
    let settings = Settings::load().context("load settings")?;
    settings.validate(mode).context("validate settings")?;
    // Held for the whole process: the OTLP batch processor buffers spans and
    // nothing flushes them unless this guard is shut down before exit.
    let telemetry = telemetry::init(&settings.telemetry)?;

    let result = run(mode, settings).await;

    // Runs on every exit path — including the early-returning CLI modes and the
    // error path — so a clean shutdown never discards the final span batch.
    flush_telemetry(telemetry).await;

    result
}

/// Flushes the OpenTelemetry pipeline.
///
/// `SdkTracerProvider::shutdown` drains the batch processor's dedicated worker
/// thread with a blocking wait, so it is moved off the async runtime. A failed
/// flush is logged, never propagated: losing trailing spans must not turn a
/// successful run into a failed process exit.
async fn flush_telemetry(telemetry: moira::config::telemetry::TelemetryGuard) {
    match tokio::task::spawn_blocking(move || telemetry.shutdown()).await {
        Ok(TelemetryShutdown::NotEnabled | TelemetryShutdown::Flushed) => {}
        Ok(TelemetryShutdown::Failed(reason)) => {
            warn!(%reason, "telemetry pipeline did not flush cleanly on shutdown");
        }
        Err(err) => {
            warn!(%err, "telemetry shutdown task failed to join");
        }
    }
}

async fn run(mode: ProcessMode, settings: Settings) -> anyhow::Result<()> {
    let unsafe_features = settings.unsafe_development_features(mode);
    if !unsafe_features.is_empty() {
        warn!(
            features = ?unsafe_features,
            "unsafe development configuration is active"
        );
    }

    match mode {
        ProcessMode::Migrate => {
            migrate(settings).await?;
            return Ok(());
        }
        ProcessMode::BootstrapSystemKey => {
            bootstrap_system_key(settings).await?;
            return Ok(());
        }
        ProcessMode::ExecuteTest => {
            execute_test(settings).await?;
            return Ok(());
        }
        ProcessMode::Keyring => {
            keyring(settings).await?;
            return Ok(());
        }
        ProcessMode::Serve => {}
    }

    let pool = db::connect(&settings.database).await?;
    if settings.database.migrate_on_startup
        && let Some(pool) = &pool
    {
        db::migrate(pool).await?;
    }

    let addr: SocketAddr = settings.server.bind_addr()?;
    let state = AppState::new(settings, pool).await?;

    // Before the listener binds and before any worker starts: a replica that the
    // cluster will not admit must not serve a single request, and must not run a
    // sweep either. The error propagates to a non-zero exit, which is what makes
    // the pod fail to become Ready — the only ceiling `kubectl scale` cannot walk
    // past.
    let cluster_lease = cluster_lease::acquire(
        state.pool.as_ref(),
        &state.settings.cluster,
        &state.cluster_lease,
    )
    .await
    .context("acquire the cluster admission lease")?;

    // Detached, and deliberately so: the keyring is already fully loaded by the time
    // `AppState::new` returned, and a tick that never runs again leaves the process serving
    // correctly from a snapshot that is merely stale. That is the same posture a failed
    // refresh has, which is why there is nothing here to join on or to fail over.
    let _content_keyring_refresh = state
        .content_keyring
        .as_ref()
        .map(ContentKeyring::spawn_refresh);

    let worker_supervisor = state.workers.spawn_supervisor(state.clone());
    let invalidation_targets = db::RuntimeInvalidationTargets::from_state(&state);
    let _runtime_config_listener = state
        .pool
        .as_ref()
        .map(|pool| db::spawn_runtime_config_listener(pool.clone(), invalidation_targets.clone()));
    // The second channel, and only ever a second one. It exists when Redis is
    // enabled — which is not the default — and the Postgres listener above is
    // spawned regardless, so turning Redis off removes a signal path rather than
    // the signal.
    let _redis_invalidation_listener = state.redis.as_ref().map(|redis| {
        db::spawn_redis_invalidation_listener(redis.clone(), invalidation_targets.clone())
    });
    let app = build_router(state)?;

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    info!(%addr, "moira listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve moira")?;

    // Workers first: the supervisor resigns its leader lock as it stops, and
    // handing leadership over before giving up the admission lease keeps the two
    // handovers in a defensible order.
    if let Some(supervisor) = worker_supervisor {
        supervisor.shutdown().await;
    }

    // Released on the graceful-shutdown path so the replacement pod is admitted
    // immediately instead of waiting out `cluster.lease_expiry_seconds`. A
    // rolling update where this does not run — SIGKILL, a panic — still recovers
    // via heartbeat expiry; it just takes longer.
    if let Some(lease) = cluster_lease {
        lease.release().await;
    }

    Ok(())
}

async fn migrate(settings: Settings) -> anyhow::Result<()> {
    let pool = db::connect(&settings.database)
        .await?
        .context("database url is required for migrate")?;
    db::migrate(&pool).await?;
    info!("database migrations completed");
    Ok(())
}

async fn bootstrap_system_key(settings: Settings) -> anyhow::Result<()> {
    let pool = db::connect(&settings.database)
        .await?
        .context("database url is required for bootstrap-system-key")?;
    db::migrate(&pool).await?;

    let state = AppState::new(settings, Some(pool)).await?;
    let actor = Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some("bootstrap-cli".to_string()),
        scopes: vec!["moira:admin".to_string()],
        ..Actor::default()
    };
    let ctx = RequestContext {
        request_id: format!("req_{}", uuid::Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("moira-bootstrap-cli".to_string()),
        idempotency_key: None,
    };
    let response = AdminService::new(&state)?
        .create_system_key(
            &actor,
            &ctx,
            SystemKeyCreateRequest {
                display_name: "Bootstrap Root System Key".to_string(),
                scopes: vec!["moira:admin".to_string()],
                expires_at: None,
            },
        )
        .await?;

    println!(
        "{}",
        serde_json::to_string_pretty(&response).context("serialize bootstrap key response")?
    );
    Ok(())
}

/// `moira keyring <verb> …` — the content data key rotation verbs.
///
/// **Deliberately does not build an `AppState` and does not load the keyring.** `abandon` is
/// reachable only after `ContentKeyring::load` has already refused to start this process, so a
/// mode that loaded the keyring first would be unable to run the one command that exists to
/// repair that condition. It takes a pool and a preflighted custody backend, and nothing else.
///
/// It also does **not** migrate on entry, unlike `bootstrap-system-key` and `execute-test`. A
/// rotation verb run against a database whose schema is behind should say so, not quietly
/// change it: `content_data_keys` arrived in `0027`, and applying migrations as a side effect
/// of `keyring status` is exactly the kind of surprise an operator mid-incident does not need.
async fn keyring(settings: Settings) -> anyhow::Result<()> {
    // Triaged against `rust.lang.security.args.args`, as in `main` above. These are the
    // arguments of the `keyring` operator subcommand; they choose a verb and a key id. No
    // security decision is derived from them — the database and the master keys both come
    // from configuration, and `abandon`'s guards are enforced in `KeyringAdmin`, not here.
    // nosemgrep: rust.lang.security.args.args
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let command = KeyringCommand::parse(&args)?;

    let pool = db::connect(&settings.database)
        .await?
        .context("database url is required for keyring")?;
    let custody = build_content_custody(&settings).await?;

    let output = keyring_cli::run(command, &KeyringAdmin::new(pool, custody.custody())).await?;
    print!("{output}");
    Ok(())
}

async fn execute_test(settings: Settings) -> anyhow::Result<()> {
    // Triaged against `rust.lang.security.args.args`, as in `main` above. These are the
    // arguments of the `execute-test` operator subcommand; they choose a route and a
    // prompt for a diagnostic execution. The `Actor` used for that execution is built
    // below from fixed values, not from argv.
    // nosemgrep: rust.lang.security.args.args
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let request = diagnostic_request_from_args(&args)?;
    let pool = db::connect(&settings.database)
        .await?
        .context("database url is required for execute-test")?;
    db::migrate(&pool).await?;

    let state = AppState::new(settings, Some(pool)).await?;
    let actor = Actor {
        actor_type: ActorType::DevAdmin,
        subject: Some("execute-test-cli".to_string()),
        scopes: vec![
            "moira:admin".to_string(),
            "moira:runtime:diagnose".to_string(),
            "moira:execution:override-route".to_string(),
            "moira:execution:override-provider".to_string(),
            "moira:execution:override-model".to_string(),
            "moira:execution:override-credential".to_string(),
        ],
        ..Actor::default()
    };
    let ctx = RequestContext {
        request_id: format!("req_{}", uuid::Uuid::now_v7()),
        source_ip: None,
        user_agent: Some("moira-execute-test-cli".to_string()),
        idempotency_key: None,
    };
    let command = MoiraExecutionService::command_from_diagnostic(&actor, &ctx, request);
    let (outcome, events) = MoiraExecutionService::new(state)?
        .execute_with_events(command)
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "outcome": outcome,
            "events": events
        }))
        .context("serialize execute-test response")?
    );
    Ok(())
}

fn diagnostic_request_from_args(args: &[String]) -> anyhow::Result<DiagnosticExecutionRequest> {
    let mut prompt = None;
    let mut route = None;
    let mut application_id = None;
    let mut external_tenant_id = None;
    let mut external_user_id = None;
    let mut provider_id = None;
    let mut provider_model_id = None;
    let mut credential_id = None;
    let mut stream = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {}
            "--prompt" => {
                i += 1;
                prompt = args.get(i).cloned();
            }
            "--route" => {
                i += 1;
                route = args.get(i).cloned();
            }
            "--application-id" => {
                i += 1;
                application_id = args.get(i).map(|value| value.parse()).transpose()?;
            }
            "--external-tenant-id" => {
                i += 1;
                external_tenant_id = args.get(i).cloned();
            }
            "--external-user-id" => {
                i += 1;
                external_user_id = args.get(i).cloned();
            }
            "--provider-id" => {
                i += 1;
                provider_id = args.get(i).map(|value| value.parse()).transpose()?;
            }
            "--provider-model-id" => {
                i += 1;
                provider_model_id = args.get(i).map(|value| value.parse()).transpose()?;
            }
            "--credential-id" => {
                i += 1;
                credential_id = args.get(i).map(|value| value.parse()).transpose()?;
            }
            "--stream" => stream = true,
            other => anyhow::bail!("unknown execute-test argument {other}"),
        }
        i += 1;
    }
    Ok(DiagnosticExecutionRequest {
        application_id,
        external_tenant_id,
        external_user_id,
        route,
        provider_id,
        provider_model_id,
        credential_id,
        prompt: prompt.context("--prompt is required")?,
        stream,
        options: ExecutionOptions::default(),
        metadata: serde_json::json!({ "source": "execute-test-cli" }),
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
