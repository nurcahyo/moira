//! `moira-runner` — the containerised Claude runner control service.
//!
//! The second binary of this crate, and the only component permitted to talk to the Docker
//! Engine API. See the [`moira::runner`] module docs for the design, the measured evidence it
//! rests on, and the parts that are unproven.
//!
//! ```text
//! MOIRA_RUNNER__CONTROL_TOKEN=$(openssl rand -hex 32) \
//! MOIRA_RUNNER__IMAGE=sha256:<64 hex> \
//!   moira-runner
//! ```
//!
//! # Why this does not call `moira::config::telemetry::init`
//!
//! That function takes a `TelemetrySettings` out of the API process's `Settings`, which
//! carries a database URL, a master key and a provider configuration this process has no use
//! for and should not be handed. It also builds an OTLP exporter whose span filter is an
//! allow-list of Moira's own targets. So the subscriber here is assembled from the same
//! crates and the same idiom — `tracing_subscriber::registry()`, an `EnvFilter`, and the same
//! JSON/text choice — with the OTLP layer left off. That is reuse of the idiom rather than a
//! parallel logging stack, and it is a deliberately smaller thing than the API's pipeline.

use std::sync::Arc;

use anyhow::Context;
use moira::runner::{
    config::RunnerConfig, docker_engine::DockerEngine, engine::ContainerEngine, http,
    service::RunnerService,
};
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Arc::new(RunnerConfig::from_env().context("load moira-runner configuration")?);
    info!(config = ?config, "moira-runner configuration accepted");

    let engine = Arc::new(
        DockerEngine::connect(config.docker_host.as_deref())
            .context("connect to the Docker Engine API")?,
    );
    // A failed ping is a warning, not a refusal: Docker Desktop and `dockerd` are routinely
    // slower to come up than their dependants, and a control plane that exits because the
    // daemon was not ready yet turns a transient condition into a crash loop. `GET /healthz`
    // reports the truth in the meantime.
    if let Err(error) = engine.ping().await {
        warn!(%error, "the Docker daemon did not answer at startup; /healthz will report it");
    }

    let service = Arc::new(RunnerService::new(Arc::clone(&engine), Arc::clone(&config)));
    let reaper = tokio::spawn(reap_forever(Arc::clone(&service)));

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    let bound = listener.local_addr().context("read the bound address")?;
    info!(%bound, "moira-runner is listening");

    let result = axum::serve(listener, http::router(Arc::clone(&service)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve the moira-runner control surface");

    reaper.abort();
    result
}

/// Sweeps expired runners for the life of the process.
///
/// Driven off the Docker labels, so it also collects containers orphaned by a restart or a
/// crash — see [`moira::runner::service::RunnerService::reap`]. A failed sweep is logged and
/// the loop continues: a daemon that is temporarily away must not permanently stop reaping,
/// because the thing being reaped is a container holding a live login session.
async fn reap_forever<E: ContainerEngine>(service: Arc<RunnerService<E>>) {
    let mut ticker = tokio::time::interval(service.config().reap_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match service.reap(chrono::Utc::now()).await {
            Ok(0) => {}
            Ok(reaped) => info!(reaped, "reaped expired runners"),
            Err(error) => error!(%error, "a reaper sweep failed; retrying on the next tick"),
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => warn!(%error, "could not install a SIGTERM handler"),
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    info!("shutdown signal received");
}

/// `MOIRA_RUNNER__LOG` selects the filter, `MOIRA_RUNNER__LOG_JSON` the format.
///
/// Deliberately not `RUST_LOG`: this process runs alongside the API process and often in the
/// same compose file, and a shared variable would reconfigure both at once.
fn init_tracing() {
    let filter = std::env::var("MOIRA_RUNNER__LOG")
        .ok()
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new("info,moira=info"));

    let json = std::env::var("MOIRA_RUNNER__LOG_JSON")
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"));

    let (json_layer, text_layer) = if json {
        (Some(tracing_subscriber::fmt::layer().json()), None)
    } else {
        (None, Some(tracing_subscriber::fmt::layer()))
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .with(text_layer)
        .init();
}
