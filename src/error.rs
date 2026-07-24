use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;

use crate::domain::ResponseText;

tokio::task_local! {
    pub(crate) static REQUEST_ID: Option<String>;
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message_key: String,
    pub message: String,
    pub message_args: Value,
    pub request_id: String,
    pub details: Option<Value>,
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

    pub fn error_response(&self, request_id: Option<String>) -> ErrorResponse {
        ErrorResponse {
            error: ErrorDetail::from((self, request_id)),
        }
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

    fn message_key(&self) -> String {
        format!("moira.error.{}", self.code())
    }

    fn english_message(&self) -> String {
        match self {
            Self::BadRequest(message)
            | Self::Unauthorized(message)
            | Self::Forbidden(message)
            | Self::NotFound(message)
            | Self::Upstream(message)
            | Self::Config(message)
            | Self::Internal(message) => message.clone(),
            Self::Api { message, .. } => message.clone(),
            Self::DatabaseUnavailable => "database is not configured".to_string(),
            Self::Sqlx(_) => "database error".to_string(),
            Self::Reqwest(_) => "http client error".to_string(),
            Self::Redis(_) => "redis error".to_string(),
        }
    }

    fn response_text(&self) -> ResponseText {
        let message_key = self.message_key();
        ResponseText::new(message_key, self.english_message())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = self.error_response(current_request_id());
        (status, Json(body)).into_response()
    }
}

impl ErrorDetail {
    pub fn new(
        code: impl Into<String>,
        message: ResponseText,
        request_id: String,
        details: Option<Value>,
    ) -> Self {
        let ResponseText {
            message_key,
            message,
            message_args,
        } = message;
        Self {
            code: code.into(),
            message_key,
            message,
            message_args,
            request_id,
            details,
        }
    }
}

impl From<&AppError> for ErrorDetail {
    fn from(value: &AppError) -> Self {
        Self::from((value, current_request_id()))
    }
}

impl From<(&AppError, Option<String>)> for ErrorDetail {
    fn from(value: (&AppError, Option<String>)) -> Self {
        let (error, request_id) = value;
        Self::new(
            error.code().to_string(),
            error.response_text(),
            request_id.unwrap_or_else(generate_request_id),
            None,
        )
    }
}

impl From<&AppError> for ErrorResponse {
    fn from(value: &AppError) -> Self {
        Self {
            error: ErrorDetail::from(value),
        }
    }
}

impl From<(&AppError, Option<String>)> for ErrorResponse {
    fn from(value: (&AppError, Option<String>)) -> Self {
        Self {
            error: ErrorDetail::from(value),
        }
    }
}

pub(crate) fn current_request_id() -> Option<String> {
    REQUEST_ID
        .try_with(|request_id| request_id.clone())
        .ok()
        .flatten()
}

fn generate_request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_error_preserves_status_and_codes() {
        let error = AppError::conflict("duplicate_application", "application already exists");

        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "duplicate_application");
    }

    #[test]
    fn error_response_includes_i18n_fields_and_request_id() {
        let error = AppError::BadRequest("application_slug must not be empty".to_string());
        let response = error.error_response(Some("req_123".to_string()));

        assert_eq!(response.error.code, "bad_request");
        assert_eq!(response.error.message_key, "moira.error.bad_request");
        assert_eq!(response.error.message, "application_slug must not be empty");
        assert_eq!(response.error.message_args.as_object().unwrap().len(), 0);
        assert_eq!(response.error.request_id, "req_123");
        assert_eq!(response.error.details, None);
    }

    #[test]
    fn api_error_uses_english_fallback_message_without_code_prefix() {
        let error = AppError::coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "duplicate_resource",
            "resource already exists",
        );
        let response = error.error_response(None);

        assert_eq!(response.error.code, "duplicate_resource");
        assert_eq!(response.error.message_key, "moira.error.duplicate_resource");
        assert_eq!(response.error.message, "resource already exists");
    }
}
