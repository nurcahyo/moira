use axum::{Json, extract::State};

use crate::{
    app::AppState,
    domain::HealthResponse,
    error::{AppError, ErrorResponse},
};

#[utoipa::path(
    get,
    path = "/health/live",
    tag = "health",
    responses(
        (status = 200, description = "Service is live", body = HealthResponse)
    )
)]
pub async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        database: if state.pool.is_some() {
            "configured"
        } else {
            "not_configured"
        },
        redis: if state.redis.is_some() {
            "configured"
        } else {
            "not_configured"
        },
        workers: if state.workers.enabled() {
            "enabled"
        } else {
            "disabled"
        },
        metrics: if state.settings.telemetry.prometheus_enabled {
            "enabled"
        } else {
            "disabled"
        },
    })
}

#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "health",
    responses(
        (status = 200, description = "Required dependencies are ready", body = HealthResponse),
        (status = 503, description = "A required dependency is unavailable", body = ErrorResponse),
        (status = 500, description = "A dependency readiness check failed", body = ErrorResponse)
    )
)]
pub async fn readyz(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    if let Some(pool) = &state.pool {
        sqlx::query("select 1").execute(pool).await?;
    } else {
        return Err(AppError::DatabaseUnavailable);
    }

    if let Some(redis) = &state.redis {
        redis.ping().await?;
    }

    Ok(Json(HealthResponse {
        status: "ready",
        database: "ready",
        redis: if state.redis.is_some() {
            "ready"
        } else {
            "not_configured"
        },
        workers: if state.workers.enabled() {
            "enabled"
        } else {
            "disabled"
        },
        metrics: if state.settings.telemetry.prometheus_enabled {
            "enabled"
        } else {
            "disabled"
        },
    }))
}
