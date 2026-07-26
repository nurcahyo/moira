use std::net::SocketAddr;

use anyhow::Context;
use moira::{
    app::AppState,
    application::{AdminService, MoiraExecutionService, RequestContext},
    build_router,
    config::{
        ProcessMode, Settings,
        telemetry::{self, TelemetryShutdown},
    },
    domain::{DiagnosticExecutionRequest, ExecutionOptions, SystemKeyCreateRequest},
    infra::db,
    security::{Actor, ActorType},
};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mode =
        ProcessMode::parse(std::env::args().nth(1).as_deref()).context("parse process mode")?;
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
        ProcessMode::Serve => {}
    }

    let pool = db::connect(&settings.database).await?;
    if settings.database.migrate_on_startup
        && let Some(pool) = &pool
    {
        db::migrate(pool).await?;
    }

    let addr: SocketAddr = settings.server.bind_addr()?;
    let state = AppState::new(settings, pool)?;
    let worker_supervisor = state.workers.spawn_supervisor(state.clone());
    let _runtime_config_listener = state.pool.as_ref().map(|pool| {
        db::spawn_runtime_config_listener(
            pool.clone(),
            state.runtime_cache.clone(),
            state.runtime_handles.clone(),
            state.auth_settings_cache.clone(),
            state.circuits.clone(),
        )
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

    if let Some(supervisor) = worker_supervisor {
        supervisor.shutdown().await;
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

    let state = AppState::new(settings, Some(pool))?;
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

async fn execute_test(settings: Settings) -> anyhow::Result<()> {
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let request = diagnostic_request_from_args(&args)?;
    let pool = db::connect(&settings.database)
        .await?
        .context("database url is required for execute-test")?;
    db::migrate(&pool).await?;

    let state = AppState::new(settings, Some(pool))?;
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
