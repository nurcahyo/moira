//! Containerised Claude runner orchestration (issue #275, workstream R2 of #272).
//!
//! # The one rule this file exists to hold
//!
//! **The minted token never leaves the Moira process.** [`ClaudeRunnerService::finalize`] reads it
//! from `moira-runner`'s one-shot token endpoint and moves it, in the same expression, into
//! [`AdminService::create_credential`] — the existing chain that builds the AAD, encrypts under
//! `SecretCipher`, fingerprints, masks, writes the audit row and runs inside the idempotent
//! command envelope. From there it is a `provider_credentials` row like any other.
//!
//! It is never returned in a response body, never logged, never traced and never placed in an
//! error. `secrecy::ExposeSecret` is called exactly **once** in this file, at the point the
//! `CredentialSecret` is constructed, and that call site is the only place the plaintext exists
//! as a `&str`.
//!
//! # Moira holds no Docker access
//!
//! Every state below arrived over HTTP from `moira-runner`, which is the only component in the
//! deployment with Docker Engine API access. Nothing in this file, or anything it calls, may take
//! a Docker dependency — that separation is the entire security point of running the runner as a
//! separate process.
//!
//! # Why the transitions carry no `If-Match`
//!
//! [`ClaudeRunnerService::get`] refreshes the mirror from the runner service, which bumps
//! `version` — so a console polling for the authorization URL would invalidate its own ETag
//! between poll and submit. The transitions are guarded by an explicit *from-state* check inside
//! the same transaction as the write instead, which is the stronger guarantee: a version match
//! proves only that nobody else wrote, while a state match proves the transition is legal.
//! `DELETE` keeps the precondition, because destroying a container is not a state-machine step
//! and a lost-update there is a container removed out from under a concurrent operator.

use std::sync::Arc;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        AdminCommandMutation, AdminCommandRunner, AdminService, RequestContext,
        admin::shared::{
            PageRequest, admin_command_spec, authorize_credential_scope, command_hasher, paginate,
            success_audit, validate_credential_scope_shape,
        },
    },
    domain::{
        ClaudeRunnerAuthorizationCodeRequest, ClaudeRunnerFinalizeRequest,
        ClaudeRunnerProvisionRequest, ClaudeRunnerRecord, ClaudeRunnerState,
        CredentialCreateRequest, CredentialScope, CredentialSecret, CursorScope, ListCursor,
        ListResponse,
    },
    error::AppError,
    infra::{
        repositories::{
            ClaudeRunnerInsert, ClaudeRunnerRepository, PgAdminRepository,
            PgClaudeRunnerRepository, RunnerStateUpdate, runner_version_conflict,
        },
        runner_control::{RunnerControl, RunnerControlClient, RunnerControlError, RunnerStatus},
    },
    security::Actor,
};

/// Its own scope, so a cursor issued for another list cannot be replayed against this one.
const RUNNERS_CURSOR: CursorScope = CursorScope::new("admin.claude_runners");

/// Bounds on the caller-supplied TTL.
///
/// A runner is an interactive login window held open by a container. The floor stops a caller
/// provisioning a runner that expires before the operator can read the URL; the ceiling stops one
/// living for a week holding a half-finished OAuth flow, which is the state with the largest
/// blast radius in the whole flow.
const MIN_TTL_SECONDS: u32 = 60;
const MAX_TTL_SECONDS: u32 = 3_600;

/// The label charset `moira-runner` accepts, because the value becomes part of a container name.
const MAX_LABEL_LENGTH: usize = 64;

pub struct ClaudeRunnerService<'a> {
    state: &'a AppState,
    repo: PgAdminRepository,
    runners: Arc<dyn ClaudeRunnerRepository>,
    /// `None` when `claude_runner.enabled` is false, which is the default. Every method that
    /// needs it goes through [`Self::control`], which turns the absence into one named `503`.
    control: Option<Arc<dyn RunnerControl>>,
}

