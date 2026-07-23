use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("{code}: {message}")]
    Api {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("database is not configured")]
    DatabaseUnavailable,
    #[error("upstream provider failed: {0}")]
    Upstream(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error")]
    Sqlx(#[from] sqlx::Error),
    #[error("http client error")]
    Reqwest(#[from] reqwest::Error),
    #[error("redis error")]
    Redis(#[from] redis::RedisError),
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
}

impl AppError {
    pub fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Api {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::coded(StatusCode::CONFLICT, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::coded(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::Config(_) => StatusCode::BAD_REQUEST,
            Self::Api { status, .. } => *status,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::DatabaseUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Sqlx(_) | Self::Reqwest(_) | Self::Redis(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Api { code, .. } => code,
            Self::Unauthorized(_) => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::DatabaseUnavailable => "database_unavailable",
            Self::Upstream(_) => "upstream_error",
            Self::Config(_) => "configuration_error",
            Self::Sqlx(_) => "database_error",
            Self::Reqwest(_) => "http_client_error",
            Self::Redis(_) => "redis_error",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ErrorResponse {
            error: ErrorDetail {
                code: self.code().to_string(),
                message: self.to_string(),
                request_id: None,
            },
        };
        (status, Json(body)).into_response()
    }
}
