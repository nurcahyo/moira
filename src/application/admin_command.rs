use chrono::{Duration, Utc};
use futures_util::future::BoxFuture;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
    error::{AppError, ReplayedError},
    infra::repositories::{
        AdminIdempotencyClaim, AdminIdempotencyClaimOutcome, PgAdminCommandTransaction,
        PgAdminRepository,
    },
    security::{IdempotencyHasher, request_hash},
};

const ADMIN_COMMAND_VERSION: u16 = 1;
const IDEMPOTENCY_RETENTION_HOURS: i64 = 24;

#[derive(Debug, Clone)]
pub struct AdminCommandSpec {
    operation: String,
    path: Value,
    request: Value,
    expected_version: Option<i64>,
    idempotency: Option<AdminCommandIdempotency>,
}

#[derive(Debug, Clone)]
pub struct AdminCommandIdempotency {
    pub key: String,
    pub actor_fingerprint: String,
}

#[derive(Debug)]
pub struct AdminCommandMutation<T> {
    pub client_response: T,
    pub replay_response: Value,
    pub status: u16,
    pub resource_id: Option<String>,
}

#[derive(Debug)]
pub struct AdminCommandOutcome<T> {
    pub response: T,
    pub status: u16,
    pub resource_id: Option<String>,
    pub replayed: bool,
}

#[derive(Debug, Clone)]
pub struct AdminCommandRunner {
    repository: PgAdminRepository,
    /// Keyed hasher for every ledger value this runner writes (plan 03, P1-1). Admin
    /// command bodies carry provider API keys and credential material, so their digests
    /// must not be offline-attackable from the database alone.
    hasher: IdempotencyHasher,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAdminReplay {
    Success { body: Value },
    Failure { error: StoredAdminError },
}

#[derive(Debug, Serialize, serde::Deserialize)]
struct StoredAdminError {
    code: String,
    message_key: String,
    message: String,
    message_args: Value,
    details: Option<Value>,
}

impl AdminCommandSpec {
    pub fn new(
        operation: impl Into<String>,
        path: impl Serialize,
        request: impl Serialize,
    ) -> Result<Self, AppError> {
        Ok(Self {
            operation: operation.into(),
            path: serde_json::to_value(path).map_err(invalid_command)?,
            request: serde_json::to_value(request).map_err(invalid_command)?,
            expected_version: None,
            idempotency: None,
        })
    }

    pub fn with_expected_version(mut self, expected_version: Option<i64>) -> Self {
        self.expected_version = expected_version;
        self
    }

    pub fn with_idempotency(mut self, idempotency: Option<AdminCommandIdempotency>) -> Self {
        self.idempotency = idempotency;
        self
    }

    /// The keyed, versioned hash of the canonical command envelope — the value written to
    /// `idempotency_records.request_hash` for every new claim.
    pub fn request_hash(&self, hasher: &IdempotencyHasher) -> Result<String, AppError> {
        self.envelope_bytes().map(|bytes| hasher.hash(&bytes))
    }

    /// The legacy unkeyed hash of the same envelope.
    ///
    /// Only used to compare against rows written before the HMAC switch (plan 03, P1-1);
    /// never written. Legacy rows expire within `IDEMPOTENCY_RETENTION_HOURS`.
    pub fn legacy_request_hash(&self) -> Result<String, AppError> {
        self.envelope_bytes().map(|bytes| request_hash(&bytes))
    }

    fn envelope_bytes(&self) -> Result<Vec<u8>, AppError> {
        let envelope = json!({
            "version": ADMIN_COMMAND_VERSION,
            "operation": self.operation,
            "path": self.path,
            "request": self.request,
            "expected_version": self.expected_version,
        });
        serde_json::to_vec(&canonicalize(envelope)).map_err(invalid_command)
    }
}

impl<T> AdminCommandMutation<T>
where
    T: Serialize,
{
    pub fn new(response: T, status: u16, resource_id: Option<String>) -> Result<Self, AppError> {
        let replay_response = serde_json::to_value(&response).map_err(|error| {
            AppError::Internal(format!("encode admin command response: {error}"))
        })?;
        Ok(Self {
            client_response: response,
            replay_response,
            status,
            resource_id,
        })
    }

    pub fn with_replay_response(
        client_response: T,
        replay_response: impl Serialize,
        status: u16,
        resource_id: Option<String>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            client_response,
            replay_response: serde_json::to_value(replay_response).map_err(|error| {
                AppError::Internal(format!("encode sanitized admin command response: {error}"))
            })?,
            status,
            resource_id,
        })
    }
}

