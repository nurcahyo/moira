use std::{pin::Pin, time::Duration};

use async_stream::stream;
use chrono::Utc;
use futures_util::Stream;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{
        ConversationExecutionLink, ConversationService, ExecutionService, RequestContext,
        admin::{PageRequest, shared::paginate},
    },
    config::ImageUrlSettings,
    domain::{
        ApplicationExecutionPolicyPutRequest, ApplicationExecutionPolicyRecord, AuditLogInsert,
        AuditResult, CallerRuntimeIdentity, CursorScope, DomainMessage, DomainMessageContent,
        DomainMessageRole, ExecutionCommand, ExecutionFailure, ExecutionFailureClass,
        ExecutionOptions, ExecutionOutcome, ExecutionQuery, ExecutionStatus, IdempotencyRecord,
        Keyed, ListResponse, OpenAiCompatTextFormat, OpenAiCompatTextOptions,
        OpenAiResponseCompatRequest, PublicCapabilities, PublicCitation, PublicContentPart,
        PublicConversationRef, PublicExecutionSummary, PublicInputMessage, PublicListQuery,
        PublicMessageRole, PublicModelRef, PublicModelResource, PublicOutputContentPart,
        PublicOutputItem, PublicResponse, PublicResponseFormat, PublicResponseRecord,
        PublicResponseRequest, PublicResponseStatus, PublicRouteRef, PublicRouteResource,
        PublicSseEnvelope, PublicUsageRecord, PublicUsageSummary, RuntimeEventEnvelope,
        RuntimeEventType, UsageQuery,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, IdempotencyClaim, PgAdminRepository, PgPublicRepository, PublicAccess,
        PublicRepository, ResponseStartedInsert, ResponseTerminalUpdate,
        default_application_execution_policy, idempotency_record,
    },
    security::{
        Actor, ActorType, HostResolver, IdempotencyHasher, OutboundDenialReason, OutboundUrlDenial,
        OutboundUrlPolicy, SystemResolver, secret_fingerprint, validate_outbound_url,
    },
};

/// Cursor scopes for the four public lists (issue #93).
///
/// Same contract as the admin scopes in `crate::application::admin::shared`: the label is
/// mixed into the cursor's integrity tag but never stored inside it, so a cursor minted by
/// one list fails closed with `400 invalid_cursor` on another instead of paging through an
/// unrelated key space. `/v1/executions` and `/v1/usage` are the pair that most needs it —
/// both are `(timestamp, uuid)` over rows keyed by the same execution, so nothing in the
/// payload distinguishes them.
const EXECUTIONS_CURSOR: CursorScope = CursorScope::new("public.executions");
const USAGE_CURSOR: CursorScope = CursorScope::new("public.usage");
const MODELS_CURSOR: CursorScope = CursorScope::new("public.models");
const ROUTES_CURSOR: CursorScope = CursorScope::new("public.routes");

/// The public list queries reach the shared [`PageRequest`] the same way `PageQuery` does:
/// by a `From` that carries **both** the limit and the cursor.
///
/// That is the whole point of the conversion existing at all. `PageRequest` deliberately has
/// no constructor taking a bare limit, because the nine admin lists once shipped exactly that
/// — they compiled, they paginated, and they silently dropped every cursor a caller sent. The
/// four public lists shipped the same defect for longer: they advertised a `cursor` parameter
/// and bound it to no SQL whatsoever. Going through `PageRequest` means a handler cannot get
/// a page size here without having also handed over the cursor.
impl From<&ExecutionQuery> for PageRequest {
    fn from(query: &ExecutionQuery) -> Self {
        Self::from_limit_and_cursor(query.limit(), query.cursor.clone())
    }
}

impl From<&UsageQuery> for PageRequest {
    fn from(query: &UsageQuery) -> Self {
        Self::from_limit_and_cursor(query.limit(), query.cursor.clone())
    }
}

impl From<&PublicListQuery> for PageRequest {
    fn from(query: &PublicListQuery) -> Self {
        Self::from_limit_and_cursor(query.limit(), query.cursor.clone())
    }
}

/// Assembles a public page from the repository's over-fetched, key-tagged rows.
///
/// This is `crate::application::admin::shared::paginate` with the [`Keyed`] wrapper peeled
/// off afterwards — the trimming, the `has_more` arithmetic and the "encode the last row
/// actually returned, never the over-fetched one" rule are that function's, not a second
/// copy of them. The only thing done here is dropping the sort keys, which exist so the
/// cursor can be minted and must not reach the wire.
fn paginate_public<T>(
    rows: Vec<Keyed<T>>,
    page: &PageRequest,
    scope: CursorScope,
) -> ListResponse<T> {
    let keyed = paginate(rows, page, scope, |row| row.key);
    let mut response = ListResponse::new(keyed.data.into_iter().map(|row| row.row).collect());
    response.pagination = keyed.pagination;
    response
}

#[derive(Clone)]
pub struct PublicExecutionService {
    state: AppState,
    public_repo: PgPublicRepository,
    admin_repo: PgAdminRepository,
}

#[derive(Debug, Clone)]
pub struct ExecutionPipeline {
    interceptors: Vec<&'static str>,
}

impl ExecutionPipeline {
    pub fn phase_four() -> Self {
        Self {
            interceptors: vec![
                "RequestNormalizationInterceptor",
                "IdentityBindingInterceptor",
                "ExecutionAuthorizationInterceptor",
                "InputValidationInterceptor",
                "ApplicationPolicyInterceptor",
                "RateLimitInterceptor",
                "IdempotencyInterceptor",
                "ContextBudgetInterceptor",
                "ExecutionDispatchInterceptor",
                "UsageFinalizationInterceptor",
                "AuditInterceptor",
            ],
        }
    }

    pub fn names(&self) -> &[&'static str] {
        &self.interceptors
    }
}

#[derive(Debug, Clone)]
struct PreparedExecution {
    response_id: Uuid,
    command: ExecutionCommand,
    policy: ApplicationExecutionPolicyRecord,
    metadata: Value,
}

#[derive(Debug, Clone)]
struct IdempotencyState {
    key_hash: String,
    actor_fingerprint: String,
    operation: &'static str,
}