impl<'a> ClaudeRunnerService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        let control = RunnerControlClient::from_settings(&state.settings.claude_runner)?
            .map(|client| Arc::new(client) as Arc<dyn RunnerControl>);
        Ok(Self {
            state,
            repo: PgAdminRepository::new(pool.clone()),
            runners: Arc::new(PgClaudeRunnerRepository::new(pool)),
            control,
        })
    }

    fn control(&self) -> Result<&Arc<dyn RunnerControl>, AppError> {
        self.control.as_ref().ok_or_else(runner_service_disabled)
    }

    pub async fn provision(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: ClaudeRunnerProvisionRequest,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        self.state.authz.require(actor, "moira:runners:write")?;
        validate_label(&request.label)?;
        validate_ttl(request.ttl_seconds)?;
        // Absent means the platform-wide account, which is what a deployment that has never heard
        // of tenant runners gets.
        let scope = request.scope.clone().unwrap_or(CredentialScope::Global);
        // The SAME two checks `CredentialAdminService::create_credential` applies to a
        // caller-supplied scope, run here at provisioning time. Running them now rather than only
        // at finalize is the point: the finalize path has already spent the one-shot token by the
        // time the credential chain sees the scope, so a scope this deployment would refuse must
        // be refused before a container is ever started.
        //
        // **Honest note on how strong `authorize_credential_scope` actually is.** It constrains
        // only actors bound to an application (`Actor::internal_application_id`, i.e. consumer-key
        // principals). A system-key or trusted-JWT admin holding `moira:credentials:write` may
        // create a credential for ANY `external_tenant_id` today — Moira has no per-tenant
        // authorization model, on this surface or on the credential surface it is copied from.
        // That is deliberately NOT tightened here: a stricter rule on runners than on the
        // credential API they write through would be a second, divergent answer to the same
        // question, and the gap belongs to the credential surface. It is recorded in the PR.
        validate_credential_scope_shape(&scope)?;
        authorize_credential_scope(actor, &scope)?;
        let control = self.control()?;

        // The container is started BEFORE the mirror row is written, deliberately. The reverse
        // order would leave a row naming a runner that does not exist whenever the control call
        // fails, and nothing on this side could tell that row apart from one whose container was
        // reaped. A control call that succeeds and a row write that then fails leaves an orphan
        // container instead — which the runner service's own TTL reaper removes, because
        // `ttl_seconds` is set on creation and reaping is driven off the Docker labels.
        let status = control
            .create(&request.label, request.ttl_seconds)
            .await
            .map_err(map_control_error)?;

        // # What `Idempotency-Key` does and does not buy on this route — stated plainly
        //
        // The envelope makes the **row** idempotent: a replay returns the original record rather
        // than writing a second one. It does **not** make the container idempotent, because the
        // control call above has already happened by the time the envelope is consulted. A
        // genuine replay therefore starts a second container and then discards it by returning the
        // stored response.
        //
        // That orphan is bounded rather than leaked: `ttl_seconds` is set at creation and the
        // runner service's reaper is driven off the Docker labels, so it is force-removed within
        // the TTL whether or not Moira ever refers to it again.
        //
        // The alternative — performing the control call *inside* the command transaction — trades
        // this for an HTTP round trip held open across a database transaction, on a route whose
        // upstream starts a container. That is the worse failure: a slow runner service would pin
        // a connection and a row lock together. The ordering here is deliberate; the caveat is the
        // price.
        let spec = admin_command_spec(ctx, actor, "claude_runner.provision", json!({}), &request)?;
        let actor = actor.clone();
        let ctx = ctx.clone();
        let runners = self.runners.clone();
        let label = request.label.clone();
        let metadata = request.metadata.clone();
        let stored_scope = scope.clone();
        let expires_at = parse_expires_at(status.expires_at.as_deref());
        let reference = status.id.clone();
        let outcome = AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))
            .execute(spec, move |transaction| {
                Box::pin(async move {
                    let id = Uuid::now_v7();
                    let record = runners
                        .create(
                            transaction.connection(),
                            ClaudeRunnerInsert {
                                id,
                                label: &label,
                                runner_reference: &reference,
                                provider_id: None,
                                scope: &stored_scope,
                                expires_at,
                                metadata: &metadata,
                            },
                        )
                        .await?;
                    transaction
                        .insert_audit(success_audit(
                            &actor,
                            &ctx,
                            "claude_runner.provision",
                            "claude_runner",
                            Some(record.id.to_string()),
                            // `runner_reference` is an opaque id from another service and is
                            // recorded so an operator can correlate the two logs. Nothing here
                            // is, or has been near, credential material.
                            json!({
                                "label": record.label,
                                "runner_reference": record.runner_reference,
                                "scope": record.scope,
                            }),
                        ))
                        .await?;
                    AdminCommandMutation::new(record.clone(), 201, Some(record.id.to_string()))
                })
            })
            .await?;
        Ok(outcome.response)
    }

    pub async fn list(
        &self,
        actor: &Actor,
        page: impl Into<PageRequest>,
    ) -> Result<ListResponse<ClaudeRunnerRecord>, AppError> {
        self.state.authz.require(actor, "moira:runners:read")?;
        let page = page.into();
        // Deliberately no refresh. A list of N runners would be N calls into a service that talks
        // to the Docker daemon, on every console poll; the per-runner `GET` is where a live read
        // belongs, because that is the one the operator is actually watching.
        let rows = self
            .runners
            .list(page.decode(RUNNERS_CURSOR)?, page.limit())
            .await?;
        Ok(paginate(rows, &page, RUNNERS_CURSOR, |row| {
            ListCursor::new(row.created_at, row.id)
        }))
    }

    /// Reads a runner, refreshing Moira's mirror from the runner service first when the runner
    /// could still be moving.
    ///
    /// The refresh is what surfaces `authorization_url`: it is scraped out of the container's tty
    /// stream by the runner service and does not exist at provision time, so a console that only
    /// read the mirror would never see it. A terminal runner is answered from the mirror with no
    /// outbound call, which is what stops a dashboard left open overnight from holding one
    /// connection per row into a service with Docker access.
    ///
    /// The refresh writes, and therefore bumps `version`. That is why the state transitions carry
    /// no `If-Match` — see the module header.
    pub async fn get(&self, actor: &Actor, id: Uuid) -> Result<ClaudeRunnerRecord, AppError> {
        self.state.authz.require(actor, "moira:runners:read")?;
        let record = self.runners.get(id).await?;
        if record.state.is_terminal() || self.control.is_none() {
            return Ok(record);
        }
        match self.control()?.status(&record.runner_reference).await {
            Ok(status) => self.apply_refresh(&record, &status).await,
            // A runner the control plane no longer knows about has been reaped past its TTL. That
            // is an ordinary end of life, not a missing resource: the row stays, marked `expired`,
            // so the console can show what happened instead of a runner vanishing from its list.
            Err(error) if error.status == Some(StatusCode::NOT_FOUND) => {
                self.runners
                    .transition(
                        record.id,
                        NON_TERMINAL_STATES,
                        RunnerStateUpdate {
                            state: Some(ClaudeRunnerState::Expired),
                            ..RunnerStateUpdate::default()
                        },
                        None,
                    )
                    .await
            }
            Err(error) => Err(map_control_error(error)),
        }
    }

    async fn apply_refresh(
        &self,
        record: &ClaudeRunnerRecord,
        status: &RunnerStatus,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        let Some(state) = state_from_contract(&status.state) else {
            // A state string this build does not know is a newer runner service, not a corrupt
            // row. Returning the mirror unchanged keeps the console working against a rolling
            // deploy; refusing would take the whole surface down for the length of one.
            tracing::warn!(
                runner_id = %record.id,
                upstream_state = %status.state,
                "unknown moira-runner state; leaving the mirror unchanged"
            );
            return Ok(record.clone());
        };
        // `Linked` is Moira's own terminal state and must never be walked back to the upstream
        // `ready` it was reached from — the runner service's `ready` outlives the one-shot token
        // read that produced the credential.
        if record.state == ClaudeRunnerState::Linked {
            return Ok(record.clone());
        }
        self.runners
            .transition(
                record.id,
                NON_TERMINAL_STATES,
                RunnerStateUpdate {
                    state: Some(state),
                    authorization_url: status.authorization_url.clone(),
                    error_code: status.error_code.clone(),
                    expires_at: parse_expires_at(status.expires_at.as_deref()),
                    credential_id: None,
                },
                None,
            )
            .await
    }

    /// Forwards the operator's pasted authorization code to the runner service.
    ///
    /// The code is a single-use OAuth authorization code. It is forwarded and dropped: never
    /// stored, never logged, never echoed back, and it does not appear in the audit metadata
    /// below.
    pub async fn submit_authorization_code(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: ClaudeRunnerAuthorizationCodeRequest,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        self.state.authz.require(actor, "moira:runners:write")?;
        if request.code.trim().is_empty() {
            return Err(AppError::unprocessable(
                "runner_request_rejected",
                "an authorization code is required",
            ));
        }
        let record = self.runners.get(id).await?;
        let control = self.control()?;
        // Refuse locally first when the mirror already proves the transition illegal: one round
        // trip saved, and the same code either way — the relayed upstream 409 maps here too.
        if record.state != ClaudeRunnerState::AwaitingAuthorization {
            return Err(crate::infra::repositories::runner_wrong_state());
        }
        control
            .submit_authorization_code(&record.runner_reference, request.code.trim())
            .await
            .map_err(map_control_error)?;
        self.runners
            .transition(
                id,
                &[ClaudeRunnerState::AwaitingAuthorization],
                RunnerStateUpdate {
                    state: Some(ClaudeRunnerState::Exchanging),
                    ..RunnerStateUpdate::default()
                },
                Some(success_audit(
                    actor,
                    ctx,
                    "claude_runner.authorization_code",
                    "claude_runner",
                    Some(id.to_string()),
                    // No `code` field, and none may be added: this row is readable through
                    // `GET /api/v1/admin/audit-events`.
                    json!({}),
                )),
            )
            .await
    }

    /// Fetches the minted token and stores it as a provider credential.
    ///
    /// # The security-critical path
    ///
    /// 1. The mirror is refreshed, so a runner that reached `ready` upstream can be finalized in
    ///    one call rather than requiring the console to poll first.
    /// 2. The state must be `ready`. Anything else is `409 runner_wrong_state`, decided before
    ///    the token endpoint is touched — a token read is one-shot, so a speculative one is a
    ///    token destroyed.
    /// 3. The token is read into a [`SecretString`] and moved straight into
    ///    [`AdminService::create_credential`]. `expose_secret` is called once, inline, at the
    ///    construction of the [`CredentialSecret`].
    /// 4. Only after the credential row exists is the runner marked `linked`.
    ///
    /// # What a failure at step 3 leaves behind
    ///
    /// Nothing. No credential row, no state change — the mirror is untouched and the error is
    /// returned. The token itself is gone, because the runner service's token endpoint is
    /// one-shot, so a second finalize answers `410 token_already_retrieved` and maps to
    /// `runner_token_unavailable`. That is the honest signal: the operator must delete the runner
    /// and provision a new one. Marking the row `failed` here was considered and rejected —
    /// writing a state change on the failure path is exactly the "stores nothing" property this
    /// step is tested for, and the second attempt's `410` already names the situation.
    pub async fn finalize(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: ClaudeRunnerFinalizeRequest,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        self.state.authz.require(actor, "moira:runners:write")?;
        // `moira:credentials:write` is checked **here as well as** inside
        // `create_credential`, and the duplication is the point. The credential chain's own
        // check runs after the token has been read, and that read is one-shot: an authorization
        // failure at that depth would destroy a token that no retry can recover, turning a
        // permissions mistake into a runner that has to be thrown away. Checking first costs
        // nothing and makes the refusal free.
        //
        // The chain's check is emphatically not removed. It is the one that is load-bearing —
        // this one is an optimisation of the failure path, and a check that exists only at the
        // caller is a check that a second caller will not have.
        self.state.authz.require(actor, "moira:credentials:write")?;
        let record = self.get(actor, id).await?;
        if record.state != ClaudeRunnerState::Ready {
            return Err(crate::infra::repositories::runner_wrong_state());
        }

        let token: SecretString = self
            .control()?
            .fetch_token(&record.runner_reference)
            .await
            .map_err(map_control_error)?;

        let credential = AdminService::new(self.state)?
            .create_credential(
                actor,
                ctx,
                CredentialCreateRequest {
                    provider_id: request.provider_id,
                    credential_type: ClaudeRunnerFinalizeRequest::CREDENTIAL_TYPE,
                    // **The runner's own stored scope, never one from this request.** The request
                    // has no scope field and `deny_unknown_fields` refuses one, because the scope
                    // is sealed into the credential's AAD (`credential_aad`) and a value chosen
                    // here could contradict the one the console has displayed since provisioning.
                    // A tenant's runner therefore produces a `tenant`-scoped credential, which
                    // `resolve_runtime_credential` already ranks above the platform's `global`
                    // one for that tenant.
                    scope: record.scope.clone(),
                    // The only `expose_secret` in this file. The value is constructed and moved;
                    // it is never bound to a named local, never formatted, never logged.
                    secret: CredentialSecret::OAuth2 {
                        access_token: token.expose_secret().to_string(),
                        refresh_token: None,
                        token_type: Some("Bearer".to_string()),
                        expires_at: None,
                    },
                    display_name: request
                        .display_name
                        .clone()
                        .or_else(|| Some(format!("claude-runner {}", record.label))),
                    priority: 100,
                    expires_at: None,
                    metadata: request.metadata.clone(),
                },
            )
            .await?;
        drop(token);

        self.runners
            .transition(
                id,
                &[ClaudeRunnerState::Ready],
                RunnerStateUpdate {
                    state: Some(ClaudeRunnerState::Linked),
                    credential_id: Some(credential.id),
                    ..RunnerStateUpdate::default()
                },
                Some(success_audit(
                    actor,
                    ctx,
                    "claude_runner.finalize",
                    "claude_runner",
                    Some(id.to_string()),
                    json!({
                        "provider_id": request.provider_id,
                        "credential_id": credential.id,
                        "scope": record.scope,
                    }),
                )),
            )
            .await
    }

    /// Tears the container down and soft-deletes the mirror row.
    ///
    /// The credential a finalized runner produced is deliberately **not** deleted with it: it is
    /// an ordinary `provider_credentials` row that routing may be executing against right now,
    /// and deleting it as a side effect of tidying up a runner would take a working provider
    /// offline. `credential_id` on the row is `on delete set null` for the mirror image of the
    /// same reason.
    ///
    /// # The `If-Match` precondition is checked twice, and the early one is not redundant
    ///
    /// `soft_delete` re-checks the version inside its own transaction, and that check is the
    /// authoritative one — it holds a `for update` lock, so it is the only one that closes the
    /// race. But it runs *after* the container has been destroyed, and destroying a container is
    /// not something a later `409` undoes.
    ///
    /// Without the early check below, a caller presenting a stale ETag got a `409` saying the
    /// delete had not happened while the runner's container had in fact already been removed —
    /// which is precisely the lost update the precondition exists to prevent, in its most
    /// irreversible form. Caught by
    /// `deleting_a_runner_needs_if_match_and_leaves_its_credential_alone`, which observed the
    /// runner service's delete counter reading 2 for one successful delete.
    ///
    /// Same shape as the `moira:credentials:write` pre-check in [`Self::finalize`]: refuse before
    /// the irreversible step, and leave the load-bearing check where it already was.
    pub async fn delete(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:runners:delete")?;
        let record = self.runners.get(id).await?;
        if record.version != expected_version {
            return Err(runner_version_conflict());
        }
        // Best effort, and the ordering is deliberate: the container is destroyed first, so a
        // failure to remove it is reported rather than silently leaving a container behind a row
        // that no longer exists. `delete` is idempotent at the far end (204 when already gone),
        // so re-running after a partial failure is safe.
        if let Some(control) = self.control.as_ref() {
            control
                .delete(&record.runner_reference)
                .await
                .map_err(map_control_error)?;
        }
        self.runners
            .soft_delete(
                id,
                expected_version,
                success_audit(
                    actor,
                    ctx,
                    "claude_runner.delete",
                    "claude_runner",
                    Some(id.to_string()),
                    json!({ "runner_reference": record.runner_reference }),
                ),
            )
            .await
    }
}

