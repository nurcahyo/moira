use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
};

use crate::{
    app::AppState,
    error::{AppError, ErrorResponse},
};

#[utoipa::path(
    get,
    path = "/metrics",
    tag = "observability",
    responses(
        (
            status = 200,
            description = "Prometheus metrics",
            body = String,
            content_type = "text/plain"
        ),
        (status = 404, description = "Prometheus metrics are disabled", body = ErrorResponse),
        (status = 500, description = "Metrics rendering failed", body = ErrorResponse)
    )
)]
pub async fn metrics(State(state): State<AppState>) -> Result<(HeaderMap, String), AppError> {
    if !state.settings.telemetry.prometheus_enabled {
        return Err(AppError::coded(
            StatusCode::NOT_FOUND,
            "metrics_disabled",
            "prometheus metrics are disabled",
        ));
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    Ok((
        headers,
        state.metrics.render_prometheus(
            &state.settings.telemetry.service_name,
            state.redis.is_some(),
            state.workers.enabled(),
        ),
    ))
}
