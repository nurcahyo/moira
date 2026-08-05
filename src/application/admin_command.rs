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

/// The idempotency envelope of a command: the raw `Idempotency-Key` and the canonical
/// actor-identity bytes.
///
/// Both are carried *unhashed* so that [`AdminCommandRunner`] — the layer that actually owns
/// the [`IdempotencyHasher`] — derives the current keyed value and the legacy unkeyed one
/// from the same bytes. Storing a pre-hashed fingerprint here would put the choice of
/// keying at each of the thirty spec-building call sites instead of in the one place that
/// knows whether the dual-read window is open.
#[derive(Debug, Clone)]
pub struct AdminCommandIdempotency {
    pub key: String,
    /// `crate::application::admin::actor_identity_bytes` output. Not a digest.
    pub actor_identity: Vec<u8>,
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
    /// Whether pre-switch, unkeyed ledger values are still honoured on read — the
    /// migration window from `idempotency.accept_legacy_hashes` (plan 03 finding F4).
    ///
    /// `true` reproduces the behaviour shipped with the HMAC switch. `false` stops
    /// accepting unkeyed digests **and** drops the extra legacy lookup per claim.
    ///
    /// It is `true` on every production runner today: nothing outside the tests calls
    /// [`Self::accepting_legacy_hashes`], so the operator setting never reaches this field
    /// and the key-hash window on this path cannot actually be closed. Pre-existing since
    /// plan 03, not introduced with the pepper. Tracked in `TODO.md` and issue #125.
    accept_legacy_hashes: bool,
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
    /// Builds a runner with the dual-read window **open**, which is the behaviour the HMAC
    /// switch shipped with. Callers that have a `Settings` in hand should immediately chain
    /// [`Self::accepting_legacy_hashes`].
    pub fn new(repository: PgAdminRepository, hasher: IdempotencyHasher) -> Self {
        Self {
            repository,
            hasher,
            accept_legacy_hashes: true,
        }
    }