/// Every state a refresh may legally write over.
///
/// Terminal states are excluded so a late reply from the control plane cannot resurrect a runner
/// that has already been linked, failed or expired.
const NON_TERMINAL_STATES: &[ClaudeRunnerState] = &[
    ClaudeRunnerState::Provisioning,
    ClaudeRunnerState::AwaitingAuthorization,
    ClaudeRunnerState::Exchanging,
    ClaudeRunnerState::Ready,
];

fn runner_service_disabled() -> AppError {
    AppError::coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "runner_service_disabled",
        "containerised Claude runners are not enabled in this deployment",
    )
}

pub(crate) fn validate_label(label: &str) -> Result<(), AppError> {
    if label.is_empty() || label.len() > MAX_LABEL_LENGTH {
        return Err(AppError::unprocessable(
            "runner_label_invalid",
            "a runner label must be 1 to 64 characters of [a-z0-9-]",
        ));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::unprocessable(
            "runner_label_invalid",
            "a runner label must be 1 to 64 characters of [a-z0-9-]",
        ));
    }
    Ok(())
}

pub(crate) fn validate_ttl(ttl_seconds: u32) -> Result<(), AppError> {
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(AppError::unprocessable(
            "runner_ttl_invalid",
            format!("ttl_seconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"),
        ));
    }
    Ok(())
}