impl AdminCommandRunner {
    pub fn new(repository: PgAdminRepository, hasher: IdempotencyHasher) -> Self {
        Self { repository, hasher }
    }

    pub async fn execute<T, F>(
        &self,
        spec: AdminCommandSpec,
        mutation: F,
    ) -> Result<AdminCommandOutcome<T>, AppError>
    where
        T: Serialize + DeserializeOwned,
        F: for<'a> FnOnce(
            &'a mut PgAdminCommandTransaction,
        ) -> BoxFuture<'a, Result<AdminCommandMutation<T>, AppError>>,
    {
        let mut transaction = self.repository.begin_admin_command().await?;
        let claim = spec.idempotency.as_ref().map(|idempotency| {
            Ok::<_, AppError>(AdminIdempotencyClaim {
                record_id: Uuid::now_v7(),
                key_hash: self.hasher.hash(idempotency.key.as_bytes()),
                // The key hash is an index key under the unique index on
                // (idempotency_key_hash, actor_fingerprint, operation), so a legacy row
                // is unreachable by the versioned hash alone. Carrying the legacy value
                // keeps pre-deploy claims replayable until they expire.
                legacy_key_hash: self.hasher.legacy_hash(idempotency.key.as_bytes()),
                actor_fingerprint: idempotency.actor_fingerprint.clone(),
                operation: spec.operation.clone(),
                request_hash: spec.request_hash(&self.hasher)?,
                legacy_request_hash: spec.legacy_request_hash()?,
                expires_at: Utc::now() + Duration::hours(IDEMPOTENCY_RETENTION_HOURS),
            })
        });
        let claim = claim.transpose()?;

        if let Some(claim) = &claim {
            match transaction.claim_idempotency(claim).await? {
                AdminIdempotencyClaimOutcome::Acquired => {}
                AdminIdempotencyClaimOutcome::Replay(record) => {
                    transaction.commit().await?;
                    return replay_record(record);
                }
            }
        }

        transaction.begin_command_savepoint().await?;
        match mutation(&mut transaction).await {
            Ok(result) => {
                transaction.release_command_savepoint().await?;
                if let Some(claim) = &claim {
                    let replay = serde_json::to_value(StoredAdminReplay::Success {
                        body: result.replay_response,
                    })
                    .map_err(|error| {
                        AppError::Internal(format!("encode idempotent success: {error}"))
                    })?;
                    transaction
                        .finalize_idempotency(
                            claim.record_id,
                            i32::from(result.status),
                            &replay,
                            result.resource_id.as_deref(),
                        )
                        .await?;
                }
                transaction.commit().await?;
                Ok(AdminCommandOutcome {
                    response: result.client_response,
                    status: result.status,
                    resource_id: result.resource_id,
                    replayed: false,
                })
            }
            Err(error) if claim.is_some() && error.is_cacheable_admin_failure() => {
                transaction.rollback_command_savepoint().await?;
                let claim = claim.expect("checked above");
                let response = error.error_response(Some(String::new()));
                let replay = StoredAdminReplay::Failure {
                    error: StoredAdminError {
                        code: response.error.code,
                        message_key: response.error.message_key,
                        message: response.error.message,
                        message_args: response.error.message_args,
                        details: response.error.details,
                    },
                };
                let replay = serde_json::to_value(replay).map_err(|encode_error| {
                    AppError::Internal(format!("encode idempotent failure: {encode_error}"))
                })?;
                transaction
                    .finalize_idempotency(
                        claim.record_id,
                        i32::from(error.status().as_u16()),
                        &replay,
                        None,
                    )
                    .await?;
                transaction.commit().await?;
                Err(error)
            }
            Err(error) => {
                transaction.rollback().await?;
                Err(error)
            }
        }
    }
}

