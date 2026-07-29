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
    },
    domain::{
        ApplicationExecutionPolicyPutRequest, ApplicationExecutionPolicyRecord, AuditLogInsert,
        AuditResult, CallerRuntimeIdentity, DomainMessage, DomainMessageContent, DomainMessageRole,
        ExecutionCommand, ExecutionFailure, ExecutionFailureClass, ExecutionOptions,
        ExecutionOutcome, ExecutionQuery, ExecutionStatus, IdempotencyRecord, ListResponse,
        OpenAiResponseCompatRequest, PublicCapabilities, PublicContentPart, PublicConversationRef,
        PublicExecutionSummary, PublicInputMessage, PublicMessageRole, PublicModelRef,
        PublicModelResource, PublicOutputContentPart, PublicOutputItem, PublicResponse,
        PublicResponseFormat, PublicResponseRecord, PublicResponseRequest, PublicResponseStatus,
        PublicRouteRef, PublicRouteResource, PublicSseEnvelope, PublicUsageRecord,
        PublicUsageSummary, RuntimeEventEnvelope, RuntimeEventType, UsageQuery,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, IdempotencyClaim, PgAdminRepository, PgPublicRepository, PublicAccess,
        PublicRepository, ResponseStartedInsert, ResponseTerminalUpdate,
        default_application_execution_policy, idempotency_record,
    },
    security::{Actor, ActorType, IdempotencyHasher, secret_fingerprint},
};

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
        let prepared = self
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
                idempotency_request.conversation.as_ref(),
                &idempotency_request.input,
            )
            .await?;
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
                let public = public_response_from_record(&record, outcome.output_text.clone());
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
        let prepared = self
            .prepare_execution(&actor, &ctx, request, policy.clone(), application_id, true)
            .await?;
        let conversation_service = ConversationService::new(&self.state)?;
        let conversation_link = conversation_service
            .prepare_response_conversation(
                &actor,
                &ctx,
                conversation_request.conversation.as_ref(),
                &conversation_request.input,
            )
            .await?;
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
        Ok(public_response_from_record(&record, None))
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
        self.public_repo
            .list_executions_authorized(&access, query)
            .await
            .map(ListResponse::new)
    }

    pub async fn list_usage(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        query: &UsageQuery,
    ) -> Result<ListResponse<PublicUsageRecord>, AppError> {
        self.state.authz.require(actor, "moira:usage:read")?;
        let access = public_access(actor, can_read_all(actor, "moira:usage:read", &self.state))?;
        let records = self
            .public_repo
            .list_usage_authorized(&access, query)
            .await?;
        self.audit(
            actor,
            ctx,
            "usage.read",
            AuditResult::Success,
            None,
            json!({ "count": records.len() }),
        )
        .await?;
        Ok(ListResponse::new(records))
    }

    pub async fn list_models(
        &self,
        actor: &Actor,
    ) -> Result<ListResponse<PublicModelResource>, AppError> {
        self.state.authz.require(actor, "moira:models:read")?;
        let access = public_access(actor, false)?;
        self.public_repo
            .list_visible_models(&access, 200)
            .await
            .map(ListResponse::new)
    }

    pub async fn list_routes(
        &self,
        actor: &Actor,
    ) -> Result<ListResponse<PublicRouteResource>, AppError> {
        self.state.authz.require(actor, "moira:routes:read")?;
        let access = public_access(actor, false)?;
        self.public_repo
            .list_visible_routes(&access, 200)
            .await
            .map(ListResponse::new)
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
            response_format: PublicResponseFormat::Text,
            tools: Vec::new(),
            tool_choice: None,
            metadata: request.metadata,
            seed: None,
        })
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
        validate_request(&self.state, actor, &request, &policy, stream)?;
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
            PublicResponseFormat::JsonObject => {
                required_capabilities.push("structured_output".to_string());
                Some(json!({ "type": "object" }))
            }
            PublicResponseFormat::JsonSchema { schema, .. } => {
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
        let actor_fingerprint = crate::application::admin::actor_fingerprint(actor);
        let hasher = &self.state.idempotency_hasher;
        let request_bytes = normalized_request_bytes(&(application_id, request))?;

        // Pre-claim sweep, deliberately *before* the claim insert, over every historical
        // spelling of this row's position in the unique index
        // (idempotency_key_hash, actor_fingerprint, operation). Two of its three columns
        // have been redefined under deployed rows:
        //
        //   * `idempotency_key_hash` — unkeyed SHA-256 → keyed HMAC (plan 03, P1-1). The
        //     current hash needs no pre-claim probe: the claim's `on conflict` finds it.
        //   * `actor_fingerprint` — this module's 4-field formula → the crate-wide 10-field
        //     one (plan 06, Module 16 / P2-15). Both key hashes must be probed under the
        //     legacy value, because a pre-plan-06 row may carry either.
        //
        // Skipping the sweep is not a slow replay, it is a *wrong* one: an unswept row sits
        // at a different point of the unique index, so the claim below would insert
        // alongside it rather than conflict with it, and `/v1/responses` would execute a
        // second time against the provider — a contractual replay turned into a duplicate
        // billable call.
        //
        // Cost: up to three indexed lookups per idempotent request, on the miss path only.
        // The write below always uses the current pair, so the legacy value can never
        // re-enter the ledger and the sweep drains as rows expire.
        //
        // TODO(post-deploy): drop the two `legacy_actor_fingerprint` probes once every ledger
        // row written before plan 06 shipped has expired. `idempotency_record` sets
        // `expires_at` 24h ahead, so the earliest safe removal date is the deploy date of
        // Module 16 + 1 day. The plan-03 `legacy_hash` probe has its own, earlier window.
        //
        // Gated on a DEPLOY, not on a merge, and deliberately not owned by any plan — see the
        // matching note in `runtime_admin.rs`.
        let legacy_actor_fingerprint = legacy_public_actor_fingerprint(actor, application_id);
        let legacy_key_hash = hasher.legacy_hash(key.as_bytes());
        let current_key_hash = hasher.hash(key.as_bytes());
        let candidates = [
            (&legacy_key_hash, &actor_fingerprint),
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

fn validate_request(
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
    if matches!(
        request.response_format,
        PublicResponseFormat::JsonObject | PublicResponseFormat::JsonSchema { .. }
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
    validate_content(state, request, policy)
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

fn validate_content(
    state: &AppState,
    request: &PublicResponseRequest,
    policy: &ApplicationExecutionPolicyRecord,
) -> Result<(), AppError> {
    let mut image_count = 0usize;
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
                    validate_image_url(image_url)?;
                }
            }
        }
    }
    Ok(())
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

fn validate_image_url(value: &str) -> Result<(), AppError> {
    let url = url::Url::parse(value)
        .map_err(|_| AppError::unprocessable("image_url_not_allowed", "image URL is invalid"))?;
    if url.scheme() != "https" {
        return Err(AppError::unprocessable(
            "image_url_not_allowed",
            "image URL must use https",
        ));
    }
    if url.username() != "" || url.password().is_some() {
        return Err(AppError::unprocessable(
            "image_url_not_allowed",
            "image URL credentials are not allowed",
        ));
    }
    if matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1")
    ) {
        return Err(AppError::unprocessable(
            "image_url_not_allowed",
            "local image URLs are not allowed",
        ));
    }
    Ok(())
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

fn public_response_from_record(
    record: &PublicResponseRecord,
    output_text: Option<String>,
) -> PublicResponse {
    let output = if let Some(text) = output_text {
        vec![PublicOutputItem::Message {
            role: "assistant".to_string(),
            content: vec![PublicOutputContentPart::OutputText { text }],
        }]
    } else if record.status == PublicResponseStatus::Completed && !record.output_persisted {
        vec![PublicOutputItem::Message {
            role: "assistant".to_string(),
            content: vec![PublicOutputContentPart::OutputUnavailable {
                reason: "metadata_only_persistence".to_string(),
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
        // Always empty: RAG retrieval is not wired into response generation (see plans/11-rag-memory-intelligence.md).
        citations: Vec::new(),
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
        ExecutionFailureClass::NoEligibleModel
        | ExecutionFailureClass::ModelNotFound
        | ExecutionFailureClass::RouteNotFound
        | ExecutionFailureClass::CredentialNotFound => StatusCode::NOT_FOUND,
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

    #[test]
    fn metadata_rejects_secret_like_keys() {
        let state = AppState::new(crate::config::Settings::default(), None).unwrap();
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
            assert_ne!(
                crate::application::admin::actor_fingerprint(&base),
                crate::application::admin::actor_fingerprint(&variant),
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
        assert_ne!(
            crate::application::admin::actor_fingerprint(&base),
            crate::application::admin::actor_fingerprint(&other),
            "application identity must still partition the ledger without the argument"
        );
        assert_ne!(
            legacy_public_actor_fingerprint(&base, effective_application_id(&base)),
            legacy_public_actor_fingerprint(&other, effective_application_id(&other)),
        );
    }
}