/// The contract's state vocabulary. `None` for anything else — see [`ClaudeRunnerService::apply_refresh`].
///
/// `linked` is absent on purpose: it is Moira's own state and the runner service never emits it.
/// Accepting it here would let a compromised or buggy control plane declare a runner credentialed
/// without a credential ever having been written.
pub(crate) fn state_from_contract(value: &str) -> Option<ClaudeRunnerState> {
    Some(match value {
        "provisioning" => ClaudeRunnerState::Provisioning,
        "awaiting_authorization" => ClaudeRunnerState::AwaitingAuthorization,
        "exchanging" => ClaudeRunnerState::Exchanging,
        "ready" => ClaudeRunnerState::Ready,
        "failed" => ClaudeRunnerState::Failed,
        "expired" => ClaudeRunnerState::Expired,
        _ => return None,
    })
}

fn parse_expires_at(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Maps a control-plane failure onto Moira's own coded error.
///
/// **Nothing from the upstream response body reaches the caller.** [`RunnerControlError`] carries
/// a status and a validated `code` identifier and nothing else, and this function reads only
/// those. The runner service scrapes a container's tty stream — which is where the token lives —
/// so relaying its prose would put an unbounded string that has been adjacent to credential
/// material into a Moira response and a Moira log line at once.
pub(crate) fn map_control_error(error: RunnerControlError) -> AppError {
    if let Some(code) = error.code.as_deref() {
        match code {
            "runner_not_found" => {
                return AppError::coded(
                    StatusCode::NOT_FOUND,
                    "runner_not_found",
                    "the runner was not found",
                );
            }
            "runner_wrong_state" => return crate::infra::repositories::runner_wrong_state(),
            "token_already_retrieved" => return runner_token_unavailable(),
            _ => {}
        }
    }
    match error.status {
        Some(StatusCode::NOT_FOUND) => AppError::coded(
            StatusCode::NOT_FOUND,
            "runner_not_found",
            "the runner was not found",
        ),
        Some(StatusCode::CONFLICT) => crate::infra::repositories::runner_wrong_state(),
        Some(StatusCode::GONE) => runner_token_unavailable(),
        Some(StatusCode::BAD_REQUEST) => AppError::unprocessable(
            "runner_request_rejected",
            "the runner service rejected the request",
        ),
        // Moira's own bearer token is wrong or has been rotated out from under it. Separated from
        // the generic unavailability code because the remedy is different in kind: an operator
        // fixes configuration, not capacity, and folding the two together costs them a page.
        Some(StatusCode::UNAUTHORIZED) | Some(StatusCode::FORBIDDEN) => AppError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "runner_service_unauthorized",
            "Moira is not authorized to call the runner service",
        ),
        _ => AppError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "runner_service_unavailable",
            "the runner service could not be reached",
        ),
    }
}

