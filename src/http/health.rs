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
    // First, and before any I/O. A replica that has lost its cluster admission
    // lease is outside the configured replica ceiling: its dependencies may all
    // be perfectly healthy and it still must not receive traffic, or P3-2 is
    // fixed only at startup. Failing readiness takes it out of the Service's
    // endpoints without killing in-flight requests.
    //
    // `is_denied()` is false unless admission is both enabled and lost, so this
    // costs one relaxed atomic load on every probe of every default deployment.
    if state.cluster_lease.is_denied() {
        // The code is written as a **literal**, not as
        // `crate::app::CLUSTER_LEASE_DENIED_CODE`, and that is not an oversight:
        // `every_coded_error_literal_in_src_has_a_catalog_entry` walks the source
        // for `AppError::coded` arguments and can only prove a catalog entry
        // exists for a literal. A constant here defeats the gate that guarantees
        // this response carries an English message. The constant still exists for
        // the structured `reason` fields in `src/app/cluster_lease.rs`, and
        // `readyz_returns_503_and_cluster_lease_denied...` in
        // `tests/cluster_admission.rs` asserts the two agree.
        return Err(AppError::coded(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "cluster_lease_denied",
            "this replica does not hold a valid cluster admission lease",
        ));
    }

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
