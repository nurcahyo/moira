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
        /// Machine-readable diagnostic data for the envelope's existing `details` field.
        ///
        /// Added by plan 11 Sub-Phase D: `context_length_exceeded` has to tell the caller
        /// *which* required section overflowed and by how much, and doing that in the English
        /// `message` would put structured data in prose — exactly what `CONVENTIONS.md` §4.3
        /// forbids. `ErrorDetail.details` already existed and was only ever populated on the
        /// replay path; this is the channel that lets a live error use it too.
        ///
        /// Whatever goes in here is returned verbatim to the caller, so it must carry ids,
        /// counts and enum-like reason strings only — never content, never a provider body.
        details: Option<Box<Value>>,
    },
    #[error("{}: {}", .0.code, .0.message)]
    Replayed(Box<ReplayedError>),
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

#[derive(Debug, Clone)]
pub struct ReplayedError {
    pub status: StatusCode,
    pub code: String,
    pub message_key: String,
    pub message: String,
    pub message_args: Value,
    pub details: Option<Value>,
}

impl AppError {
    pub fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Api {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    /// As [`Self::coded`], attaching machine-readable diagnostics to the envelope's `details`.
    ///
    /// `details` is echoed to the caller verbatim — ids, counts and reason strings only.
    pub fn coded_with_details(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        details: Value,
    ) -> Self {
        Self::Api {
            status,
            code,
            message: message.into(),
            details: Some(Box::new(details)),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::coded(StatusCode::CONFLICT, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::coded(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn error_response(&self, request_id: Option<String>) -> ErrorResponse {
        if let Self::Replayed(replay) = self {
            return ErrorResponse {
                error: ErrorDetail {
                    code: replay.code.clone(),
                    message_key: replay.message_key.clone(),
                    message: replay.message.clone(),
                    message_args: replay.message_args.clone(),
                    request_id: request_id.unwrap_or_else(generate_request_id),
                    details: replay.details.clone(),
                },
            };
        }
        ErrorResponse {
            error: ErrorDetail::from((self, request_id)),
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::Config(_) => StatusCode::BAD_REQUEST,
            Self::Api { status, .. } => *status,
            Self::Replayed(replay) => replay.status,
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

    fn code(&self) -> &str {
        match self {
            Self::BadRequest(_) => "bad_request",
            Self::Api { code, .. } => code,
            Self::Replayed(replay) => &replay.code,
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
            Self::Replayed(replay) => replay.message.clone(),
            Self::DatabaseUnavailable => "database is not configured".to_string(),
            Self::Sqlx(_) => "database error".to_string(),
            Self::Reqwest(_) => "http client error".to_string(),
            Self::Redis(_) => "redis error".to_string(),
        }
    }

    fn response_text(&self) -> ResponseText {
        if let Self::Replayed(replay) = self {
            return ResponseText::with_args(
                replay.message_key.clone(),
                replay.message.clone(),
                replay.message_args.clone(),
            );
        }
        let message_key = self.message_key();
        ResponseText::new(message_key, self.english_message())
    }

    pub fn is_cacheable_admin_failure(&self) -> bool {
        matches!(
            self.status(),
            StatusCode::BAD_REQUEST
                | StatusCode::NOT_FOUND
                | StatusCode::CONFLICT
                | StatusCode::UNPROCESSABLE_ENTITY
        ) && !matches!(
            self,
            Self::Sqlx(_)
                | Self::Reqwest(_)
                | Self::Redis(_)
                | Self::Internal(_)
                | Self::Unauthorized(_)
                | Self::Forbidden(_)
        )
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
        if let AppError::Replayed(replay) = error {
            return Self {
                code: replay.code.clone(),
                message_key: replay.message_key.clone(),
                message: replay.message.clone(),
                message_args: replay.message_args.clone(),
                request_id: request_id.unwrap_or_else(generate_request_id),
                details: replay.details.clone(),
            };
        }
        let details = match error {
            AppError::Api { details, .. } => details.as_deref().cloned(),
            _ => None,
        };
        Self::new(
            error.code().to_string(),
            error.response_text(),
            request_id.unwrap_or_else(generate_request_id),
            details,
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

    /// The trap plan 07's i18n section names explicitly: `AppError::BadRequest` /
    /// `Forbidden` / `NotFound` / `Unauthorized` derive the generic codes
    /// `bad_request`/`forbidden`/`not_found`/`unauthorized` and would silently drop a
    /// plan's specific key. Every identity and auth-settings condition must be raised
    /// with `AppError::coded`/`conflict`/`unprocessable` instead, so its `message_key`
    /// is `moira.error.<the documented code>`, not one of the four generic ones — and
    /// the resulting key must actually resolve in the catalog.
    #[test]
    fn identity_error_codes_derive_their_documented_message_keys() {
        for code in [
            "unregistered_trusted_issuer",
            "admin_claim_email_required",
            "admin_claim_email_not_verified",
            "admin_claim_domain_not_allowed",
            "admin_identity_already_claimed",
            "setup_claim_credential_required",
            "setup_token_not_supported",
            "auth_provider_not_found",
            "duplicate_auth_provider",
            "auth_provider_method_config_incomplete",
            "auth_provider_url_not_allowed",
            "console_issuer_must_not_assert_scopes",
        ] {
            let error = AppError::coded(StatusCode::BAD_REQUEST, code, "test message");
            let expected_key = format!("moira.error.{code}");
            assert_eq!(
                error.message_key(),
                expected_key,
                "{code} must derive {expected_key}, never a generic bad_request/forbidden/\
                 not_found/unauthorized code"
            );
            assert!(
                crate::i18n::default_message_for_key(&expected_key).is_some(),
                "{expected_key} must actually resolve in the i18n catalog"
            );
        }
    }
}