    /// Wires `idempotency.accept_legacy_hashes` (plan 03 finding F4).
    ///
    /// A builder rather than a `new` parameter so the construction sites that existed when
    /// plan 03 landed kept compiling and kept their current behaviour until each was opted
    /// in. **None of them has been opted in yet** — the only caller is a test — so
    /// `idempotency.accept_legacy_hashes` is inert on the admin-command path. There are now
    /// nineteen production sites, spread across `application/identity.rs`,
    /// `application/conversation.rs`, `application/admin/` and `application/auth_settings.rs`
    /// rather than the two modules this note originally named. Wiring them is its own change,
    /// tracked in `TODO.md` and issue #125.
    #[must_use]
    pub fn accepting_legacy_hashes(mut self, accept_legacy_hashes: bool) -> Self {
        self.accept_legacy_hashes = accept_legacy_hashes;
        self
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
        // The canonical envelope is needed twice — once to derive the digest a fresh claim
        // writes, and once to *verify* a stored digest in constant time on replay — so it
        // is computed once here rather than re-serialized per use.
        let envelope = match spec.idempotency {
            Some(_) => Some(spec.envelope_bytes()?),
            None => None,
        };
        let claim = match (spec.idempotency.as_ref(), envelope.as_deref()) {
            (Some(idempotency), Some(envelope)) => Some(AdminIdempotencyClaim {
                record_id: Uuid::now_v7(),
                key_hash: self.hasher.hash(idempotency.key.as_bytes()),
                // The key hash is an index key under the unique index on
                // (idempotency_key_hash, actor_fingerprint, operation), so a legacy row
                // is unreachable by the versioned hash alone. Carrying the legacy value
                // keeps pre-deploy claims replayable until they expire — and dropping it
                // once the window is closed removes the extra SELECT it costs on every
                // idempotent admin write (plan 03 finding F4).
                legacy_key_hash: self
                    .accept_legacy_hashes
                    .then(|| self.hasher.legacy_hash(idempotency.key.as_bytes())),
                actor_fingerprint: self.hasher.hash(&idempotency.actor_identity),
                // The fingerprint is the *second* column of that same unique index and it
                // moved from an unkeyed digest to a keyed one when the pepper landed, so a
                // row written before *that* deploy sits at a point of the index the peppered
                // value cannot address. Probing the unkeyed spelling keeps those rows
                // replayable until they expire; the write path above emits only the peppered
                // value, so the legacy one can never re-enter the ledger.
                //
                // Deliberately NOT gated on `accept_legacy_hashes`. That switch governs the
                // plan-03 *key-hash* window, which operators were told to close 24h after
                // that deploy — an instruction a deployment may already have followed.
                // Hanging the fingerprint window off it would ship this
                // change with no dual-read at all on the very path it exists to protect, and
                // every pre-pepper admin ledger row would become unreachable the moment the
                // pepper landed: a retried admin command would execute a second time instead
                // of replaying. The two windows opened at different deploys and must close
                // independently — see the matching unconditional probes in
                // `runtime_admin::idempotency_replay` and `public::claim_idempotency`.
                legacy_actor_fingerprint: Some(
                    self.hasher.legacy_hash(&idempotency.actor_identity),
                ),
                operation: spec.operation.clone(),
                request_hash: self.hasher.hash(envelope),
                expires_at: Utc::now() + Duration::hours(IDEMPOTENCY_RETENTION_HOURS),
            }),
            _ => None,
        };

        if let Some(claim) = &claim {
            match transaction.claim_idempotency(claim).await? {
                AdminIdempotencyClaimOutcome::Acquired => {}
                AdminIdempotencyClaimOutcome::Existing(record) => {
                    let envelope = envelope
                        .as_deref()
                        .expect("an idempotent claim always carries its envelope");
                    // Returning `Err` here drops `transaction`, which rolls back — the same
                    // outcome the repository produced when it raised these two conflicts
                    // itself.
                    verify_stored_request_hash(
                        &self.hasher,
                        self.accept_legacy_hashes,
                        envelope,
                        &record.request_hash,
                    )?;
                    if record.response_status.is_none() || record.response_body.is_none() {
                        return Err(AppError::conflict(
                            "idempotency_in_progress",
                            "another request with this Idempotency-Key is still in progress",
                        ));
                    }
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

/// Checks that a *stored* body digest describes the request being executed.
///
/// Runs through [`IdempotencyHasher::verify`], i.e. through `Mac::verify_slice`, which is
/// constant-time and length-checked. It replaces the byte-wise, early-exit `String`
/// equality the repository used to perform (plan 03 finding F3) — the path that carries
/// `POST /api/v1/admin/provider-credentials`, `.../system-keys` and `.../consumer-keys`
/// bodies, and therefore the one place the plan's stated constant-time property matters
/// most.
///
/// A stored value with no `':'` separator is a pre-switch, unkeyed SHA-256. Once the
/// dual-read window is closed (`accept_legacy_hashes = false`) it is rejected outright
/// instead of being handed to `verify`'s legacy arm (finding F4). See the note on
/// `AdminCommandRunner::accept_legacy_hashes`: no production runner closes it today, because
/// nothing wires the setting into the runner — issue #125.
fn verify_stored_request_hash(
    hasher: &IdempotencyHasher,
    accept_legacy_hashes: bool,
    envelope: &[u8],
    stored: &str,
) -> Result<(), AppError> {
    let is_legacy_format = !stored.contains(':');
    let matches = (accept_legacy_hashes || !is_legacy_format) && hasher.verify(envelope, stored);
    if matches {
        Ok(())
    } else {
        Err(AppError::conflict(
            "idempotency_conflict",
            "same Idempotency-Key was used with a different request",
        ))
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

    /// Plan 03 finding F3: the stored-vs-live comparison must go through
    /// `IdempotencyHasher::verify`, and it must still accept both formats while the
    /// dual-read window is open.
    #[test]
    fn stored_request_hash_verification_accepts_both_formats_while_the_window_is_open() {
        let hasher = hasher();
        let spec = AdminCommandSpec::new(
            "credential.create",
            json!({}),
            json!({"secret": "sk-live-not-recoverable"}),
        )
        .unwrap();
        let envelope = spec.envelope_bytes().unwrap();

        verify_stored_request_hash(
            &hasher,
            true,
            &envelope,
            &spec.request_hash(&hasher).unwrap(),
        )
        .expect("the versioned digest must verify");
        verify_stored_request_hash(
            &hasher,
            true,
            &envelope,
            &spec.legacy_request_hash().unwrap(),
        )
        .expect("the pre-switch unkeyed digest must still verify while the window is open");

        let other = AdminCommandSpec::new("credential.create", json!({}), json!({"secret": "no"}))
            .unwrap()
            .request_hash(&hasher)
            .unwrap();
        let error = verify_stored_request_hash(&hasher, true, &envelope, &other).unwrap_err();
        assert!(
            error.to_string().contains("different request"),
            "unexpected error: {error}"
        );
    }

    /// Plan 03 finding F4: closing the window must actually stop the unkeyed arm from
    /// matching, without disturbing the keyed one.
    #[test]
    fn closing_the_legacy_window_rejects_unkeyed_stored_digests() {
        let hasher = hasher();
        let spec =
            AdminCommandSpec::new("system_key.create", json!({}), json!({"label": "ops"})).unwrap();
        let envelope = spec.envelope_bytes().unwrap();

        verify_stored_request_hash(
            &hasher,
            false,
            &envelope,
            &spec.request_hash(&hasher).unwrap(),
        )
        .expect("the versioned digest must keep verifying after the window closes");

        let legacy = spec.legacy_request_hash().unwrap();
        assert!(
            !legacy.contains(':'),
            "the pre-switch format carries no version prefix"
        );
        assert!(
            verify_stored_request_hash(&hasher, false, &envelope, &legacy).is_err(),
            "an unkeyed digest must not match once accept_legacy_hashes is false"
        );
        assert!(
            hasher.verify(&envelope, &legacy),
            "the hasher itself still accepts it — the gate is the runner's, which is what \
             makes the setting the thing that closes the window"
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