fn runner_token_unavailable() -> AppError {
    AppError::conflict(
        "runner_token_unavailable",
        "this runner's token has already been retrieved and cannot be read again",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_must_be_container_safe() {
        for label in ["claude-1", "a", "runner-01", &"a".repeat(64)] {
            assert!(validate_label(label).is_ok(), "{label:?} must be accepted");
        }
        for label in [
            "",
            &"a".repeat(65),
            "Claude",
            "runner_1",
            "runner 1",
            "runner/1",
            "../etc",
        ] {
            let error = validate_label(label).expect_err("{label:?} must be refused");
            assert!(
                format!("{error}").contains("runner_label_invalid"),
                "{label:?} must be refused with runner_label_invalid, got {error}"
            );
        }
    }

    #[test]
    fn a_ttl_outside_the_window_is_refused_rather_than_clamped() {
        assert!(validate_ttl(60).is_ok());
        assert!(validate_ttl(900).is_ok());
        assert!(validate_ttl(3_600).is_ok());
        for ttl in [0, 59, 3_601, u32::MAX] {
            assert!(
                validate_ttl(ttl).is_err(),
                "ttl {ttl} must be refused, not silently clamped: a clamp makes a \
                 misconfiguration invisible"
            );
        }
    }

    /// `linked` must never be accepted from the control plane.
    ///
    /// It is Moira's own terminal state and it means "a credential row exists". A control plane
    /// able to assert it could mark a runner credentialed with no credential ever written.
    #[test]
    fn the_contract_decoder_refuses_moiras_own_linked_state() {
        assert_eq!(state_from_contract("ready"), Some(ClaudeRunnerState::Ready));
        assert_eq!(state_from_contract("linked"), None);
        assert_eq!(state_from_contract("something-new"), None);
    }

    #[test]
    fn every_contract_state_except_linked_decodes() {
        for (raw, expected) in [
            ("provisioning", ClaudeRunnerState::Provisioning),
            (
                "awaiting_authorization",
                ClaudeRunnerState::AwaitingAuthorization,
            ),
            ("exchanging", ClaudeRunnerState::Exchanging),
            ("ready", ClaudeRunnerState::Ready),
            ("failed", ClaudeRunnerState::Failed),
            ("expired", ClaudeRunnerState::Expired),
        ] {
            assert_eq!(state_from_contract(raw), Some(expected), "state {raw}");
        }
    }

    /// The upstream error vocabulary maps onto Moira's codes, and nothing from a body escapes.
    #[test]
    fn control_errors_map_onto_coded_moira_errors_and_carry_no_upstream_prose() {
        let cases: [(RunnerControlError, StatusCode, &str); 6] = [
            (
                RunnerControlError {
                    status: Some(StatusCode::NOT_FOUND),
                    code: Some("runner_not_found".to_string()),
                    transport: None,
                },
                StatusCode::NOT_FOUND,
                "runner_not_found",
            ),
            (
                RunnerControlError {
                    status: Some(StatusCode::CONFLICT),
                    code: Some("runner_wrong_state".to_string()),
                    transport: None,
                },
                StatusCode::CONFLICT,
                "runner_wrong_state",
            ),
            (
                RunnerControlError {
                    status: Some(StatusCode::GONE),
                    code: Some("token_already_retrieved".to_string()),
                    transport: None,
                },
                StatusCode::CONFLICT,
                "runner_token_unavailable",
            ),
            (
                RunnerControlError {
                    status: Some(StatusCode::UNAUTHORIZED),
                    code: Some("unauthorized".to_string()),
                    transport: None,
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "runner_service_unauthorized",
            ),
            (
                RunnerControlError {
                    status: Some(StatusCode::SERVICE_UNAVAILABLE),
                    code: Some("docker_unavailable".to_string()),
                    transport: None,
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "runner_service_unavailable",
            ),
            (
                RunnerControlError {
                    status: None,
                    code: None,
                    transport: Some("request_failed"),
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "runner_service_unavailable",
            ),
        ];
        for (error, status, code) in cases {
            let mapped = map_control_error(error.clone());
            let AppError::Api {
                status: mapped_status,
                code: mapped_code,
                ..
            } = &mapped
            else {
                panic!("{error:?} must map to a coded Api error, got {mapped:?}");
            };
            assert_eq!(*mapped_status, status, "status for {error:?}");
            assert_eq!(*mapped_code, code, "code for {error:?}");
        }
    }

    #[test]
    fn a_disabled_deployment_answers_one_named_503() {
        let error = runner_service_disabled();
        let AppError::Api { status, code, .. } = &error else {
            panic!("expected a coded error");
        };
        assert_eq!(*status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(*code, "runner_service_disabled");
    }

    #[test]
    fn no_terminal_state_can_be_written_over_by_a_refresh() {
        for state in NON_TERMINAL_STATES {
            assert!(
                !state.is_terminal(),
                "{state:?} is on the refresh allow-list and must not be terminal"
            );
        }
        assert_eq!(NON_TERMINAL_STATES.len(), 4);
    }
}