impl PublicExecutionService {
    pub fn new(state: &AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state: state.clone(),
            public_repo: PgPublicRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool),
        })
    }

    pub fn pipeline(&self) -> ExecutionPipeline {
        ExecutionPipeline::phase_four()
    }

    pub async fn create_response(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: PublicResponseRequest,
    ) -> Result<PublicResponse, AppError> {
        self.state.authz.require(actor, "moira:responses:create")?;
        let access = public_access(actor, false)?;
        let application_id = access.application_id;
        let policy = self.execution_policy(application_id).await?;
        if !policy.responses_enabled {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "responses_disabled",
                "responses are disabled for this application",
            ));
        }
        self.check_rate_limit(actor, application_id, policy.rate_limit_requests_per_minute)
            .await?;

        let idempotency_request = request.clone();
        let mut prepared = self
            .prepare_execution(actor, ctx, request, policy.clone(), application_id, false)
            .await?;
        let idempotency = self
            .claim_idempotency(ctx, actor, application_id, &idempotency_request)
            .await?;
        if let Some(state) = &idempotency
            && let Some(replay) = self
                .replay_idempotency(state, actor, &idempotency_request)
                .await?
        {
            return Ok(replay);
        }
        let conversation_service = ConversationService::new(&self.state)?;
        let conversation_link = conversation_service
            .prepare_response_conversation(
                actor,
                ctx,
                prepared.command.execution_id,
                prepared.command.route_hint.clone(),
                idempotency_request.conversation.as_ref(),
                &idempotency_request.input,
            )
            .await?;
        apply_planned_context(&mut prepared, conversation_link.as_ref());
        let response = self
            .public_repo
            .insert_response_started(&ResponseStartedInsert {
                id: prepared.response_id,
                execution_id: prepared.command.execution_id,
                request_id: prepared.command.request_id.clone(),
                application_id: prepared.command.application_id,
                external_tenant_id: prepared.command.external_tenant_id.clone(),
                external_user_id: prepared.command.external_user_id.clone(),
                conversation_public_id: conversation_link
                    .as_ref()
                    .map(|link| link.conversation_id.clone()),
                metadata: prepared.metadata.clone(),
                expires_at: retention_expires_at(&prepared.policy),
            })
            .await?;
        self.audit(
            actor,
            ctx,
            "response.started",
            AuditResult::Success,
            Some(response.id.to_string()),
            json!({ "execution_id": response.execution_id }),
        )
        .await?;

        let outcome = crate::application::MoiraExecutionService::new(self.state.clone())?
            .execute_with_events(prepared.command)
            .await?
            .0;

        let mapped = match outcome.status {
            ExecutionStatus::Succeeded => {
                let update = terminal_update_from_outcome(
                    &self.state.idempotency_hasher,
                    &outcome,
                    &prepared.policy,
                    None,
                );
                let record = self
                    .public_repo
                    .complete_response(response.id, &update)
                    .await?;
                self.record_conversation_assistant(
                    actor,
                    ctx,
                    conversation_link.as_ref(),
                    record.id,
                    record.execution_id,
                    outcome.output_text.as_deref(),
                )
                .await?;
                let public = public_response_from_record(
                    &record,
                    outcome.output_text.clone(),
                    citations_from_link(conversation_link.as_ref()),
                );
                self.audit(
                    actor,
                    ctx,
                    "response.completed",
                    AuditResult::Success,
                    Some(record.id.to_string()),
                    json!({
                        "execution_id": record.execution_id,
                        "route_id": record.route_id,
                        "provider_model_id": record.provider_model_id,
                        "usage": record.usage
                    }),
                )
                .await?;
                if let Some(state) = &idempotency {
                    self.finish_idempotency(state, 200, &public, Some(public.id.as_str()))
                        .await?;
                }
                Ok(public)
            }
            ExecutionStatus::Failed | ExecutionStatus::Cancelled => {
                let failure = outcome.failure.clone().unwrap_or_else(|| {
                    ExecutionFailure::new(
                        ExecutionFailureClass::InternalError,
                        "execution failed without a failure class",
                    )
                });
                let update = terminal_update_from_outcome(
                    &self.state.idempotency_hasher,
                    &outcome,
                    &prepared.policy,
                    Some(&failure),
                );
                let record = self.public_repo.fail_response(response.id, &update).await?;
                let status = failure_http_status(failure.class);
                let body = json!({
                    "error": {
                        "code": failure_code(failure.class),
                        "message": failure.message,
                        "request_id": ctx.request_id
                    }
                });
                self.audit(
                    actor,
                    ctx,
                    "response.failed",
                    AuditResult::Failed,
                    Some(record.id.to_string()),
                    json!({ "execution_id": record.execution_id, "failure_class": failure.class }),
                )
                .await?;
                if let Some(state) = &idempotency {
                    self.public_repo
                        .finish_idempotency(
                            &state.key_hash,
                            &state.actor_fingerprint,
                            state.operation,
                            status.as_u16() as i32,
                            &body,
                            Some(&format!("resp_{}", record.id)),
                        )
                        .await?;
                }
                Err(AppError::coded(
                    status,
                    failure_code(failure.class),
                    failure.message,
                ))
            }
        }?;

        Ok(mapped)
    }

    pub async fn stream_response(
        &self,
        actor: Actor,
        ctx: RequestContext,
        request: PublicResponseRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<PublicSseEnvelope, AppError>> + Send>>, AppError>
    {
        if ctx.idempotency_key.is_some() {
            return Err(AppError::unprocessable(
                "idempotency_not_supported_for_stream",
                "Idempotency-Key is not supported for response streams",
            ));
        }
        self.state.authz.require(&actor, "moira:responses:stream")?;
        let application_id = public_access(&actor, false)?.application_id;
        let policy = self.execution_policy(application_id).await?;
        if !policy.streaming_enabled {
            return Err(AppError::coded(
                axum::http::StatusCode::FORBIDDEN,
                "streaming_not_supported",
                "streaming is disabled for this application",
            ));
        }
        self.check_rate_limit(&actor, application_id, policy.rate_limit_streams_per_minute)
            .await?;
        let conversation_request = request.clone();
        let mut prepared = self
            .prepare_execution(&actor, &ctx, request, policy.clone(), application_id, true)
            .await?;
        let conversation_service = ConversationService::new(&self.state)?;
        let conversation_link = conversation_service
            .prepare_response_conversation(
                &actor,
                &ctx,
                prepared.command.execution_id,
                prepared.command.route_hint.clone(),
                conversation_request.conversation.as_ref(),
                &conversation_request.input,
            )
            .await?;
        apply_planned_context(&mut prepared, conversation_link.as_ref());
        let response = self
            .public_repo
            .insert_response_started(&ResponseStartedInsert {
                id: prepared.response_id,
                execution_id: prepared.command.execution_id,
                request_id: prepared.command.request_id.clone(),
                application_id: prepared.command.application_id,
                external_tenant_id: prepared.command.external_tenant_id.clone(),
                external_user_id: prepared.command.external_user_id.clone(),
                conversation_public_id: conversation_link
                    .as_ref()
                    .map(|link| link.conversation_id.clone()),
                metadata: prepared.metadata.clone(),
                expires_at: retention_expires_at(&prepared.policy),
            })
            .await?;
        self.audit(
            &actor,
            &ctx,
            "response.stream.started",
            AuditResult::Success,
            Some(response.id.to_string()),
            json!({ "execution_id": response.execution_id }),
        )
        .await?;

        let execution = crate::application::MoiraExecutionService::new(self.state.clone())?;
        let handle = match execution.execute_stream(prepared.command).await {
            Ok(handle) => handle,
            Err(error) => {
                let failure = ExecutionFailure::new(
                    ExecutionFailureClass::InternalError,
                    "stream execution failed to start",
                );
                let update = ResponseTerminalUpdate {
                    route_id: None,
                    provider_id: None,
                    provider_model_id: None,
                    output_summary: json!({ "persistence_mode": policy.persistence_mode }),
                    usage: PublicUsageSummary::default(),
                    failure_class: Some(failure_code(failure.class).to_string()),
                    failure_message: Some(error.to_string()),
                    output_persisted: false,
                };
                let _ = self.public_repo.fail_response(response.id, &update).await;
                return Err(error);
            }
        };

        let capacity = self
            .state
            .settings
            .runtime
            .internal_stream_queue_capacity
            .max(1);
        let send_timeout =
            Duration::from_secs(self.state.settings.public_api.heartbeat_seconds.max(1));
        let (public_tx, mut public_rx) = mpsc::channel(capacity);
        let service = self.clone();
        tokio::spawn(async move {
            service
                .supervise_public_stream(
                    actor,
                    ctx,
                    policy,
                    conversation_link,
                    response,
                    handle,
                    public_tx,
                    send_timeout,
                )
                .await;
        });

        Ok(Box::pin(stream! {
            while let Some(envelope) = public_rx.recv().await {
                yield Ok(envelope);
            }
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn supervise_public_stream(
        &self,
        actor: Actor,
        ctx: RequestContext,
        policy: ApplicationExecutionPolicyRecord,
        conversation_link: Option<ConversationExecutionLink>,
        response: PublicResponseRecord,
        mut handle: crate::domain::ExecutionStreamHandle,
        public_tx: mpsc::Sender<PublicSseEnvelope>,
        send_timeout: Duration,
    ) {
        let mut sequence = 1u64;
        let mut disconnected = false;

        for (event_type, payload) in [
            ("response.created", json!({ "status": "in_progress" })),
            ("response.in_progress", json!({})),
        ] {
            if send_public_event(
                &public_tx,
                public_sse(
                    response.id,
                    response.execution_id,
                    response.request_id.clone(),
                    sequence,
                    event_type,
                    payload,
                ),
                send_timeout,
            )
            .await
            {
                sequence += 1;
            } else {
                disconnected = true;
                handle.cancel();
                break;
            }
        }

        while !disconnected {
            tokio::select! {
                _ = public_tx.closed() => {
                    disconnected = true;
                    handle.cancel();
                }
                event = handle.events.recv() => {
                    let event = match event {
                        Some(Ok(event)) => event,
                        Some(Err(_)) => continue,
                        None => break,
                    };
                    let Some(mapped) = map_runtime_event(response.id, &event, sequence) else {
                        continue;
                    };
                    if send_public_event(&public_tx, mapped, send_timeout).await {
                        sequence += 1;
                    } else {
                        disconnected = true;
                        handle.cancel();
                    }
                }
            }
        }

        if disconnected {
            while handle.events.recv().await.is_some() {}
        }

        let outcome = (&mut handle.outcome).await;
        if !disconnected && public_tx.is_closed() {
            disconnected = true;
            handle.cancel();
        }
        if disconnected {
            let update = cancellation_update(
                &self.state.idempotency_hasher,
                outcome.as_ref().ok().and_then(|value| value.as_ref().ok()),
                &policy,
            );
            if self
                .public_repo
                .cancel_response(response.id, &update)
                .await
                .is_err()
            {
                let _ = self
                    .audit(
                        &actor,
                        &ctx,
                        "response.stream.cancellation_persistence_failed",
                        AuditResult::Failed,
                        Some(response.id.to_string()),
                        json!({ "execution_id": response.execution_id }),
                    )
                    .await;
                return;
            }
            let _ = self
                .audit(
                    &actor,
                    &ctx,
                    "response.stream.cancelled",
                    AuditResult::Failed,
                    Some(response.id.to_string()),
                    json!({ "execution_id": response.execution_id }),
                )
                .await;
            return;
        }

        let terminal = match outcome {
            Ok(Ok(outcome)) => match outcome.status {
                ExecutionStatus::Succeeded => {
                    let update = terminal_update_from_outcome(
                        &self.state.idempotency_hasher,
                        &outcome,
                        &policy,
                        None,
                    );
                    match self
                        .public_repo
                        .complete_response(response.id, &update)
                        .await
                    {
                        Ok(_) => {
                            let conversation_result = self
                                .record_conversation_assistant(
                                    &actor,
                                    &ctx,
                                    conversation_link.as_ref(),
                                    response.id,
                                    response.execution_id,
                                    outcome.output_text.as_deref(),
                                )
                                .await;
                            if conversation_result.is_err() {
                                let _ = self
                                    .audit(
                                        &actor,
                                        &ctx,
                                        "response.stream.conversation_persistence_failed",
                                        AuditResult::Failed,
                                        Some(response.id.to_string()),
                                        json!({ "execution_id": response.execution_id }),
                                    )
                                    .await;
                            } else {
                                let _ = self
                                    .audit(
                                        &actor,
                                        &ctx,
                                        "response.stream.completed",
                                        AuditResult::Success,
                                        Some(response.id.to_string()),
                                        json!({ "execution_id": response.execution_id }),
                                    )
                                    .await;
                            }
                            public_sse(
                                response.id,
                                response.execution_id,
                                response.request_id.clone(),
                                sequence,
                                "response.completed",
                                json!({
                                    "status": "completed",
                                    "usage": PublicUsageSummary::from(outcome.usage)
                                }),
                            )
                        }
                        Err(error) => {
                            let failure = ExecutionFailure::new(
                                ExecutionFailureClass::InternalError,
                                error.to_string(),
                            );
                            let failure_update = failure_update(&failure, &policy);
                            if self
                                .public_repo
                                .fail_response(response.id, &failure_update)
                                .await
                                .is_err()
                            {
                                return;
                            }
                            terminal_failure(&response, sequence, &ctx, failure)
                        }
                    }
                }
                ExecutionStatus::Failed => {
                    let failure = outcome.failure.clone().unwrap_or_else(|| {
                        ExecutionFailure::new(
                            ExecutionFailureClass::InternalError,
                            "execution failed",
                        )
                    });
                    let update = terminal_update_from_outcome(
                        &self.state.idempotency_hasher,
                        &outcome,
                        &policy,
                        Some(&failure),
                    );
                    if self
                        .public_repo
                        .fail_response(response.id, &update)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = self
                        .audit(
                            &actor,
                            &ctx,
                            "response.stream.failed",
                            AuditResult::Failed,
                            Some(response.id.to_string()),
                            json!({
                                "execution_id": response.execution_id,
                                "failure_class": failure.class
                            }),
                        )
                        .await;
                    terminal_failure(&response, sequence, &ctx, failure)
                }
                ExecutionStatus::Cancelled => {
                    let update = cancellation_update(
                        &self.state.idempotency_hasher,
                        Some(&outcome),
                        &policy,
                    );
                    if self
                        .public_repo
                        .cancel_response(response.id, &update)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = self
                        .audit(
                            &actor,
                            &ctx,
                            "response.stream.cancelled",
                            AuditResult::Failed,
                            Some(response.id.to_string()),
                            json!({ "execution_id": response.execution_id }),
                        )
                        .await;
                    terminal_cancelled(&response, sequence, &ctx)
                }
            },
            Ok(Err(failure)) => {
                let update = failure_update(&failure, &policy);
                if failure.class == ExecutionFailureClass::RequestCancelled {
                    if self
                        .public_repo
                        .cancel_response(response.id, &update)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    terminal_cancelled(&response, sequence, &ctx)
                } else {
                    if self
                        .public_repo
                        .fail_response(response.id, &update)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    terminal_failure(&response, sequence, &ctx, failure)
                }
            }
            Err(error) => {
                let failure = ExecutionFailure::new(
                    ExecutionFailureClass::InternalError,
                    format!("stream outcome unavailable: {error}"),
                );
                let update = failure_update(&failure, &policy);
                if self
                    .public_repo
                    .fail_response(response.id, &update)
                    .await
                    .is_err()
                {
                    return;
                }
                terminal_failure(&response, sequence, &ctx, failure)
            }
        };

        let _ = send_public_event(&public_tx, terminal, send_timeout).await;
    }

    pub async fn get_response(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        response_id: Uuid,
    ) -> Result<PublicResponse, AppError> {
        self.state.authz.require(actor, "moira:responses:read")?;
        let access = public_access(
            actor,
            can_read_all(actor, "moira:responses:read", &self.state),
        )?;
        let record = self
            .public_repo
            .find_response_authorized(response_id, &access)
            .await?;
        self.audit(
            actor,
            ctx,
            "response.read",
            AuditResult::Success,
            Some(record.id.to_string()),
            json!({}),
        )
        .await?;
        // A later `GET /responses/{id}` carries no citations, and that is honest rather than a
        // gap: provenance lives on the `context_plans` row keyed by execution, and resolving it
        // back into `PublicCitation`s here would be a second, unauthorised read path over
        // another request's plan. The diagnostic surface for that is Sub-Phase C's admin
        // endpoint, deliberately deferred out of this wave.
        Ok(public_response_from_record(&record, None, Vec::new()))
    }

    pub async fn get_execution(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        execution_id: Uuid,
    ) -> Result<PublicExecutionSummary, AppError> {
        self.state.authz.require(actor, "moira:executions:read")?;
        let access = public_access(
            actor,
            can_read_all(actor, "moira:executions:read", &self.state),
        )?;
        let record = self
            .public_repo
            .find_execution_authorized(execution_id, &access)
            .await?;
        self.audit(
            actor,
            ctx,
            "execution.read",
            AuditResult::Success,
            Some(execution_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn list_executions(
        &self,
        actor: &Actor,
        query: &ExecutionQuery,
    ) -> Result<ListResponse<PublicExecutionSummary>, AppError> {
        self.state.authz.require(actor, "moira:executions:read")?;
        let access = public_access(
            actor,
            can_read_all(actor, "moira:executions:read", &self.state),
        )?;
        // Decoded after the authorization check and before the query, exactly as the admin
        // lists do it: an unauthorized caller learns nothing about cursor validity, and a
        // bad cursor never reaches Postgres.
        let page = PageRequest::from(query);
        let cursor = page.decode(EXECUTIONS_CURSOR)?;
        let rows = self
            .public_repo
            .list_executions_authorized(&access, cursor, page.limit())
            .await?;
        Ok(paginate_public(rows, &page, EXECUTIONS_CURSOR))
    }

    pub async fn list_usage(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        query: &UsageQuery,
    ) -> Result<ListResponse<PublicUsageRecord>, AppError> {
        self.state.authz.require(actor, "moira:usage:read")?;
        let access = public_access(actor, can_read_all(actor, "moira:usage:read", &self.state))?;
        let page = PageRequest::from(query);
        let cursor = page.decode(USAGE_CURSOR)?;
        let rows = self
            .public_repo
            .list_usage_authorized(&access, query, cursor)
            .await?;
        let response = paginate_public(rows, &page, USAGE_CURSOR);
        // Counts the rows the caller actually receives, not the over-fetched probe row.
        self.audit(
            actor,
            ctx,
            "usage.read",
            AuditResult::Success,
            None,
            json!({ "count": response.data.len() }),
        )
        .await?;
        Ok(response)
    }

    pub async fn list_models(
        &self,
        actor: &Actor,
        query: &PublicListQuery,
    ) -> Result<ListResponse<PublicModelResource>, AppError> {
        self.state.authz.require(actor, "moira:models:read")?;
        let access = public_access(actor, false)?;
        let page = PageRequest::from(query);
        let cursor = page.decode(MODELS_CURSOR)?;
        let rows = self
            .public_repo
            .list_visible_models(&access, cursor, page.limit())
            .await?;
        Ok(paginate_public(rows, &page, MODELS_CURSOR))
    }

    pub async fn list_routes(
        &self,
        actor: &Actor,
        query: &PublicListQuery,
    ) -> Result<ListResponse<PublicRouteResource>, AppError> {
        self.state.authz.require(actor, "moira:routes:read")?;
        let access = public_access(actor, false)?;
        let page = PageRequest::from(query);
        let cursor = page.decode(ROUTES_CURSOR)?;
        let rows = self
            .public_repo
            .list_visible_routes(&access, cursor, page.limit())
            .await?;
        Ok(paginate_public(rows, &page, ROUTES_CURSOR))
    }

    pub async fn capabilities(&self, actor: &Actor) -> Result<PublicCapabilities, AppError> {
        self.state.authz.require(actor, "moira:capabilities:read")?;
        let policy = self
            .execution_policy(public_access(actor, false)?.application_id)
            .await?;
        Ok(PublicCapabilities {
            streaming: policy.streaming_enabled,
            vision: policy.vision_enabled,
            tools: policy.tools_enabled,
            structured_output: policy.structured_output_enabled,
            reasoning: false,
            response_persistence: policy.persistence_mode,
            max_input_items: policy.maximum_input_items,
            max_request_bytes: policy.maximum_request_bytes,
            max_output_tokens: policy.maximum_output_tokens,
        })
    }

    pub async fn get_application_execution_policy(
        &self,
        actor: &Actor,
        application_id: Uuid,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:execution-policies:read")?;
        self.public_repo
            .get_or_create_application_execution_policy(application_id)
            .await
    }

    pub async fn put_application_execution_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        application_id: Uuid,
        expected_version: Option<i64>,
        request: ApplicationExecutionPolicyPutRequest,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:execution-policies:write")?;
        validate_policy_request(&request)?;
        // The `If-Match` comparison is deliberately *not* done here. Reading the current
        // version on one pooled connection and writing on another is check-then-act: two
        // writers holding the same currently-valid version both passed and both wrote, losing
        // one update with no conflict reported to either caller. The repository now performs
        // the comparison and the write in one transaction, under a row lock, with the version
        // predicate in the `update` itself, and raises the same
        // `409 resource_version_conflict` this function used to raise.
        let record = self
            .public_repo
            .put_application_execution_policy(application_id, expected_version, &request)
            .await?;
        self.state.runtime_cache.invalidate_all().await;
        self.audit(
            actor,
            ctx,
            "application_execution_policy.upsert",
            AuditResult::Success,
            Some(application_id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub fn openai_compat_to_public(
        &self,
        request: OpenAiResponseCompatRequest,
    ) -> Result<PublicResponseRequest, AppError> {
        openai_compat_to_public_request(request)
    }

    async fn prepare_execution(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        mut request: PublicResponseRequest,
        policy: ApplicationExecutionPolicyRecord,
        application_id: Option<Uuid>,
        stream: bool,
    ) -> Result<PreparedExecution, AppError> {
        validate_request(&self.state, actor, &request, &policy, stream).await?;
        let access = public_access(actor, false)?;
        let model_hint = match request.model.take() {
            Some(model) => Some(resolve_model_hint(&self.public_repo, &access, &model).await?),
            None => None,
        };
        let messages = map_public_messages(&request, &policy)?;
        let mut required_capabilities = Vec::new();
        if request_has_image(&request) {
            required_capabilities.push("vision".to_string());
        }
        let output_schema = match &request.response_format {
            PublicResponseFormat::Text => None,
            // F46 — this arm is where the empty-object schema was born, so the refusal is
            // repeated here rather than left to `validate_request` alone. `validate_request`
            // gives the caller the coded 422; this makes the wrong schema unconstructible by
            // any future entry point that reaches `prepare_execution` without it. The two are
            // deliberately redundant: neither edit alone reintroduces the defect.
            // The trailing `None` is not dead code with a purpose of its own: it is the
            // fallback if someone ever weakens `refuse_json_object`. An unconstrained request
            // is a degradation; `Some(json!({"type":"object"}))` was a wrong answer.
            PublicResponseFormat::JsonObject => {
                refuse_json_object(&request.response_format)?;
                None
            }
            // F45 — the second layer, deliberately redundant with `validate_request`, exactly as
            // the `json_object` arm above is. This is the site that turns a caller's
            // `response_format` into the `output_schema` rig will encode under a hardcoded
            // `strict: true`, so it is the last point at which "the caller said false" is still
            // knowable. Neither edit alone reintroduces the defect.
            PublicResponseFormat::JsonSchema { schema, .. } => {
                refuse_non_strict_json_schema(&request.response_format)?;
                required_capabilities.push("structured_output".to_string());
                Some(schema.clone())
            }
        };
        let timeout_ms = request.timeout_ms.map(|value| {
            value.min(policy.maximum_timeout_ms.max(1) as u64).min(
                self.state
                    .settings
                    .runtime
                    .maximum_execution_timeout_seconds
                    * 1000,
            )
        });
        Ok(PreparedExecution {
            response_id: Uuid::now_v7(),
            command: ExecutionCommand {
                request_id: ctx.request_id.clone(),
                execution_id: Uuid::now_v7(),
                identity: caller_runtime_identity(actor),
                application_id,
                external_tenant_id: actor.external_tenant_id.clone().or(actor.tenant_id.clone()),
                external_user_id: actor.external_user_id.clone().or(actor.subject.clone()),
                messages,
                route_hint: request.route,
                provider_hint: request.provider,
                model_hint,
                credential_hint: request.credential_id,
                options: ExecutionOptions {
                    temperature: request.temperature,
                    max_tokens: request.max_output_tokens,
                    timeout_ms,
                    stream,
                    required_capabilities,
                    allow_fallback: true,
                    max_fallbacks: None,
                    max_retries: None,
                    output_schema,
                },
                metadata: request.metadata.clone(),
            },
            policy,
            metadata: request.metadata,
        })
    }

    async fn execution_policy(
        &self,
        application_id: Option<Uuid>,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        match application_id {
            Some(id) => {
                self.public_repo
                    .get_or_create_application_execution_policy(id)
                    .await
            }
            None => Ok(default_application_execution_policy(Uuid::nil())),
        }
    }

    async fn check_rate_limit(
        &self,
        actor: &Actor,
        application_id: Option<Uuid>,
        limit: i32,
    ) -> Result<(), AppError> {
        let principal = actor
            .api_key_id
            .or(actor.trusted_jwt_issuer_id)
            .map(|id| id.to_string())
            .or_else(|| actor.subject.clone())
            .unwrap_or_else(|| "anonymous".to_string());
        self.state
            .public_rate_limiter
            .check(
                format!(
                    "public:{}:{}",
                    application_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "global".to_string()),
                    principal
                ),
                limit.max(1) as u32,
                Duration::from_secs(60),
            )
            .await
    }

    async fn claim_idempotency(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        application_id: Option<Uuid>,
        request: &PublicResponseRequest,
    ) -> Result<Option<IdempotencyState>, AppError> {
        let Some(key) = ctx.idempotency_key.as_deref() else {
            return Ok(None);
        };
        let hasher = &self.state.idempotency_hasher;
        let actor_fingerprint = crate::application::admin::actor_fingerprint(hasher, actor);
        let request_bytes = normalized_request_bytes(&(application_id, request))?;

        // Pre-claim sweep, deliberately *before* the claim insert, over every historical
        // spelling of this row's position in the unique index
        // (idempotency_key_hash, actor_fingerprint, operation). Two of its three columns
        // have been redefined under deployed rows:
        //
        //   * `idempotency_key_hash` — unkeyed SHA-256 → keyed HMAC (plan 03, P1-1). The
        //     current hash needs no pre-claim probe: the claim's `on conflict` finds it.
        //   * `actor_fingerprint` — this module's 4-field formula → the crate-wide 10-field
        //     one (plan 06, Module 16 / P2-15), and then that 10-field digest from unkeyed
        //     SHA-256 → keyed HMAC when the fingerprint was peppered (issue #95). Both key
        //     hashes must be probed under each legacy value, because a pre-deploy row may
        //     carry either. A *peppered* 4-field digest is deliberately absent: the narrow
        //     formula was retired before the pepper existed, so nothing ever wrote one.
        //
        // Skipping the sweep is not a slow replay, it is a *wrong* one: an unswept row sits
        // at a different point of the unique index, so the claim below would insert
        // alongside it rather than conflict with it, and `/v1/responses` would execute a
        // second time against the provider — a contractual replay turned into a duplicate
        // billable call.
        //
        // Cost: up to five indexed lookups per idempotent request, on the miss path only.
        // The write below always uses the current pair, so no legacy value can ever re-enter
        // the ledger and the sweep drains as rows expire.
        //
        // TODO(post-deploy): drop the two `legacy_actor_fingerprint` probes once every ledger
        // row written before plan 06 shipped has expired, and the two
        // `unkeyed_actor_fingerprint` probes once every row written before the pepper deploy
        // has expired. `idempotency_record` sets `expires_at` 24h ahead, so each window closes
        // 24h after the deploy that carries it. The plan-03 `legacy_hash` probe has its own,
        // earlier window.
        //
        // Gated on a DEPLOY, not on a merge, and deliberately not owned by any plan — see the
        // matching note in `runtime_admin.rs`.
        let unkeyed_actor_fingerprint = crate::application::admin::unkeyed_actor_fingerprint(actor);
        let legacy_actor_fingerprint = legacy_public_actor_fingerprint(actor, application_id);
        let legacy_key_hash = hasher.legacy_hash(key.as_bytes());
        let current_key_hash = hasher.hash(key.as_bytes());
        let candidates = [
            (&legacy_key_hash, &actor_fingerprint),
            (&current_key_hash, &unkeyed_actor_fingerprint),
            (&legacy_key_hash, &unkeyed_actor_fingerprint),
            (&current_key_hash, &legacy_actor_fingerprint),
            (&legacy_key_hash, &legacy_actor_fingerprint),
        ];
        for (candidate_key_hash, candidate_fingerprint) in candidates {
            if let Some(existing) = self
                .public_repo
                .get_idempotency_record(
                    candidate_key_hash,
                    candidate_fingerprint,
                    "response.create",
                )
                .await?
            {
                return replayed_idempotency_state(hasher, &request_bytes, existing).map(Some);
            }
        }

        let record = idempotency_record(
            hasher,
            key,
            actor_fingerprint.clone(),
            "response.create",
            hasher.hash(&request_bytes),
        );
        match self.public_repo.claim_idempotency(&record).await? {
            IdempotencyClaim::Claimed => Ok(Some(IdempotencyState {
                key_hash: record.idempotency_key_hash,
                actor_fingerprint,
                operation: "response.create",
            })),
            IdempotencyClaim::Replay(existing) => {
                replayed_idempotency_state(hasher, &request_bytes, existing).map(Some)
            }
        }
    }

    async fn replay_idempotency(
        &self,
        state: &IdempotencyState,
        _actor: &Actor,
        _request: &PublicResponseRequest,
    ) -> Result<Option<PublicResponse>, AppError> {
        let Some(record) = self
            .public_repo
            .get_idempotency_record(&state.key_hash, &state.actor_fingerprint, state.operation)
            .await?
        else {
            return Ok(None);
        };
        let Some(body) = record.response_body else {
            return Ok(None);
        };
        if record.response_status.unwrap_or(200) >= 400 {
            return Err(AppError::coded(
                axum::http::StatusCode::BAD_GATEWAY,
                "execution_failed",
                body.pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("idempotent execution failed"),
            ));
        }
        serde_json::from_value(body)
            .map(Some)
            .map_err(|err| AppError::Internal(format!("decode idempotent response: {err}")))
    }

    async fn finish_idempotency<T: Serialize>(
        &self,
        state: &IdempotencyState,
        status: i32,
        response: &T,
        resource_id: Option<&str>,
    ) -> Result<(), AppError> {
        let body = serde_json::to_value(response)
            .map_err(|err| AppError::Internal(format!("encode idempotency response: {err}")))?;
        self.public_repo
            .finish_idempotency(
                &state.key_hash,
                &state.actor_fingerprint,
                state.operation,
                status,
                &body,
                resource_id,
            )
            .await
    }

    async fn record_conversation_assistant(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        link: Option<&ConversationExecutionLink>,
        response_id: Uuid,
        execution_id: Uuid,
        output_text: Option<&str>,
    ) -> Result<(), AppError> {
        let Some(link) = link else {
            return Ok(());
        };
        ConversationService::new(&self.state)?
            .record_assistant_response(actor, ctx, link, response_id, execution_id, output_text)
            .await?;
        Ok(())
    }

    async fn audit(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        result: AuditResult,
        resource_id: Option<String>,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(AuditLogInsert {
                request_id: Some(ctx.request_id.clone()),
                actor_type: Some(format!("{:?}", actor.actor_type)),
                actor_subject: actor.subject.clone(),
                delegated_subject: actor.delegated_subject.clone(),
                external_user_id: actor.external_user_id.clone(),
                external_tenant_id: actor.external_tenant_id.clone(),
                application_id: actor.internal_application_id,
                resource_type: "response".to_string(),
                resource_id,
                action: action.to_string(),
                result,
                source_ip: ctx.source_ip,
                user_agent: ctx.user_agent.clone(),
                metadata,
            })
            .await
    }
}

async fn resolve_model_hint(
    repo: &PgPublicRepository,
    access: &PublicAccess,
    model: &str,
) -> Result<Uuid, AppError> {
    if let Ok(id) = Uuid::parse_str(model) {
        return Ok(id);
    }
    repo.find_visible_model_id_by_key(access, model)
        .await?
        .ok_or_else(|| AppError::unprocessable("model_not_found", "model is not visible"))
}

async fn validate_request(
    state: &AppState,
    actor: &Actor,
    request: &PublicResponseRequest,
    policy: &ApplicationExecutionPolicyRecord,
    stream: bool,
) -> Result<(), AppError> {
    if request.input.is_empty() {
        return Err(AppError::unprocessable(
            "invalid_execution_request",
            "input must contain at least one message",
        ));
    }
    if request.input.len() > state.settings.public_api.maximum_messages {
        return Err(AppError::unprocessable(
            "input_too_large",
            "too many input messages",
        ));
    }
    let input_items = request
        .input
        .iter()
        .map(|message| message.content.len())
        .sum::<usize>();
    if input_items > policy.maximum_input_items as usize {
        return Err(AppError::unprocessable(
            "input_too_large",
            "too many input content items",
        ));
    }
    if request.tools.len() > state.settings.public_api.maximum_tool_count {
        return Err(AppError::unprocessable(
            "unsupported_tool",
            "too many tool declarations",
        ));
    }
    if !request.tools.is_empty() {
        if !policy.tools_enabled || !state.authz.has_scope(actor, "moira:execution:use-tools") {
            return Err(AppError::unprocessable(
                "tool_not_allowed",
                "tools are not allowed for this caller",
            ));
        }
        return Err(AppError::unprocessable(
            "unsupported_tool",
            "client-defined tools are not registered in this phase",
        ));
    }
    if request.tool_choice.is_some() {
        return Err(AppError::unprocessable(
            "unsupported_request_option",
            "tool_choice is not supported without approved tools",
        ));
    }
    if request.top_p.is_some() || request.seed.is_some() {
        return Err(AppError::unprocessable(
            "unsupported_request_option",
            "top_p and seed are not supported by the current execution mapper",
        ));
    }
    if request_has_image(request) && !policy.vision_enabled {
        return Err(AppError::unprocessable(
            "model_capability_mismatch",
            "vision inputs are disabled for this application",
        ));
    }
    // F46 — refused *before* the `structured_output_enabled` check on purpose. Both refusals
    // are honest, but `structured_output_unsupported` invites the caller to enable the policy,
    // and enabling it would never make `json_object` work. The unconditional fact is the more
    // actionable one, so it is the one reported.
    refuse_json_object(&request.response_format)?;
    // F45 — placed beside the `json_object` refusal and, for the same reason, *before* the
    // `structured_output_enabled` check: enabling that policy would never make `strict: false`
    // work, so the unconditional fact is the more actionable one to report.
    refuse_non_strict_json_schema(&request.response_format)?;
    if matches!(
        request.response_format,
        PublicResponseFormat::JsonSchema { .. }
    ) && !policy.structured_output_enabled
    {
        return Err(AppError::unprocessable(
            "structured_output_unsupported",
            "structured output is disabled for this application",
        ));
    }
    validate_override(
        state,
        actor,
        request.route.is_some(),
        policy.route_overrides_allowed,
        "moira:execution:override-route",
        "route_override_forbidden",
    )?;
    validate_override(
        state,
        actor,
        request.model.is_some(),
        policy.model_overrides_allowed,
        "moira:execution:override-model",
        "model_override_forbidden",
    )?;
    validate_override(
        state,
        actor,
        request.provider.is_some(),
        policy.provider_overrides_allowed,
        "moira:execution:override-provider",
        "provider_override_forbidden",
    )?;
    validate_override(
        state,
        actor,
        request.credential_id.is_some(),
        policy.credential_overrides_allowed,
        "moira:execution:override-credential",
        "credential_override_forbidden",
    )?;
    validate_override(
        state,
        actor,
        request.timeout_ms.is_some(),
        policy.timeout_overrides_allowed,
        "moira:execution:override-timeout",
        "timeout_override_forbidden",
    )?;
    if let Some(max_tokens) = request.max_output_tokens
        && max_tokens > policy.maximum_output_tokens as u64
    {
        return Err(AppError::unprocessable(
            "max_output_tokens_exceeded",
            "max_output_tokens exceeds application policy",
        ));
    }
    if let Some(timeout_ms) = request.timeout_ms
        && timeout_ms > policy.maximum_timeout_ms as u64
    {
        return Err(AppError::unprocessable(
            "timeout_override_forbidden",
            "timeout_ms exceeds application policy",
        ));
    }
    if stream && !policy.streaming_enabled {
        return Err(AppError::unprocessable(
            "streaming_not_supported",
            "streaming is disabled for this application",
        ));
    }
    validate_metadata(state, &request.metadata)?;
    validate_response_format(state, &request.response_format)?;
    validate_content(state, request, policy).await
}

fn validate_override(
    state: &AppState,
    actor: &Actor,
    requested: bool,
    policy_allowed: bool,
    scope: &str,
    code: &'static str,
) -> Result<(), AppError> {
    if requested && (!policy_allowed || !state.authz.has_scope(actor, scope)) {
        return Err(AppError::coded(
            axum::http::StatusCode::FORBIDDEN,
            code,
            "execution override is not authorized",
        ));
    }
    Ok(())
}

async fn validate_content(
    state: &AppState,
    request: &PublicResponseRequest,
    policy: &ApplicationExecutionPolicyRecord,
) -> Result<(), AppError> {
    let mut image_count = 0usize;
    let mut image_urls: Vec<&str> = Vec::new();
    for message in &request.input {
        if message.content.is_empty()
            || message.content.len() > state.settings.public_api.maximum_content_parts_per_message
        {
            return Err(AppError::unprocessable(
                "input_too_large",
                "invalid number of message content parts",
            ));
        }
        if matches!(
            message.role,
            PublicMessageRole::System | PublicMessageRole::Developer
        ) && !policy.caller_system_instructions_allowed
        {
            return Err(AppError::unprocessable(
                "unsupported_message_role",
                "system and developer roles are not allowed for this caller",
            ));
        }
        if matches!(message.role, PublicMessageRole::Tool) {
            return Err(AppError::unprocessable(
                "unsupported_message_role",
                "tool role is not supported without approved tools",
            ));
        }
        for part in &message.content {
            match part {
                PublicContentPart::InputText { text } => {
                    if text.len() > state.settings.public_api.maximum_text_part_bytes {
                        return Err(AppError::unprocessable(
                            "input_too_large",
                            "input text part is too large",
                        ));
                    }
                }
                PublicContentPart::InputImage { image_url } => {
                    image_count += 1;
                    if image_count > state.settings.public_api.maximum_image_count {
                        return Err(AppError::unprocessable(
                            "image_too_large",
                            "too many image inputs",
                        ));
                    }
                    image_urls.push(image_url.as_str());
                }
            }
        }
    }
    validate_image_urls(state, image_urls).await
}

fn validate_metadata(state: &AppState, metadata: &Value) -> Result<(), AppError> {
    if !metadata.is_object() {
        return Err(AppError::unprocessable(
            "invalid_metadata",
            "metadata must be a JSON object",
        ));
    }
    let bytes = serde_json::to_vec(metadata)
        .map_err(|err| AppError::BadRequest(format!("metadata is invalid JSON: {err}")))?;
    if bytes.len() > state.settings.public_api.maximum_metadata_bytes {
        return Err(AppError::unprocessable(
            "invalid_metadata",
            "metadata is too large",
        ));
    }
    validate_metadata_value(
        metadata,
        0,
        state.settings.public_api.maximum_metadata_depth,
        state.settings.public_api.maximum_metadata_keys,
        state.settings.public_api.maximum_metadata_key_bytes,
        state.settings.public_api.maximum_metadata_string_bytes,
    )
}

fn validate_metadata_value(
    value: &Value,
    depth: usize,
    max_depth: usize,
    max_keys: usize,
    max_key_bytes: usize,
    max_string_bytes: usize,
) -> Result<(), AppError> {
    if depth > max_depth {
        return Err(AppError::unprocessable(
            "invalid_metadata",
            "metadata nesting is too deep",
        ));
    }
    match value {
        Value::Object(map) => {
            if map.len() > max_keys {
                return Err(AppError::unprocessable(
                    "invalid_metadata",
                    "metadata has too many keys",
                ));
            }
            for (key, value) in map {
                let normalized = key.to_ascii_lowercase();
                if key.len() > max_key_bytes || secret_like_key(&normalized) {
                    return Err(AppError::unprocessable(
                        "invalid_metadata",
                        "metadata contains a disallowed key",
                    ));
                }
                validate_metadata_value(
                    value,
                    depth + 1,
                    max_depth,
                    max_keys,
                    max_key_bytes,
                    max_string_bytes,
                )?;
            }
        }
        Value::String(value) if value.len() > max_string_bytes => {
            return Err(AppError::unprocessable(
                "invalid_metadata",
                "metadata string value is too large",
            ));
        }
        Value::Array(items) => {
            for value in items {
                validate_metadata_value(
                    value,
                    depth + 1,
                    max_depth,
                    max_keys,
                    max_key_bytes,
                    max_string_bytes,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Translates an OpenAI Responses-shaped request into Moira's native
/// [`PublicResponseRequest`].
///
/// A free function rather than a method because it reads nothing from the service: the
/// method on [`PublicExecutionService`] delegates here so the translation can be asserted
/// without an `AppState`, a database, or a provider.
pub(crate) fn openai_compat_to_public_request(
    request: OpenAiResponseCompatRequest,
) -> Result<PublicResponseRequest, AppError> {
    let response_format = compat_response_format(request.text)?;
    let input = if let Some(text) = request.input.as_str() {
        vec![PublicInputMessage {
            role: PublicMessageRole::User,
            content: vec![PublicContentPart::InputText {
                text: text.to_string(),
            }],
        }]
    } else {
        serde_json::from_value(request.input).map_err(|err| {
            AppError::unprocessable(
                "unsupported_request_option",
                format!("unsupported compatibility input shape: {err}"),
            )
        })?
    };
    Ok(PublicResponseRequest {
        input,
        route: None,
        model: request.model,
        provider: None,
        credential_id: None,
        conversation: None,
        temperature: request.temperature,
        top_p: None,
        max_output_tokens: request.max_output_tokens,
        timeout_ms: None,
        response_format,
        tools: Vec::new(),
        tool_choice: None,
        metadata: request.metadata,
        seed: None,
    })
}

/// Maps OpenAI's `text.format` onto Moira's native [`PublicResponseFormat`], refusing what
/// Moira will not honour instead of accepting it and doing something else.
///
/// `json_object` is the one shape deliberately refused rather than translated. It is refused
/// because Moira cannot honour it *anywhere* — see [`refuse_json_object`] for the mechanism and
/// the reversal condition. Since F46 the native path refuses it too, so translating it here
/// would produce the same 422 one layer later; refusing at the translation keeps the error
/// naming the field the caller actually sent.
fn compat_response_format(
    text: Option<OpenAiCompatTextOptions>,
) -> Result<PublicResponseFormat, AppError> {
    match text.and_then(|options| options.format) {
        None | Some(OpenAiCompatTextFormat::Text) => Ok(PublicResponseFormat::Text),
        Some(OpenAiCompatTextFormat::JsonSchema {
            name,
            schema,
            strict,
            // F45 — `strict` is carried across as the `Option` it arrived as. It used to be
            // `strict.unwrap_or(false)`, which collapsed "the caller omitted it" into "the caller
            // asked for non-strict" at the only boundary where the two were still distinguishable,
            // and then the native path dropped the result anyway. Both endpoints now refuse an
            // explicit `false` and accept an omitted one.
        }) => Ok(PublicResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        }),
        Some(OpenAiCompatTextFormat::JsonObject) => Err(AppError::unprocessable(
            "unsupported_request_option",
            "text.format type json_object is not supported: Moira has no way to ask a provider \
             for free-form JSON, so it would constrain the output to the empty object instead. \
             Send text.format.type = json_schema with an explicit schema instead.",
        )),
    }
}

/// F46 — `response_format: {"type":"json_object"}` is refused, on the native path as well as
/// on the compat one, rather than translated into a schema that means the opposite.
///
/// `JsonObject` used to become the output schema `{"type":"object"}`. `rig-core` 0.40 has **no**
/// representation of free-form JSON — the string `"json_object"` does not occur anywhere in the
/// crate — so `CompletionRequest::output_schema` is the only structured-output seam, and every
/// encoder reads it as a *constraint*:
///
/// - the OpenAI family (`OpenAi`, `AzureOpenAi`, `DeepSeek`, `OpenAiCompatible`, `Local`) runs
///   `sanitize_schema`, which completes an object schema with `properties: {}` and
///   `additionalProperties: false` and then sets `required` to the (empty) property key list,
///   and sends the result under a hardcoded `strict: true`. The wire schema
///   `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` is satisfied
///   by exactly one document, `{}`;
/// - Anthropic maps it to `output_config.format = json_schema` and its API has no free-form JSON
///   mode at all;
/// - Gemini only sets `generation_config.response_mime_type` when a schema is present.
///
/// So a caller asking for free-form JSON was answered with the empty object, under a `200` and a
/// `succeeded` status — the exact opposite of the request.
///
/// **Why refuse instead of honour.** Honouring it would mean Moira hand-building
/// `additional_params.response_format` per provider and bypassing `output_schema` — the boundary
/// violation `moira-rig-integration` exists to prevent, already invoked in this tree against the
/// same temptation for F45. It is also not expressible for every provider Moira routes to
/// (Anthropic has none), so the same request would succeed or fail by routing outcome. Opening
/// the schema with `additionalProperties: true` survives `sanitize_schema` but is rejected by
/// OpenAI's strict mode, which trades a silent wrong answer for a provider 400.
///
/// This follows F35, which already refuses this shape on `POST /v1/responses`. The two endpoints
/// now agree.
///
/// *Reversal condition:* this refusal becomes a translation when `rig-core` gains a
/// schema-free structured-output mode that Moira can set through a typed `CompletionRequest`
/// seam — not through `additional_params` — for every provider in
/// [`crate::domain::ProviderType`]. Until then the variant stays in the public contract so the
/// refusal can *name* it rather than 400 on an unknown variant.
fn refuse_json_object(format: &PublicResponseFormat) -> Result<(), AppError> {
    if matches!(format, PublicResponseFormat::JsonObject) {
        return Err(AppError::unprocessable(
            "unsupported_request_option",
            "response_format type json_object is not supported: Moira would constrain the \
             output to the empty object rather than to free-form JSON. Send response_format \
             {\"type\": \"json_schema\"} with an explicit schema instead.",
        ));
    }
    Ok(())
}

/// F45 — `response_format.json_schema.strict = false` is refused rather than accepted and
/// inverted.
///
/// **`strict` is not expressible anywhere.** Verified against the vendored `rig-core` 0.40, not
/// assumed:
///
/// - the OpenAI family (`OpenAi`, `AzureOpenAi`, `OpenAiCompatible`, `Local`) hardcodes
///   `"strict": true` in the `response_format` it builds (`providers/openai/completion/mod.rs:1838`).
///   `additional_params` cannot override it: the encoder's object is the `b` argument to
///   `json_utils::merge`, so it wins whenever an `output_schema` is present;
/// - Anthropic builds `OutputConfig { format: JsonSchema { schema } }` — no strictness field;
/// - Gemini sets `generation_config.response_mime_type` and `response_json_schema` — no
///   strictness field;
/// - DeepSeek drops the schema before the wire entirely (`SUPPORTS_RESPONSE_FORMAT = false`),
///   which is why F39 reconciles it out of routing at admission.
///
/// **Why this is not a harmless over-delivery.** Strict mode is not "the same answer, checked
/// harder". `sanitize_schema` sets `required` to the full property-key list, so every field the
/// caller declared optional becomes mandatory and the model is forced to invent one; and
/// OpenAI's strict mode rejects schemas outside its supported subset, so a caller who asked for
/// best-effort validation of a schema using, say, `pattern` receives a provider error. The
/// caller asked for one contract and silently got a different one that can fail.
///
/// **Why refusing is now available when F35 judged it unavailable.** F35 considered refusing
/// `strict: false` and rejected it because "OpenAI's own default for `strict` is falsy, so it
/// would refuse the common case". That was correct against the field as it stood — `#[serde(default)]
/// strict: bool` made an omitted `strict` indistinguishable from an explicit `false`. The field
/// is now `Option<bool>`, so only an explicit `false` is refused and the common case is
/// untouched. The compat DTO `OpenAiCompatTextFormat` already carried `Option<bool>`; the
/// information was being destroyed by `strict.unwrap_or(false)` at the translation, and that
/// `unwrap_or` is gone.
///
/// **Both endpoints refuse the same shape**, which is the property F35 and F46 both insisted on.
/// This is the third member of that family: `json_object` (F46) and non-strict `json_schema`
/// (here) are both refused natively and on `/v1/responses`.
///
/// *Reversal condition:* this refusal becomes an honouring when `rig-core` exposes strictness on
/// a typed `CompletionRequest` seam — not through `additional_params` — for every variant of
/// [`crate::domain::ProviderType`] Moira routes to. A release in which the OpenAI encoder reads
/// a strictness input instead of hardcoding `true`, *and* Anthropic and Gemini gain an
/// equivalent, is the concrete trigger. Partial support is not enough: routing-dependent
/// semantics on a public API is the failure mode this refusal exists to avoid.
fn refuse_non_strict_json_schema(format: &PublicResponseFormat) -> Result<(), AppError> {
    if let PublicResponseFormat::JsonSchema {
        strict: Some(false),
        ..
    } = format
    {
        return Err(AppError::unprocessable(
            "unsupported_request_option",
            "response_format json_schema strict=false is not supported: Moira has no way to ask \
             a provider for non-strict structured output, so the request would be answered in \
             strict mode instead — which makes every declared property required and can be \
             rejected outright by the provider. Omit strict, or send strict=true.",
        ));
    }
    Ok(())
}

fn validate_response_format(
    state: &AppState,
    format: &PublicResponseFormat,
) -> Result<(), AppError> {
    if let PublicResponseFormat::JsonSchema { schema, .. } = format {
        let bytes = serde_json::to_vec(schema)
            .map_err(|err| AppError::BadRequest(format!("schema is invalid JSON: {err}")))?;
        if bytes.len() > state.settings.public_api.maximum_schema_bytes {
            return Err(AppError::unprocessable(
                "structured_output_invalid",
                "structured output schema is too large",
            ));
        }
    }
    Ok(())
}

/// The client-visible message for every image URL refused on address-space grounds.
///
/// Deliberately one message for all of them. Whether a host resolved into private space,
/// sits outside the egress allow-list, or failed to resolve at all are facts about the
/// deployment's internal network, and a caller who can tell them apart has a working
/// internal port and address scanner built out of 422 responses. The specific reason is
/// recorded server-side by [`image_url_policy`]'s caller instead.
///
/// Scheme and parse failures are *not* funnelled through this message: those are
/// properties of the string the caller already holds, so naming them tells the caller
/// nothing it did not supply, and a precise message is worth far more than the nothing it
/// leaks.
const IMAGE_URL_REJECTED_MESSAGE: &str = "image URL is not allowed";

/// This deployment's image-URL policy.
///
/// Note what is *not* here compared with the JWKS policy: no content-type allow-list and
/// no response byte cap. Moira never fetches the image — it is handed to the provider as
/// `UserContent::image_url` — so there is no response for Moira to police. See
/// [`crate::config::ImageUrlSettings`] for the full reasoning.
fn image_url_policy(settings: &ImageUrlSettings) -> OutboundUrlPolicy {
    OutboundUrlPolicy {
        subject: "image url",
        dns_timeout: Duration::from_millis(settings.dns_timeout_ms.max(1)),
        allowed_hosts: settings.allowed_hosts.clone(),
        // The image path has always refused these and continues to.
        reject_credentials: true,
        allow_insecure: settings.allow_insecure_dev_urls,
    }
}

/// Validates every caller-supplied image URL through the shared SSRF guard.
///
/// Runs *after* the structural checks in [`validate_content`] rather than inline with
/// them: resolution is the only part of request validation that touches the network, and a
/// request that is already refusable for being too large or malformed must not be able to
/// buy a DNS lookup with the attempt.
async fn validate_image_urls(state: &AppState, urls: Vec<&str>) -> Result<(), AppError> {
    validate_image_urls_with(&state.settings.public_api.image_urls, urls, &SystemResolver).await
}

/// [`validate_image_urls`] with the resolver and settings supplied explicitly.
///
/// Split out so the request-level controls — de-duplication, the request-wide budget, and
/// the single client-visible message — are provable against a scripted resolver, with no
/// `AppState`, no database and no network. Those three are exactly the properties that a
/// test using the real resolver cannot pin down.
///
/// Distinct URLs are resolved once each, and the whole loop is bounded by one request-wide
/// deadline, so `maximum_image_count` slow hostnames cost one budget between them rather
/// than one each.
async fn validate_image_urls_with<R: HostResolver>(
    settings: &ImageUrlSettings,
    urls: Vec<&str>,
    resolver: &R,
) -> Result<(), AppError> {
    if urls.is_empty() {
        return Ok(());
    }
    let mut distinct: Vec<&str> = Vec::with_capacity(urls.len());
    for url in urls {
        if !distinct.contains(&url) {
            distinct.push(url);
        }
    }

    let policy = image_url_policy(settings);
    let budget = Duration::from_millis(settings.total_validation_timeout_ms.max(1));

    let checks = async {
        for raw in distinct {
            if let Err(denial) = validate_outbound_url(raw, &policy, resolver).await {
                return Err(image_denial_to_error(&denial));
            }
        }
        Ok(())
    };

    match tokio::time::timeout(budget, checks).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                budget_ms = budget.as_millis(),
                "image URL validation exceeded the request-wide budget"
            );
            Err(AppError::unprocessable(
                "image_url_not_allowed",
                IMAGE_URL_REJECTED_MESSAGE,
            ))
        }
    }
}

/// Maps a guard denial onto the caller-visible error, logging the server-side detail.
///
/// The detail can name a resolved internal address, so it goes to the log and never into
/// the response body.
fn image_denial_to_error(denial: &OutboundUrlDenial) -> AppError {
    tracing::warn!(
        reason = denial.reason().as_str(),
        detail = denial.detail(),
        "refused a caller-supplied image URL"
    );
    let message = match denial.reason() {
        // Facts about the caller's own string: safe, and useful, to name precisely.
        OutboundDenialReason::Url => "image URL is invalid",
        OutboundDenialReason::Scheme => "image URL must use https",
        OutboundDenialReason::Credentials => "image URL credentials are not allowed",
        // Everything below is a fact about the deployment's network. One message.
        OutboundDenialReason::Host
        | OutboundDenialReason::Resolution
        | OutboundDenialReason::IpRange
        | OutboundDenialReason::HostNotAllowed
        | OutboundDenialReason::Timeout => IMAGE_URL_REJECTED_MESSAGE,
    };
    AppError::unprocessable("image_url_not_allowed", message)
}

fn map_public_messages(
    request: &PublicResponseRequest,
    policy: &ApplicationExecutionPolicyRecord,
) -> Result<Vec<DomainMessage>, AppError> {
    request
        .input
        .iter()
        .map(|message| map_public_message(message, policy))
        .collect()
}

fn map_public_message(
    message: &PublicInputMessage,
    policy: &ApplicationExecutionPolicyRecord,
) -> Result<DomainMessage, AppError> {
    match message.role {
        PublicMessageRole::System | PublicMessageRole::Developer => {
            if !policy.caller_system_instructions_allowed {
                return Err(AppError::unprocessable(
                    "unsupported_message_role",
                    "system and developer roles are not allowed",
                ));
            }
            Ok(DomainMessage::system(text_only_content(message)?))
        }
        PublicMessageRole::User => {
            let content = message
                .content
                .iter()
                .map(|part| match part {
                    PublicContentPart::InputText { text } => {
                        DomainMessageContent::Text { text: text.clone() }
                    }
                    PublicContentPart::InputImage { image_url } => DomainMessageContent::ImageUrl {
                        url: image_url.clone(),
                    },
                })
                .collect::<Vec<_>>();
            if content.is_empty() {
                return Err(AppError::unprocessable(
                    "invalid_execution_request",
                    "user message is empty",
                ));
            }
            Ok(DomainMessage::new(DomainMessageRole::User, content))
        }
        PublicMessageRole::Assistant => Ok(DomainMessage::assistant(text_only_content(message)?)),
        PublicMessageRole::Tool => Err(AppError::unprocessable(
            "unsupported_message_role",
            "tool messages require an approved tool registry",
        )),
    }
}

fn text_only_content(message: &PublicInputMessage) -> Result<String, AppError> {
    let mut text = Vec::new();
    for part in &message.content {
        match part {
            PublicContentPart::InputText { text: value } => text.push(value.clone()),
            PublicContentPart::InputImage { .. } => {
                return Err(AppError::unprocessable(
                    "unsupported_input_type",
                    "this role only supports text content",
                ));
            }
        }
    }
    Ok(text.join("\n"))
}

fn terminal_update_from_outcome(
    hasher: &IdempotencyHasher,
    outcome: &ExecutionOutcome,
    policy: &ApplicationExecutionPolicyRecord,
    failure: Option<&ExecutionFailure>,
) -> ResponseTerminalUpdate {
    let output_text_bytes = outcome
        .output_text
        .as_ref()
        .map(|text| text.len())
        .unwrap_or(0);
    let output_hash = outcome
        .output_text
        .as_ref()
        .map(|text| hasher.hash(text.as_bytes()));
    ResponseTerminalUpdate {
        route_id: outcome.route.as_ref().map(|route| route.route_id),
        provider_id: outcome.model.as_ref().map(|model| model.provider_id),
        provider_model_id: outcome.model.as_ref().map(|model| model.provider_model_id),
        output_summary: json!({
            "persistence_mode": policy.persistence_mode,
            "output_text_bytes": output_text_bytes,
            "output_hash": output_hash,
        }),
        usage: outcome.usage.clone().into(),
        failure_class: failure.map(|value| failure_code(value.class).to_string()),
        failure_message: failure.map(|value| value.message.clone()),
        output_persisted: false,
    }
}

/// Prepends the planner's assembled context to the command's messages.
///
/// **Order is load-bearing.** The planner's messages go *before* the caller's own, so the
/// current turn stays last — which is what providers expect and what keeps retrieved content
/// from being the final thing the model reads. Any caller-supplied `system` message therefore
/// keeps its position at the head of the list and is never displaced or merged.
///
/// A no-op when nothing was planned, which is the default: retrieval is off unless an
/// application enables it.
fn apply_planned_context(
    prepared: &mut PreparedExecution,
    link: Option<&ConversationExecutionLink>,
) {
    let Some(link) = link else {
        return;
    };
    if link.context.messages.is_empty() {
        return;
    }
    let mut messages = link.context.messages.clone();
    messages.append(&mut prepared.command.messages);
    prepared.command.messages = messages;
}

/// The citations for a response, taken from the plan computed earlier in this same request.
///
/// Deliberately **not** re-read from `context_plans`: the ids are already in hand, and
/// re-querying would open a window where a concurrent plan for another execution could be
/// resolved onto this response. Empty when nothing was retrieved, which serialises as `[]` and
/// never as `null`.
fn citations_from_link(link: Option<&ConversationExecutionLink>) -> Vec<PublicCitation> {
    link.map(|link| link.context.citations.clone())
        .unwrap_or_default()
}

/// Why a **completed** response is being served without its output text — finding F40.
///
/// # This is never "the model returned nothing"
///
/// A completed response whose model produced an empty string still carries
/// `OutputText { text: "" }`, because `ExecutionStatus::Succeeded` always arrives with
/// `output_text: Some(_)`. So the caller can tell an empty answer from an absent one, which
/// is the whole reason this variant exists and the reason a bare `[]` would be wrong here.
///
/// # The mode is read from the response row, not from today's policy
///
/// `output_summary.persistence_mode` is written by [`terminal_update_from_outcome`] at the
/// moment the response completed. The application's policy may have changed since; the row is
/// the historical fact and the row is what is reported.
///
/// The literal was previously `"metadata_only_persistence"` unconditionally, which is correct
/// for the default configuration and **false** for the other three modes — it named a cause
/// the operator had not configured, sending them to change a setting that was not the reason.
/// `plain_content` and `encrypted_content` are honoured by nothing in the tree (see
/// `docs/response-persistence.md`, and F33 for the five unused encryption-at-rest columns), so
/// the honest answer for those is that content persistence is unimplemented, not that the
/// application asked for metadata only.
///
/// An absent or unrecognised mode falls back to the previous literal: `output_summary` is
/// always written on the completed path, so the fallback is unreachable, and if it ever became
/// reachable the default deployment's answer is the right guess.
fn output_unavailable_reason(record: &PublicResponseRecord) -> &'static str {
    if record.output_persisted {
        // Unreachable today: every `ResponseTerminalUpdate` in this file hardcodes
        // `output_persisted: false` and the column defaults to false, so nothing sets it.
        // It is spelled out rather than folded into the branch below because the previous
        // shape sent exactly this state to a silent `[]` — the more the system persisted, the
        // less it returned. Whoever implements content persistence will see this string
        // instead of an empty array, and the string names the work left to do: this read path
        // does not load the stored body.
        return "persisted_output_not_loaded";
    }
    match record
        .output_summary
        .get("persistence_mode")
        .and_then(Value::as_str)
    {
        Some("none") => "persistence_disabled",
        Some("plain_content" | "encrypted_content") => "content_persistence_not_implemented",
        _ => "metadata_only_persistence",
    }
}

fn public_response_from_record(
    record: &PublicResponseRecord,
    output_text: Option<String>,
    citations: Vec<PublicCitation>,
) -> PublicResponse {
    // The invariant: a **completed** response never carries an empty `output`. Either the
    // text, or a reason it is absent — because `[]` on a completed response is
    // indistinguishable from a model that returned nothing, and those are very different
    // results. A response that is queued, in progress, failed or cancelled has genuinely
    // produced no output, and `status` already says which; `[]` is the honest answer there
    // and stays.
    let output = if let Some(text) = output_text {
        vec![PublicOutputItem::Message {
            role: "assistant".to_string(),
            content: vec![PublicOutputContentPart::OutputText { text }],
        }]
    } else if record.status == PublicResponseStatus::Completed {
        vec![PublicOutputItem::Message {
            role: "assistant".to_string(),
            content: vec![PublicOutputContentPart::OutputUnavailable {
                reason: output_unavailable_reason(record).to_string(),
            }],
        }]
    } else {
        Vec::new()
    };
    PublicResponse {
        id: format!("resp_{}", record.id),
        object: "response".to_string(),
        created_at: record.created_at,
        status: record.status,
        execution_id: format!("exec_{}", record.execution_id),
        request_id: record.request_id.clone(),
        route: record
            .route_id
            .zip(record.route_key.clone())
            .map(|(id, key)| PublicRouteRef { id, key }),
        model: match (
            record.provider_model_id,
            record.provider_type,
            record.model_key.clone(),
        ) {
            (Some(id), Some(provider), Some(key)) => Some(PublicModelRef { id, provider, key }),
            _ => None,
        },
        conversation: record
            .conversation_public_id
            .clone()
            .map(|id| PublicConversationRef { id }),
        output,
        // Real provenance, from the `context_plans` row written for this execution — one entry
        // per memory or chunk that actually reached the assembled context. Never a superset:
        // a candidate the budget dropped is not cited (`assemble_context` builds both lists in
        // the same pass, so they cannot disagree).
        citations,
        usage: record.usage.clone(),
        metadata: record.metadata.clone(),
        output_persisted: record.output_persisted,
    }
}

fn map_runtime_event(
    response_id: Uuid,
    event: &RuntimeEventEnvelope,
    sequence: u64,
) -> Option<PublicSseEnvelope> {
    let (event_type, payload) = match event.event_type {
        RuntimeEventType::ExecutionStarted
        | RuntimeEventType::ExecutionCompleted
        | RuntimeEventType::ExecutionFailed => return None,
        RuntimeEventType::RoutingStarted => ("response.routing.started", event.payload.clone()),
        RuntimeEventType::RouteSelected => ("response.routing.completed", event.payload.clone()),
        RuntimeEventType::ModelSelected => ("response.model.selected", event.payload.clone()),
        RuntimeEventType::ProviderAttemptStarted => {
            ("response.provider_attempt.started", event.payload.clone())
        }
        RuntimeEventType::OutputTextDelta => ("response.output_text.delta", event.payload.clone()),
        RuntimeEventType::UsageUpdated => ("response.usage.updated", event.payload.clone()),
        RuntimeEventType::ProviderAttemptFailed => {
            ("response.provider_attempt.failed", event.payload.clone())
        }
        RuntimeEventType::FallbackSelected => ("response.fallback.selected", event.payload.clone()),
        RuntimeEventType::ToolCallStarted
        | RuntimeEventType::ToolCallDelta
        | RuntimeEventType::ToolCallCompleted
        | RuntimeEventType::ToolResult => return None,
        // F50 — deliberately not on the caller's stream.
        //
        // A dangling `agent_profile_id` is an operator fault in this deployment's
        // configuration, and its payload names a route and a profile the caller has no
        // relationship with. Putting it on the public SSE contract would leak the shape of
        // the admin plane to every API consumer. The audiences that need it are the
        // diagnostic endpoint (which returns every `RuntimeEventEnvelope` verbatim), the
        // `warn!` and the audit row.
        //
        // Issue #79 did not change this. The caller is now refused, and learns why from the
        // terminal `response.failed` event's `agent_profile_disabled` /
        // `agent_profile_not_found` error — which names the profile and the remedy without
        // exposing the route id or the admin-plane event stream.
        RuntimeEventType::AgentProfileUnavailable => return None,
    };
    Some(public_sse(
        response_id,
        event.execution_id,
        event.request_id.clone(),
        sequence,
        event_type,
        payload,
    ))
}

fn failure_update(
    failure: &ExecutionFailure,
    policy: &ApplicationExecutionPolicyRecord,
) -> ResponseTerminalUpdate {
    ResponseTerminalUpdate {
        route_id: None,
        provider_id: None,
        provider_model_id: None,
        output_summary: json!({ "persistence_mode": policy.persistence_mode }),
        usage: PublicUsageSummary::default(),
        failure_class: Some(failure_code(failure.class).to_string()),
        failure_message: Some(failure.message.clone()),
        output_persisted: false,
    }
}

fn cancellation_update(
    hasher: &IdempotencyHasher,
    outcome: Option<&ExecutionOutcome>,
    policy: &ApplicationExecutionPolicyRecord,
) -> ResponseTerminalUpdate {
    let failure =
        ExecutionFailure::new(ExecutionFailureClass::RequestCancelled, "stream cancelled");
    outcome
        .map(|outcome| terminal_update_from_outcome(hasher, outcome, policy, Some(&failure)))
        .unwrap_or_else(|| failure_update(&failure, policy))
}

fn terminal_failure(
    response: &PublicResponseRecord,
    sequence: u64,
    ctx: &RequestContext,
    failure: ExecutionFailure,
) -> PublicSseEnvelope {
    public_sse(
        response.id,
        response.execution_id,
        response.request_id.clone(),
        sequence,
        "response.failed",
        json!({
            "status": "failed",
            "error": {
                "code": failure_code(failure.class),
                "message": failure.message,
                "request_id": ctx.request_id
            }
        }),
    )
}

fn terminal_cancelled(
    response: &PublicResponseRecord,
    sequence: u64,
    ctx: &RequestContext,
) -> PublicSseEnvelope {
    public_sse(
        response.id,
        response.execution_id,
        response.request_id.clone(),
        sequence,
        "response.cancelled",
        json!({
            "status": "cancelled",
            "error": {
                "code": "request_cancelled",
                "message": "stream cancelled",
                "request_id": ctx.request_id
            }
        }),
    )
}

fn public_sse(
    response_id: Uuid,
    execution_id: Uuid,
    request_id: String,
    sequence: u64,
    event_type: &str,
    payload: Value,
) -> PublicSseEnvelope {
    PublicSseEnvelope {
        response_id: format!("resp_{response_id}"),
        execution_id: format!("exec_{execution_id}"),
        request_id,
        sequence,
        timestamp: Utc::now(),
        event_type: event_type.to_string(),
        payload,
    }
}

async fn send_public_event(
    tx: &mpsc::Sender<PublicSseEnvelope>,
    event: PublicSseEnvelope,
    send_timeout: Duration,
) -> bool {
    tokio::select! {
        _ = tx.closed() => false,
        result = tokio::time::timeout(send_timeout, tx.send(event)) => {
            matches!(result, Ok(Ok(())))
        }
    }
}

fn public_access(actor: &Actor, privileged: bool) -> Result<PublicAccess, AppError> {
    let application_id = effective_application_id(actor);
    if matches!(
        actor.actor_type,
        ActorType::ConsumerKey | ActorType::TrustedJwt
    ) && !privileged
        && application_id.is_none()
    {
        return Err(AppError::Forbidden(
            "application-bound caller identity is required for public access".to_string(),
        ));
    }

    Ok(PublicAccess {
        privileged,
        application_id,
        external_tenant_id: actor.external_tenant_id.clone().or(actor.tenant_id.clone()),
        external_user_id: actor.external_user_id.clone(),
    })
}

fn can_read_all(actor: &Actor, scope: &str, state: &AppState) -> bool {
    matches!(actor.actor_type, ActorType::SystemKey | ActorType::DevAdmin)
        && state.authz.has_scope(actor, scope)
}

fn effective_application_id(actor: &Actor) -> Option<Uuid> {
    actor.internal_application_id
}

fn caller_runtime_identity(actor: &Actor) -> CallerRuntimeIdentity {
    CallerRuntimeIdentity {
        actor_type: format!("{:?}", actor.actor_type),
        subject: actor.subject.clone(),
        external_user_id: actor.external_user_id.clone(),
        external_tenant_id: actor.external_tenant_id.clone().or(actor.tenant_id.clone()),
        application_id: effective_application_id(actor),
        scopes: actor.scopes.clone(),
    }
}

fn retention_expires_at(
    policy: &ApplicationExecutionPolicyRecord,
) -> Option<chrono::DateTime<Utc>> {
    if policy.response_retention_seconds == 0 {
        None
    } else {
        Some(Utc::now() + chrono::Duration::seconds(policy.response_retention_seconds))
    }
}

fn request_has_image(request: &PublicResponseRequest) -> bool {
    request.input.iter().any(|message| {
        message
            .content
            .iter()
            .any(|part| matches!(part, PublicContentPart::InputImage { .. }))
    })
}

/// The fingerprint `/v1/responses` wrote **before** plan 06 unified the three formulas.
///
/// Read-only, consulted by `claim_idempotency` so a ledger row written by the previous
/// release still replays; never written. It hashes
/// `{actor_type, subject, api_key_id, application_id}`, so two callers differing only by
/// tenant, trusted-JWT issuer, external user or delegated subject collided — the public
/// half of the P2-15 hole.
///
/// `application_id` stays an explicit parameter rather than being read off the `Actor`
/// because that is what the pre-plan-06 call site passed, and reproducing the old value
/// byte-for-byte is the entire point. It is
/// `effective_application_id(actor) == actor.internal_application_id` at the one call site,
/// which is why the unified formula loses no information by dropping the parameter —
/// `internal_application_id` is one of its ten fields.
/// `tests::the_legacy_public_fingerprint_collided_across_tenant_and_delegation` pins both
/// halves of that claim.
///
/// TODO(post-deploy): delete together with the legacy passes in `claim_idempotency`, once 24h
/// (the `expires_at` window set by `infra::repositories::public::idempotency_record`) have
/// elapsed since the deploy carrying plan 06 Module 16. Gated on a deploy, not on a merge,
/// and owned by no plan.
fn legacy_public_actor_fingerprint(actor: &Actor, application_id: Option<Uuid>) -> String {
    secret_fingerprint(
        format!(
            "{:?}:{}:{}:{}",
            actor.actor_type,
            actor.subject.as_deref().unwrap_or(""),
            actor
                .api_key_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            application_id.map(|id| id.to_string()).unwrap_or_default()
        )
        .as_bytes(),
    )
}

/// The canonical bytes an idempotent `/v1/responses` request hashes to.
///
/// Returns bytes rather than a digest so the read path can run them through
/// `IdempotencyHasher::verify`, which accepts the current keyed digest and the pre-switch
/// unkeyed one alike.
fn normalized_request_bytes<T: Serialize>(request: &T) -> Result<Vec<u8>, AppError> {
    serde_json::to_vec(request)
        .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))
}

/// Turns an existing ledger row into the replay state, or into the conflict/in-progress
/// error the idempotency contract requires.
///
/// `verify` — not string equality — so a row written before the switch to keyed hashing
/// still matches the request that produced it.
fn replayed_idempotency_state(
    hasher: &IdempotencyHasher,
    request_bytes: &[u8],
    existing: IdempotencyRecord,
) -> Result<IdempotencyState, AppError> {
    if !hasher.verify(request_bytes, &existing.request_hash) {
        return Err(AppError::conflict(
            "idempotency_conflict",
            "same Idempotency-Key was used with a different request",
        ));
    }
    if existing.response_body.is_none() {
        return Err(AppError::conflict(
            "execution_in_progress",
            "execution is in progress",
        ));
    }
    Ok(IdempotencyState {
        key_hash: existing.idempotency_key_hash,
        actor_fingerprint: existing.actor_fingerprint,
        operation: "response.create",
    })
}

fn validate_policy_request(request: &ApplicationExecutionPolicyPutRequest) -> Result<(), AppError> {
    if request
        .response_retention_seconds
        .is_some_and(|value| value < 0)
        || request
            .maximum_request_bytes
            .is_some_and(|value| value <= 0)
        || request.maximum_input_items.is_some_and(|value| value <= 0)
        || request
            .maximum_output_tokens
            .is_some_and(|value| value <= 0)
        || request.maximum_timeout_ms.is_some_and(|value| value <= 0)
        || request
            .rate_limit_requests_per_minute
            .is_some_and(|value| value <= 0)
        || request
            .rate_limit_streams_per_minute
            .is_some_and(|value| value <= 0)
    {
        return Err(AppError::BadRequest(
            "execution policy numeric limits must be positive".to_string(),
        ));
    }
    Ok(())
}

fn secret_like_key(key: &str) -> bool {
    matches!(
        key,
        "api_key"
            | "authorization"
            | "password"
            | "secret"
            | "token"
            | "access_token"
            | "refresh_token"
            | "private_key"
            | "cookie"
    )
}

fn failure_http_status(class: ExecutionFailureClass) -> axum::http::StatusCode {
    use axum::http::StatusCode;
    match class {
        ExecutionFailureClass::InvalidExecutionRequest
        | ExecutionFailureClass::StructuredOutputInvalid => StatusCode::UNPROCESSABLE_ENTITY,
        ExecutionFailureClass::RouteForbidden
        | ExecutionFailureClass::ModelForbidden
        | ExecutionFailureClass::CredentialForbidden => StatusCode::FORBIDDEN,
        // Issue #79. `AgentProfileNotFound` joins the family it belongs to: a runtime reference on
        // the resolution chain (route → agent profile → model → credential) that does not resolve
        // is already a `404` for the route and for the credential, and neither of those is named
        // by the caller either. The status also agrees with what the admin plane says about the
        // same id — `GET /api/v1/admin/agent-profiles/{id}` answers `404` for a soft-deleted
        // profile — and two different answers about one row would be worse than either.
        ExecutionFailureClass::NoEligibleModel
        | ExecutionFailureClass::ModelNotFound
        | ExecutionFailureClass::RouteNotFound
        | ExecutionFailureClass::AgentProfileNotFound
        | ExecutionFailureClass::CredentialNotFound => StatusCode::NOT_FOUND,
        // Issue #79, and deliberately *not* `404`: the profile exists, it is addressable on the
        // admin plane, and the operator switched it off. Saying "not found" about a row an
        // operator can see would send them looking for the wrong thing. `409` is the state
        // conflict it is — the request cannot be completed while the target resource is in this
        // state, and retrying is futile until the state changes. It is not `503`, which promises
        // that waiting helps, and not `502`, which blames a provider none of this contacted.
        ExecutionFailureClass::AgentProfileDisabled => StatusCode::CONFLICT,
        ExecutionFailureClass::CapacityExhausted => StatusCode::TOO_MANY_REQUESTS,
        ExecutionFailureClass::ProviderTimeout | ExecutionFailureClass::DeadlineExceeded => {
            StatusCode::GATEWAY_TIMEOUT
        }
        _ => StatusCode::BAD_GATEWAY,
    }
}

fn failure_code(class: ExecutionFailureClass) -> &'static str {
    // Delegates to the domain type so the code strings have ONE definition. `ExecutionFailureClass::code`
    // is walked by the i18n catalog gate, which refuses to compile when a variant has no catalog
    // entry; a second copy of this mapping here could drift out from under that guarantee.
    class.code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ResponsePersistenceMode;

    /// Parses whatever a caller would actually put on the wire, so the assertion covers the
    /// DTO's own shape rather than a hand-built struct that cannot go wrong.
    fn compat_request(body: Value) -> Result<OpenAiResponseCompatRequest, serde_json::Error> {
        serde_json::from_value(body)
    }

    use axum::http::StatusCode;

    fn compat_error(err: &AppError) -> Option<(StatusCode, &'static str)> {
        match err {
            AppError::Api { status, code, .. } => Some((*status, code)),
            _ => None,
        }
    }

    /// F35. `text.format` was declared on a `deny_unknown_fields` struct and read by nothing,
    /// so a schema request was accepted, dropped, and answered 200 with prose.
    #[test]
    fn compat_text_format_json_schema_becomes_a_structured_response_format() {
        let request = compat_request(json!({
            "input": "hello",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object", "properties": { "answer": { "type": "string" } } }
                }
            }
        }))
        .expect("json_schema text.format should deserialize");

        let public = openai_compat_to_public_request(request)
            .expect("json_schema text.format should map to a public request");

        let PublicResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } = public.response_format
        else {
            panic!(
                "text.format.json_schema must not be discarded, got {:?}",
                public.response_format
            );
        };
        assert_eq!(name, "answer");
        assert_eq!(
            strict, None,
            "F45 — an omitted strict must stay None. Under the previous `#[serde(default)] bool` it
             collapsed to `false`, which is the value that is now refused, so the distinction is
             the whole fix"
        );
        assert_eq!(schema["properties"]["answer"]["type"], "string");
    }

    /// The caller's `strict` reaches the native request rather than being replaced by a default.
    #[test]
    fn compat_text_format_json_schema_carries_the_callers_strict_flag() {
        let request = compat_request(json!({
            "input": "hello",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object" },
                    "strict": true
                }
            }
        }))
        .expect("strict json_schema text.format should deserialize");

        let public = openai_compat_to_public_request(request).expect("strict schema should map");
        assert!(matches!(
            public.response_format,
            PublicResponseFormat::JsonSchema {
                strict: Some(true),
                ..
            }
        ));
    }

    /// F46 — the predicate. `json_object` is refused on the **native** representation, which is
    /// what `prepare_execution` reads and what the compat translation produces.
    ///
    /// This is the predicate half of the pair HANDOFF §3.4 insists on; the wiring half is
    /// `tests/openai_compat_text_format.rs`, which proves the refusal survives routing and that
    /// no provider call is made. Every laundering finding in this repository has had a correct
    /// predicate, so this test alone would prove nothing about the endpoint.
    ///
    /// It also asserts the refusal is *only* about `json_object`: `Text` and `JsonSchema` must
    /// pass, or the cheapest way to make this test green would be to refuse everything.
    #[test]
    fn native_json_object_is_refused_and_the_other_two_formats_are_not() {
        let err = refuse_json_object(&PublicResponseFormat::JsonObject)
            .expect_err("json_object must be refused on the native path");
        assert_eq!(
            compat_error(&err),
            Some((
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_request_option"
            )),
            "got {err:?}"
        );
        refuse_json_object(&PublicResponseFormat::Text).expect("text must stay honoured");
        refuse_json_object(&PublicResponseFormat::JsonSchema {
            name: "answer".to_string(),
            schema: json!({ "type": "object" }),
            strict: Some(true),
        })
        .expect("json_schema must stay honoured");
    }

    /// F45 — the predicate. Only an **explicit** `strict: false` is refused.
    ///
    /// The three cases are the whole point and none is padding. `None` is the common case and
    /// the reason F35 judged this refusal unavailable — under the previous `#[serde(default)]
    /// bool` it was the same value as `Some(false)`, so refusing would have refused everyone.
    /// `Some(true)` is the case that matches what rig actually sends. If either regressed to a
    /// refusal the endpoint would be broken for every structured-output caller, and refusing
    /// everything would otherwise be the cheapest way to make the first assertion green.
    #[test]
    fn native_strict_false_is_refused_and_an_omitted_or_true_strict_is_not() {
        let refused = |strict: Option<bool>| {
            refuse_non_strict_json_schema(&PublicResponseFormat::JsonSchema {
                name: "answer".to_string(),
                schema: json!({ "type": "object" }),
                strict,
            })
        };

        let err = refused(Some(false)).expect_err("strict=false must be refused");
        assert_eq!(
            compat_error(&err),
            Some((
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_request_option"
            )),
            "got {err:?}"
        );

        refused(None).expect("an omitted strict must stay honoured — it is the common case");
        refused(Some(true)).expect("strict=true must stay honoured — it is what rig sends");
        refuse_non_strict_json_schema(&PublicResponseFormat::Text)
            .expect("text carries no schema and must be untouched");
    }

    /// `json_object` is refused rather than translated, on the compat path's own field name.
    #[test]
    fn compat_text_format_json_object_is_refused_rather_than_silently_dropped() {
        let request = compat_request(json!({
            "input": "hello",
            "text": { "format": { "type": "json_object" } }
        }))
        .expect("json_object text.format should deserialize");

        let err = openai_compat_to_public_request(request)
            .expect_err("json_object must not be accepted and ignored");
        assert_eq!(
            compat_error(&err),
            Some((
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_request_option"
            )),
            "got {err:?}"
        );
    }

    /// The two shapes that mean "prose", which is what Moira already does. Neither may start
    /// failing: rejecting a request whose semantics are already satisfied buys nothing.
    #[test]
    fn compat_text_format_text_and_absent_text_both_stay_prose() {
        for body in [
            json!({ "input": "hello" }),
            json!({ "input": "hello", "text": {} }),
            json!({ "input": "hello", "text": { "format": { "type": "text" } } }),
        ] {
            let request =
                compat_request(body.clone()).unwrap_or_else(|err| panic!("{body} -> {err}"));
            let public = openai_compat_to_public_request(request)
                .unwrap_or_else(|err| panic!("{body} -> {err:?}"));
            assert!(
                matches!(public.response_format, PublicResponseFormat::Text),
                "{body} must stay Text"
            );
        }
    }

    /// Every `text` key Moira does not honour is refused by `deny_unknown_fields` rather than
    /// accepted and ignored — the defect F35 named, generalised past `format`.
    #[test]
    fn compat_text_rejects_options_moira_does_not_honour() {
        for body in [
            json!({ "input": "hello", "text": { "verbosity": "low" } }),
            json!({ "input": "hello", "text": { "format": { "type": "yaml" } } }),
            json!({ "input": "hello", "text": { "format": { "type": "json_schema", "name": "a", "schema": {}, "description": "d" } } }),
            json!({ "input": "hello", "text": "json" }),
        ] {
            assert!(
                compat_request(body.clone()).is_err(),
                "{body} must not deserialize into a request Moira silently ignores"
            );
        }
    }

    #[test]
    fn pipeline_names_match_phase_four_order() {
        let pipeline = ExecutionPipeline::phase_four();
        assert_eq!(
            pipeline.names().first().copied(),
            Some("RequestNormalizationInterceptor")
        );
        assert!(pipeline.names().contains(&"ExecutionDispatchInterceptor"));
        assert_eq!(pipeline.names().last().copied(), Some("AuditInterceptor"));
    }

    #[tokio::test]
    async fn metadata_rejects_secret_like_keys() {
        let state = AppState::new(crate::config::Settings::default(), None)
            .await
            .unwrap();
        let metadata = json!({ "api_key": "sk-test" });
        assert!(validate_metadata(&state, &metadata).is_err());
    }

    #[test]
    fn unbound_trusted_jwt_cannot_receive_public_access() {
        let actor = Actor {
            actor_type: ActorType::TrustedJwt,
            scopes: vec!["moira:responses:read".to_string()],
            ..Actor::default()
        };

        assert!(matches!(
            public_access(&actor, false),
            Err(AppError::Forbidden(_))
        ));
    }

    #[test]
    fn unbound_consumer_key_cannot_receive_public_access() {
        let actor = Actor {
            actor_type: ActorType::ConsumerKey,
            scopes: vec!["moira:responses:read".to_string()],
            ..Actor::default()
        };

        assert!(matches!(
            public_access(&actor, false),
            Err(AppError::Forbidden(_))
        ));
    }

    #[test]
    fn consumer_and_privileged_actor_access_semantics_are_preserved() {
        let application_id = Uuid::now_v7();
        let consumer = Actor {
            actor_type: ActorType::ConsumerKey,
            internal_application_id: Some(application_id),
            ..Actor::default()
        };
        let system = Actor {
            actor_type: ActorType::SystemKey,
            ..Actor::default()
        };

        let consumer_access = public_access(&consumer, false).unwrap();
        assert!(!consumer_access.privileged);
        assert_eq!(consumer_access.application_id, Some(application_id));

        let privileged_access = public_access(&system, true).unwrap();
        assert!(privileged_access.privileged);
        assert_eq!(privileged_access.application_id, None);
    }

    #[test]
    fn internal_terminal_events_are_not_public_sse_events() {
        let response_id = Uuid::now_v7();
        for event_type in [
            RuntimeEventType::ExecutionStarted,
            RuntimeEventType::ExecutionCompleted,
            RuntimeEventType::ExecutionFailed,
        ] {
            let event = RuntimeEventEnvelope {
                request_id: "req-test".to_string(),
                execution_id: Uuid::now_v7(),
                sequence: 1,
                timestamp: Utc::now(),
                event_type,
                payload: json!({}),
            };
            assert!(map_runtime_event(response_id, &event, 1).is_none());
        }
    }

    /// F50 — the dangling-agent-profile signal stays on the operator side.
    ///
    /// `AgentProfileUnavailable` reports that *this deployment's* route points at a
    /// disabled or deleted agent profile, and its payload names a route id, a route key and
    /// a profile id. That is admin-plane shape, and a caller can do nothing with it, so it
    /// must not appear on the public SSE contract even though the caller's own request is
    /// the thing degraded by it. The operator reads it from
    /// `POST /api/v1/admin/runtime/diagnose`, the `warn!` and the audit row instead.
    ///
    /// The control is in the same test and is load-bearing: without it this assertion holds
    /// equally against a `map_runtime_event` that returns `None` for everything, which is
    /// the F16 shape §3.4 records — an assertion that nothing happened is worthless unless
    /// something can happen.
    #[test]
    fn the_agent_profile_unavailable_event_is_not_a_public_sse_event() {
        let response_id = Uuid::now_v7();
        let envelope = |event_type| RuntimeEventEnvelope {
            request_id: "req-test".to_string(),
            execution_id: Uuid::now_v7(),
            sequence: 1,
            timestamp: Utc::now(),
            event_type,
            payload: json!({ "route_key": "general" }),
        };

        assert!(
            map_runtime_event(
                response_id,
                &envelope(RuntimeEventType::AgentProfileUnavailable),
                1
            )
            .is_none(),
            "F50's operator signal reached the caller's stream; its payload names an \
             internal route and agent profile"
        );
        assert!(
            map_runtime_event(response_id, &envelope(RuntimeEventType::RouteSelected), 1).is_some(),
            "route_selected is a public event, so the exclusion asserted above proves \
             nothing about this event in particular"
        );
    }

    #[test]
    fn runtime_delta_keeps_the_public_envelope_contract() {
        let response_id = Uuid::now_v7();
        let execution_id = Uuid::now_v7();
        let event = RuntimeEventEnvelope {
            request_id: "req-test".to_string(),
            execution_id,
            sequence: 7,
            timestamp: Utc::now(),
            event_type: RuntimeEventType::OutputTextDelta,
            payload: json!({ "delta": "hello" }),
        };

        let mapped = map_runtime_event(response_id, &event, 3).expect("delta should be public");
        assert_eq!(mapped.event_type, "response.output_text.delta");
        assert_eq!(mapped.sequence, 3);
        assert_eq!(mapped.response_id, format!("resp_{response_id}"));
        assert_eq!(mapped.execution_id, format!("exec_{execution_id}"));
        assert_eq!(mapped.payload, json!({ "delta": "hello" }));
    }

    #[tokio::test]
    async fn stalled_public_consumer_hits_bounded_send_timeout() {
        let (tx, _rx) = mpsc::channel(1);
        assert!(
            send_public_event(
                &tx,
                public_sse(
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                    "req-test".to_string(),
                    1,
                    "response.created",
                    json!({})
                ),
                Duration::from_millis(10),
            )
            .await
        );
        assert!(
            !send_public_event(
                &tx,
                public_sse(
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                    "req-test".to_string(),
                    2,
                    "response.in_progress",
                    json!({})
                ),
                Duration::from_millis(10),
            )
            .await
        );
    }

    /// The public half of the P2-15 hole, pinned rather than described.
    ///
    /// Plan 06 §16.4 asks for proof that the pre-unification formula was actually broken.
    /// This asserts both halves in one place: the 4-field formula `/v1/responses` used to
    /// write collided on every field below, and the formula now writing the ledger does
    /// not. `application_id` is held constant throughout — it was the only identity field
    /// beyond `{actor_type, subject, api_key_id}` the old formula could see, so varying it
    /// would prove nothing about what was missing.
    #[test]
    fn the_legacy_public_fingerprint_collided_across_tenant_and_delegation() {
        use crate::security::ActorType;

        let application_id = Some(Uuid::now_v7());
        let base = Actor {
            actor_type: ActorType::ConsumerKey,
            subject: Some("shared-subject".to_string()),
            api_key_id: Some(Uuid::nil()),
            internal_application_id: application_id,
            ..Actor::default()
        };

        let variants: [(&str, Actor); 5] = [
            (
                "tenant_id",
                Actor {
                    tenant_id: Some("other-tenant".to_string()),
                    ..base.clone()
                },
            ),
            (
                "external_tenant_id",
                Actor {
                    external_tenant_id: Some("other-tenant".to_string()),
                    ..base.clone()
                },
            ),
            (
                "external_user_id",
                Actor {
                    external_user_id: Some("other-user".to_string()),
                    ..base.clone()
                },
            ),
            (
                "delegated_subject",
                Actor {
                    delegated_subject: Some("other-user".to_string()),
                    ..base.clone()
                },
            ),
            (
                "trusted_jwt_issuer_id",
                Actor {
                    trusted_jwt_issuer_id: Some(Uuid::now_v7()),
                    ..base.clone()
                },
            ),
        ];

        for (field, variant) in variants {
            assert_eq!(
                legacy_public_actor_fingerprint(&base, application_id),
                legacy_public_actor_fingerprint(&variant, application_id),
                "the pre-plan-06 formula is supposed to be blind to `{field}` — if this \
                 stops holding, `claim_idempotency`'s legacy sweep is probing a value \
                 production never wrote and pre-deploy rows will not replay"
            );
            let hasher = crate::security::IdempotencyHasher::new(b"public-pepper".to_vec(), "v1");
            assert_ne!(
                crate::application::admin::actor_fingerprint(&hasher, &base),
                crate::application::admin::actor_fingerprint(&hasher, &variant),
                "the unified formula must isolate replay across `{field}`"
            );
        }
    }

    /// The unified formula drops `claim_idempotency`'s explicit `application_id` argument.
    /// That is only lossless because the argument was always
    /// `effective_application_id(actor)`, i.e. `actor.internal_application_id`, which the
    /// unified formula already covers. If a future caller passes something else, the two
    /// halves of this test disagree and it fails.
    #[test]
    fn dropping_the_explicit_application_id_argument_loses_no_isolation() {
        use crate::security::ActorType;

        let base = Actor {
            actor_type: ActorType::ConsumerKey,
            subject: Some("shared-subject".to_string()),
            api_key_id: Some(Uuid::nil()),
            internal_application_id: Some(Uuid::now_v7()),
            ..Actor::default()
        };
        let other = Actor {
            internal_application_id: Some(Uuid::now_v7()),
            ..base.clone()
        };

        assert_eq!(
            effective_application_id(&base),
            base.internal_application_id
        );
        let hasher = crate::security::IdempotencyHasher::new(b"public-pepper".to_vec(), "v1");
        assert_ne!(
            crate::application::admin::actor_fingerprint(&hasher, &base),
            crate::application::admin::actor_fingerprint(&hasher, &other),
            "application identity must still partition the ledger without the argument"
        );
        assert_ne!(
            legacy_public_actor_fingerprint(&base, effective_application_id(&base)),
            legacy_public_actor_fingerprint(&other, effective_application_id(&other)),
        );
    }

    // ------------------------------------------------------------------
    // F40 — what a completed response says when it has no output text.
    //
    // The finding reported an empty `output` array for a completed, persisted response.
    // That state is unreachable: nothing in the tree ever writes `output_persisted = true`.
    // What *is* reachable is a completed response whose text was never persisted, and the
    // single hardcoded reason it used to give was correct for one persistence mode out of
    // four. These cases pin the whole matrix, because the cheapest edit that restores the
    // defect is to collapse the four answers back into one literal — and a test that only
    // exercised the default mode would not notice.
    // ------------------------------------------------------------------

    fn completed_record(persistence_mode: &str) -> PublicResponseRecord {
        PublicResponseRecord {
            id: Uuid::now_v7(),
            execution_id: Uuid::now_v7(),
            request_id: "req-f40".to_string(),
            application_id: Some(Uuid::now_v7()),
            external_tenant_id: None,
            external_user_id: None,
            conversation_id: None,
            conversation_public_id: None,
            status: PublicResponseStatus::Completed,
            route_id: None,
            route_key: None,
            provider_id: None,
            provider_type: None,
            provider_model_id: None,
            model_key: None,
            // Exactly the shape `terminal_update_from_outcome` writes, so these cases read
            // the same field the production path populates rather than a convenient stand-in.
            output_summary: json!({
                "persistence_mode": persistence_mode,
                "output_text_bytes": 11,
                "output_hash": "hash",
            }),
            usage: PublicUsageSummary::default(),
            metadata: json!({}),
            failure_class: None,
            failure_message: None,
            output_persisted: false,
            created_at: Utc::now(),
            started_at: None,
            completed_at: Some(Utc::now()),
            failed_at: None,
            cancelled_at: None,
            expires_at: None,
            version: 1,
        }
    }

    fn only_content_part(response: &PublicResponse) -> &PublicOutputContentPart {
        let [PublicOutputItem::Message { content, .. }] = response.output.as_slice() else {
            panic!(
                "a completed response must carry exactly one output item, got {:?}",
                response.output
            );
        };
        let [part] = content.as_slice() else {
            panic!("expected exactly one content part, got {content:?}");
        };
        part
    }

    fn unavailable_reason(response: &PublicResponse) -> &str {
        match only_content_part(response) {
            PublicOutputContentPart::OutputUnavailable { reason } => reason,
            other => panic!("expected an output_unavailable part, got {other:?}"),
        }
    }

    /// Each persistence mode gets its own answer, and no two of them share one.
    ///
    /// The previous code answered `"metadata_only_persistence"` for all four, which is true
    /// only for the default. The distinctness assertion is the load-bearing half: four
    /// separate equality checks would all pass against an implementation that returned the
    /// same string for two modes if the expectations were ever edited to match.
    #[test]
    fn every_persistence_mode_gets_its_own_reason() {
        let cases = [
            ("metadata_only", "metadata_only_persistence"),
            ("none", "persistence_disabled"),
            ("plain_content", "content_persistence_not_implemented"),
            ("encrypted_content", "content_persistence_not_implemented"),
        ];

        for (mode, expected) in cases {
            let response = public_response_from_record(&completed_record(mode), None, Vec::new());
            assert_eq!(
                unavailable_reason(&response),
                expected,
                "persistence_mode {mode} reported the wrong reason"
            );
        }

        // `plain_content` and `encrypted_content` deliberately share an answer — content
        // persistence is unimplemented for both — so the distinct set is three, not four.
        let distinct: std::collections::BTreeSet<&str> =
            cases.iter().map(|(_, reason)| *reason).collect();
        assert_eq!(
            distinct.len(),
            3,
            "the four modes collapsed into fewer than three answers: {distinct:?}"
        );
    }

    /// The latent inversion F40 actually describes: the more the row claims to have
    /// persisted, the less the old code returned.
    ///
    /// Unreachable today, and named rather than left silent so the day content persistence
    /// lands the symptom is a string that says what happened instead of an empty array that
    /// looks like a model with nothing to say.
    #[test]
    fn a_completed_response_claiming_persisted_output_says_so_rather_than_returning_nothing() {
        let mut record = completed_record("plain_content");
        record.output_persisted = true;

        let response = public_response_from_record(&record, None, Vec::new());
        assert!(
            !response.output.is_empty(),
            "a completed response must never carry an empty output array"
        );
        assert_eq!(unavailable_reason(&response), "persisted_output_not_loaded");
    }

    /// The other side of the invariant, so the fix cannot quietly become
    /// "always emit an explanation".
    ///
    /// A response that is queued, running, failed or cancelled has genuinely produced no
    /// output and `status` already says which. `[]` is honest there, and turning it into an
    /// `output_unavailable` part would be a public-shape change with nothing behind it.
    #[test]
    fn only_completed_responses_carry_an_explanation() {
        for status in [
            PublicResponseStatus::Queued,
            PublicResponseStatus::InProgress,
            PublicResponseStatus::Failed,
            PublicResponseStatus::Cancelled,
        ] {
            let mut record = completed_record("metadata_only");
            record.status = status;
            let response = public_response_from_record(&record, None, Vec::new());
            assert!(
                response.output.is_empty(),
                "{status:?} must return an empty output array, got {:?}",
                response.output
            );
        }
    }

    /// An empty model reply is not the same fact as an unavailable one, and the caller can
    /// tell them apart.
    ///
    /// This is the assumption the whole finding rests on: if a succeeded execution could
    /// arrive with `output_text: None`, then `output_unavailable` would be ambiguous no
    /// matter what reason it carried.
    #[test]
    fn an_empty_model_reply_is_output_text_not_output_unavailable() {
        let response = public_response_from_record(
            &completed_record("metadata_only"),
            Some(String::new()),
            Vec::new(),
        );
        match only_content_part(&response) {
            PublicOutputContentPart::OutputText { text } => assert_eq!(text, ""),
            other => panic!("an empty completion must stay output_text, got {other:?}"),
        }
    }

    /// An unrecognised or missing mode falls back to the value the default deployment sees.
    #[test]
    fn an_unreadable_persistence_mode_falls_back_to_the_default_answer() {
        let mut record = completed_record("metadata_only");
        record.output_summary = json!({});
        assert_eq!(
            unavailable_reason(&public_response_from_record(&record, None, Vec::new())),
            "metadata_only_persistence"
        );

        record.output_summary = json!({ "persistence_mode": "a_mode_that_does_not_exist" });
        assert_eq!(
            unavailable_reason(&public_response_from_record(&record, None, Vec::new())),
            "metadata_only_persistence"
        );
    }

    /// The same record, with the mode written the way production writes it.
    ///
    /// [`completed_record`] takes a string, which is convenient and hides a coupling: the
    /// production path builds `output_summary` with `json!({ "persistence_mode": <enum> })`, so
    /// the key's value comes from `ResponsePersistenceMode`'s **serde** representation. A test
    /// that hardcodes `"plain_content"` keeps passing if someone changes the `rename_all` on the
    /// enum, while the live endpoint starts falling through to the default answer. This helper
    /// serialises the enum instead, so the two cannot drift apart unnoticed.
    fn completed_record_for(mode: ResponsePersistenceMode) -> PublicResponseRecord {
        let mut record = completed_record("metadata_only");
        record.output_summary = json!({
            "persistence_mode": mode,
            "output_text_bytes": 11,
            "output_hash": "hash",
        });
        record
    }

    /// A fifth persistence mode must not silently inherit the default answer.
    ///
    /// [`every_persistence_mode_gets_its_own_reason`] iterates mode *strings*, so a new enum
    /// variant would fall through `output_unavailable_reason`'s `_` arm, be reported as
    /// metadata-only, and red nothing — the cheapest edit that breaks this finding's property
    /// while leaving every other case here green. The `match` below is **exhaustive over the
    /// enum**, so adding a variant is a compile error in this file. A compiler error is the only
    /// guard that cannot be outrun by a change made somewhere else.
    #[test]
    fn every_persistence_mode_variant_is_mapped_and_reaches_the_reason() {
        for mode in [
            ResponsePersistenceMode::None,
            ResponsePersistenceMode::MetadataOnly,
            ResponsePersistenceMode::EncryptedContent,
            ResponsePersistenceMode::PlainContent,
        ] {
            let expected = match mode {
                ResponsePersistenceMode::None => "persistence_disabled",
                ResponsePersistenceMode::MetadataOnly => "metadata_only_persistence",
                ResponsePersistenceMode::EncryptedContent
                | ResponsePersistenceMode::PlainContent => "content_persistence_not_implemented",
            };
            let response =
                public_response_from_record(&completed_record_for(mode), None, Vec::new());
            assert_eq!(
                unavailable_reason(&response),
                expected,
                "{mode:?} did not reach its reason through the serialised output_summary"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Image-URL admission control (issue #89)
    //
    // The guard itself is proved in `crate::security::ssrf`. What is proved here
    // is the request-level behaviour layered on top of it: one lookup per distinct
    // URL, one budget for the whole request, and one client-visible message that
    // does not turn a 422 into an internal-network oracle.
    // -----------------------------------------------------------------------

    struct CountingResolver {
        answer: Vec<std::net::SocketAddr>,
        delay: Option<Duration>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingResolver {
        fn returning(address: &str) -> Self {
            Self {
                answer: vec![std::net::SocketAddr::new(
                    address.parse().expect("test address must parse"),
                    443,
                )],
                delay: None,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn slow(delay: Duration) -> Self {
            Self {
                delay: Some(delay),
                ..Self::returning("93.184.216.34")
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl HostResolver for CountingResolver {
        async fn resolve(
            &self,
            _host: &str,
            _port: u16,
        ) -> std::io::Result<Vec<std::net::SocketAddr>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            Ok(self.answer.clone())
        }
    }

    fn image_settings() -> ImageUrlSettings {
        ImageUrlSettings::default()
    }

    /// The whole client-visible payload, with the request id pinned so the only
    /// differences left between two of these are differences the caller could actually
    /// use to tell two refusals apart.
    fn message_of(error: &AppError) -> String {
        format!(
            "{:?}",
            error.error_response(Some("fixed-request-id".to_string()))
        )
    }

    /// A caller may repeat the same image URL across messages. Resolving it once per
    /// occurrence would let one request buy `maximum_image_count` lookups of a hostname
    /// the attacker controls, which is a free amplifier pointed at someone else's DNS.
    #[tokio::test]
    async fn repeated_image_urls_are_resolved_once_per_request() {
        let resolver = CountingResolver::returning("93.184.216.34");
        let url = "https://images.example.com/a.png";
        validate_image_urls_with(&image_settings(), vec![url, url, url], &resolver)
            .await
            .expect("a public host must be accepted");
        assert_eq!(
            resolver.calls(),
            1,
            "three occurrences of one URL must cost one lookup"
        );
    }

    #[tokio::test]
    async fn distinct_image_urls_are_each_resolved() {
        let resolver = CountingResolver::returning("93.184.216.34");
        validate_image_urls_with(
            &image_settings(),
            vec!["https://a.example.com/1.png", "https://b.example.com/2.png"],
            &resolver,
        )
        .await
        .expect("public hosts must be accepted");
        assert_eq!(
            resolver.calls(),
            2,
            "de-duplication must not collapse genuinely different hosts"
        );
    }

    /// Each host resolves within the per-host budget, but together they exceed the
    /// request-wide one. Without the request-wide deadline this request costs
    /// `images * dns_timeout_ms` instead of `total_validation_timeout_ms`.
    #[tokio::test]
    async fn many_slow_hosts_are_bounded_by_the_request_wide_budget() {
        let settings = ImageUrlSettings {
            dns_timeout_ms: 5_000,
            total_validation_timeout_ms: 150,
            ..image_settings()
        };
        let resolver = CountingResolver::slow(Duration::from_millis(100));
        let started = std::time::Instant::now();
        let error = validate_image_urls_with(
            &settings,
            vec![
                "https://a.example.com/1.png",
                "https://b.example.com/2.png",
                "https://c.example.com/3.png",
                "https://d.example.com/4.png",
                "https://e.example.com/5.png",
            ],
            &resolver,
        )
        .await
        .expect_err("the request-wide budget must fire");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the request budget, not the sum of the per-host budgets, must bound this"
        );
        assert!(
            message_of(&error).contains("image_url_not_allowed"),
            "budget exhaustion still answers with the catalogued code: {}",
            message_of(&error)
        );
    }

    /// The core non-leak property. A resolved internal address is a fact about the
    /// deployment's network and must not be reconstructable from the response.
    #[tokio::test]
    async fn a_refused_image_url_never_names_the_address_it_resolved_to() {
        let resolver = CountingResolver::returning("169.254.169.254");
        let error = validate_image_urls_with(
            &image_settings(),
            vec!["https://images.example.com/a.png"],
            &resolver,
        )
        .await
        .expect_err("a host resolving to the metadata endpoint must be refused");
        let body = message_of(&error);
        assert!(
            !body.contains("169.254.169.254"),
            "the resolved address must not reach the caller: {body}"
        );
        assert!(
            body.contains("image_url_not_allowed"),
            "the catalogued code is preserved: {body}"
        );
    }

    /// The property that makes the 422 useless as a scanner: two different
    /// address-space outcomes must be indistinguishable to the caller. Any change that
    /// reintroduces a per-reason message — however well meant — fails here.
    #[tokio::test]
    async fn address_space_refusals_are_indistinguishable_to_the_caller() {
        let private = validate_image_urls_with(
            &image_settings(),
            vec!["https://10.0.0.7/a.png"],
            &CountingResolver::returning("93.184.216.34"),
        )
        .await
        .expect_err("a private literal must be refused");

        let off_list = validate_image_urls_with(
            &ImageUrlSettings {
                allowed_hosts: vec!["allowed.example.com".to_string()],
                ..image_settings()
            },
            vec!["https://images.example.com/a.png"],
            &CountingResolver::returning("93.184.216.34"),
        )
        .await
        .expect_err("a host off the allow-list must be refused");

        assert_eq!(
            message_of(&private),
            message_of(&off_list),
            "a caller must not be able to tell 'private address' from 'not allow-listed'"
        );
    }

    /// The counterpart: a mistake in the caller's own string is still named precisely,
    /// because saying so tells the caller nothing it did not already send.
    #[tokio::test]
    async fn a_scheme_mistake_is_still_reported_precisely() {
        let error = validate_image_urls_with(
            &image_settings(),
            vec!["http://images.example.com/a.png"],
            &CountingResolver::returning("93.184.216.34"),
        )
        .await
        .expect_err("http must be refused");
        assert!(
            message_of(&error).contains("https"),
            "a scheme mistake is worth naming: {}",
            message_of(&error)
        );
    }

    /// A request with no images must not touch the resolver at all.
    #[tokio::test]
    async fn a_request_without_images_costs_no_resolution() {
        let resolver = CountingResolver::returning("93.184.216.34");
        validate_image_urls_with(&image_settings(), Vec::new(), &resolver)
            .await
            .expect("an image-free request is valid");
        assert_eq!(resolver.calls(), 0);
    }
}