fn replay_record<T>(
    record: crate::domain::IdempotencyRecord,
) -> Result<AdminCommandOutcome<T>, AppError>
where
    T: DeserializeOwned,
{
    let status = record
        .response_status
        .and_then(|status| u16::try_from(status).ok())
        .ok_or_else(|| {
            AppError::Internal("idempotent response is missing its status".to_string())
        })?;
    let response_body = record
        .response_body
        .ok_or_else(|| AppError::Internal("idempotent response is missing its body".to_string()))?;
    match serde_json::from_value::<StoredAdminReplay>(response_body.clone()) {
        Ok(StoredAdminReplay::Success { body }) => {
            let response = serde_json::from_value(body).map_err(|error| {
                AppError::Internal(format!("decode idempotent success response: {error}"))
            })?;
            Ok(AdminCommandOutcome {
                response,
                status,
                resource_id: record.resource_id,
                replayed: true,
            })
        }
        Ok(StoredAdminReplay::Failure { error }) => {
            Err(AppError::Replayed(Box::new(ReplayedError {
                status: http_status(status)?,
                code: error.code,
                message_key: error.message_key,
                message: error.message,
                message_args: error.message_args,
                details: error.details,
            })))
        }
        Err(_) => {
            let response = serde_json::from_value(response_body).map_err(|error| {
                AppError::Internal(format!("decode legacy idempotent response: {error}"))
            })?;
            Ok(AdminCommandOutcome {
                response,
                status,
                resource_id: record.resource_id,
                replayed: true,
            })
        }
    }
}

fn http_status(status: u16) -> Result<axum::http::StatusCode, AppError> {
    axum::http::StatusCode::from_u16(status)
        .map_err(|_| AppError::Internal(format!("invalid idempotent response status {status}")))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize(value)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        scalar => scalar,
    }
}

fn invalid_command(error: serde_json::Error) -> AppError {
    AppError::BadRequest(format!("invalid idempotent command: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasher() -> IdempotencyHasher {
        IdempotencyHasher::new(b"command-pepper".to_vec(), "v1")
    }

    #[test]
    fn command_hash_is_keyed_and_versioned() {
        let spec = AdminCommandSpec::new(
            "credential.create",
            json!({}),
            json!({"secret": "sk-live-not-recoverable"}),
        )
        .unwrap();

        let hashed = spec.request_hash(&hasher()).unwrap();
        assert!(hashed.starts_with("v1:"));
        assert_ne!(hashed, spec.legacy_request_hash().unwrap());
        assert_ne!(
            hashed,
            spec.request_hash(&IdempotencyHasher::new(b"other".to_vec(), "v1"))
                .unwrap(),
            "the pepper must actually key the command envelope digest"
        );
    }

    #[test]
    fn command_hash_is_stable_across_object_key_order() {
        let left = AdminCommandSpec::new(
            "application.create",
            json!({"provider_id": "one", "application_id": "two"}),
            json!({"metadata": {"b": 2, "a": 1}}),
        )
        .unwrap();
        let right = AdminCommandSpec::new(
            "application.create",
            json!({"application_id": "two", "provider_id": "one"}),
            json!({"metadata": {"a": 1, "b": 2}}),
        )
        .unwrap();

        let hasher = hasher();
        assert_eq!(
            left.request_hash(&hasher).unwrap(),
            right.request_hash(&hasher).unwrap()
        );
    }

    #[test]
    fn command_hash_includes_path_and_expected_version() {
        let base = AdminCommandSpec::new(
            "credential.rotate",
            json!({"credential_id": "one"}),
            json!({"secret": "hashed-only"}),
        )
        .unwrap();
        let other_path = AdminCommandSpec::new(
            "credential.rotate",
            json!({"credential_id": "two"}),
            json!({"secret": "hashed-only"}),
        )
        .unwrap();
        let versioned = base.clone().with_expected_version(Some(4));

        let hasher = hasher();
        assert_ne!(
            base.request_hash(&hasher).unwrap(),
            other_path.request_hash(&hasher).unwrap()
        );
        assert_ne!(
            base.request_hash(&hasher).unwrap(),
            versioned.request_hash(&hasher).unwrap()
        );
    }

    #[test]
    fn replayed_error_uses_the_current_request_id() {
        let error = AppError::Replayed(Box::new(ReplayedError {
            status: axum::http::StatusCode::CONFLICT,
            code: "duplicate".to_string(),
            message_key: "moira.error.duplicate".to_string(),
            message: "duplicate".to_string(),
            message_args: json!({"resource": "application"}),
            details: None,
        }));

        let response = error.error_response(Some("fresh-request".to_string()));
        assert_eq!(response.error.request_id, "fresh-request");
        assert_eq!(response.error.message_args["resource"], "application");
    }
}
