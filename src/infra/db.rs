use std::time::Duration;

use sqlx::{
    PgPool,
    postgres::{PgListener, PgPoolOptions},
};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::{
    config::DatabaseSettings,
    error::AppError,
    orchestration::{CircuitBreakerRegistry, ProviderRuntimeCache, RuntimeConfigCache},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(settings: &DatabaseSettings) -> Result<Option<PgPool>, AppError> {
    let Some(url) = settings.url.as_deref().filter(|url| !url.is_empty()) else {
        if settings.require {
            return Err(AppError::Config("database url is required".to_string()));
        }
        return Ok(None);
    };

    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .min_connections(settings.min_connections)
        .acquire_timeout(Duration::from_secs(settings.connect_timeout_seconds))
        .connect(url)
        .await
        .map(Some)
        .map_err(AppError::from)
}

pub async fn migrate(pool: &PgPool) -> Result<(), AppError> {
    MIGRATOR
        .run(pool)
        .await
        .map_err(|err| AppError::Internal(format!("run migrations: {err}")))
}

pub fn spawn_runtime_config_listener(
    pool: PgPool,
    cache: RuntimeConfigCache,
    runtime_handles: ProviderRuntimeCache,
    circuits: CircuitBreakerRegistry,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = listen_once(&pool, &cache, &runtime_handles, &circuits).await {
                warn!(error = %err, "runtime config listener disconnected");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    })
}

async fn listen_once(
    pool: &PgPool,
    cache: &RuntimeConfigCache,
    runtime_handles: &ProviderRuntimeCache,
    circuits: &CircuitBreakerRegistry,
) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("moira_runtime_config").await?;
    info!("runtime config listener attached");

    loop {
        let notification = listener.recv().await?;
        cache.invalidate_all().await;
        runtime_handles.invalidate_all().await;
        circuits.reset_all().await;
        info!(
            channel = notification.channel(),
            payload = notification.payload(),
            "runtime config cache invalidated"
        );
    }
}
