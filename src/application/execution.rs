use std::{future::Future, sync::Arc, time::Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use rig_core::completion::CompletionRequest;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, warn};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::RequestContext,
    domain::{
        AgentProfileRecord, AgentProfileResolution, AttemptStatus, AuditLogInsert, AuditResult,
        CallerRuntimeIdentity, CredentialDecision, DiagnosticExecutionRequest,
        DiagnosticExecutionResponse, DomainMessage, EffectiveExecutionPolicy, ExecutionCommand,
        ExecutionFailure, ExecutionFailureClass, ExecutionOutcome, ExecutionStatus,
        ExecutionStreamHandle, ModelCandidate, ModelDecision, ModelSelectionReason,
        ProviderAttemptSummary, ProviderRuntimePolicyRecord, ProviderType, ResolvedCredential,
        ResolvedProviderConfiguration, RouteDecision, RouteSelectionReason, RuntimeEventEnvelope,
        RuntimeEventType, UsageSummary,
    },
    error::AppError,
    infra::{
        metrics::{MetricsRegistry, provider_type_label},
        pg_rows::{credential_type_to_db, scope_type_to_db},
        repositories::{
            AdminRepository, ExecutionAttemptInsert, ExecutionAttemptUpdate, PgAdminRepository,
            PgRuntimeRepository, RuntimeRepository, UsageRecordInsert,
        },
    },
    orchestration::{
        RigRuntimeFactory, RuntimeCacheKey, RuntimeFactory, RuntimeModelHandle, RuntimeStreamItem,
        rig_chat_history,
    },
    security::{Actor, ActorType, CredentialAadParts, SecretCipher, credential_aad},
};

#[async_trait]
pub trait ExecutionService: Send + Sync {
    async fn execute(&self, command: ExecutionCommand) -> Result<ExecutionOutcome, AppError>;
    async fn execute_stream(
        &self,
        command: ExecutionCommand,
    ) -> Result<ExecutionStreamHandle, AppError>;
}

#[derive(Clone)]
pub struct MoiraExecutionService {
    state: AppState,
    runtime_repo: PgRuntimeRepository,
    admin_repo: PgAdminRepository,
    factory: RigRuntimeFactory,
}

impl MoiraExecutionService {
    pub fn new(state: AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            runtime_repo: PgRuntimeRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool),
            factory: RigRuntimeFactory::new(),
        })
    }

    pub fn command_from_diagnostic(
        actor: &Actor,
        ctx: &RequestContext,
        request: DiagnosticExecutionRequest,
    ) -> ExecutionCommand {
        let mut options = request.options;
        options.stream = request.stream;
        ExecutionCommand {
            request_id: ctx.request_id.clone(),
            execution_id: Uuid::now_v7(),
            identity: caller_identity_from_actor(actor),
            application_id: request.application_id.or(actor.internal_application_id),
            external_tenant_id: request
                .external_tenant_id
                .or_else(|| actor.external_tenant_id.clone().or(actor.tenant_id.clone())),
            external_user_id: request
                .external_user_id
                .or_else(|| actor.external_user_id.clone().or(actor.subject.clone())),
            messages: vec![DomainMessage::user(request.prompt)],
            route_hint: request.route,
            provider_hint: request.provider_id,
            model_hint: request.provider_model_id,
            credential_hint: request.credential_id,
            options,
            metadata: request.metadata,
        }
    }

    pub async fn execute_with_events(
        &self,
        command: ExecutionCommand,
    ) -> Result<(ExecutionOutcome, Vec<RuntimeEventEnvelope>), AppError> {
        let mut events = EventCollector::new(&command);
        events.push(RuntimeEventType::ExecutionStarted, json!({}));
        let result = self.execute_inner(command, &mut events).await?;
        Ok((result, events.into_events()))
    }

    async fn execute_inner(
        &self,
        mut command: ExecutionCommand,
        events: &mut EventCollector,
    ) -> Result<ExecutionOutcome, AppError> {
        let total_timeout_ms = command
            .options
            .timeout_ms
            .unwrap_or(
                self.state
                    .settings
                    .runtime
                    .default_execution_timeout_seconds
                    * 1_000,
            )
            .min(
                self.state
                    .settings
                    .runtime
                    .maximum_execution_timeout_seconds
                    * 1_000,
            );
        let execution_deadline = Instant::now() + Duration::from_millis(total_timeout_ms);
        let mut attempts = Vec::new();
        if let Some(failure) = self.validate_command(&mut command).await? {
            events.push(
                RuntimeEventType::ExecutionFailed,
                json!({ "failure_class": failure.class }),
            );
            return Ok(failed_outcome(command, None, None, attempts, failure));
        }
        self.audit_runtime_event(
            &command,
            "execution.started",
            AuditResult::Success,
            json!({}),
        )
        .await?;

        let policy = match DefaultExecutionPolicyService::new(&self.state)
            .evaluate(&command)
            .await
        {
            Ok(policy) => policy,
            Err(failure) => {
                events.push(
                    RuntimeEventType::ExecutionFailed,
                    json!({ "failure_class": failure.class }),
                );
                return Ok(failed_outcome(command, None, None, attempts, failure));
            }
        };

        events.push(RuntimeEventType::RoutingStarted, json!({}));
        let route = match DefaultTaskRouter::new(&self.runtime_repo)
            .select_route(&command)
            .await
        {
            Ok(route) => route,
            Err(failure) => {
                self.audit_execution(&command, "execution.failed", AuditResult::Failed, &failure)
                    .await?;
                events.push(
                    RuntimeEventType::ExecutionFailed,
                    json!({ "failure_class": failure.class }),
                );
                return Ok(failed_outcome(command, None, None, attempts, failure));
            }
        };
        events.push(
            RuntimeEventType::RouteSelected,
            json!({ "route_id": route.route_id, "route_key": route.route_key, "reason": route.reason }),
        );
        self.audit_runtime_event(
            &command,
            "routing.completed",
            AuditResult::Success,
            json!({ "route_id": route.route_id, "route_key": route.route_key, "reason": route.reason }),
        )
        .await?;

        // F50, decided fail-closed on issue #79. A route that names an agent profile the
        // runtime cannot use is refused here, before any provider is chosen, any credential is
        // decrypted and any attempt row is written — see `agent_profile_failure` for the
        // reasoning and for why the two answers carry different codes.
        let agent_profile = match route.agent_profile_id {
            Some(id) => {
                let resolution = AgentProfileResolution::classify(
                    self.runtime_repo.find_agent_profile_reference(id).await?,
                );
                match resolution {
                    AgentProfileResolution::Active(profile) => Some(*profile),
                    unusable => {
                        let failure = agent_profile_failure(&route, id, &unusable);
                        self.announce_dangling_agent_profile(
                            &command, &route, id, &unusable, events,
                        )
                        .await?;
                        self.audit_execution(
                            &command,
                            "execution.failed",
                            AuditResult::Failed,
                            &failure,
                        )
                        .await?;
                        events.push(
                            RuntimeEventType::ExecutionFailed,
                            json!({ "failure_class": failure.class }),
                        );
                        return Ok(failed_outcome(
                            command,
                            Some(route),
                            None,
                            attempts,
                            failure,
                        ));
                    }
                }
            }
            None => None,
        };

        let candidates = match DefaultModelRouter::new(&self.runtime_repo, &self.state)
            .select_candidates(&command, &policy, &route)
            .await
        {
            Ok(candidates) => candidates,
            Err(failure) => {
                self.audit_execution(&command, "execution.failed", AuditResult::Failed, &failure)
                    .await?;
                events.push(
                    RuntimeEventType::ExecutionFailed,
                    json!({ "failure_class": failure.class }),
                );
                return Ok(failed_outcome(
                    command,
                    Some(route),
                    None,
                    attempts,
                    failure,
                ));
            }
        };

        let mut last_failure = None;
        let mut total_attempts = 0usize;
        let max_candidates = if policy.allow_fallback {
            policy.max_fallbacks.saturating_add(1)
        } else {
            1
        };

        for candidate in candidates.into_iter().take(max_candidates) {
            let model = ModelDecision {
                policy_id: candidate.policy_id,
                provider_id: candidate.provider_id,
                provider_model_id: candidate.provider_model_id,
                model_key: candidate.model_key.clone(),
                provider_type: candidate.provider_type,
                reason: if command.model_hint == Some(candidate.provider_model_id) {
                    ModelSelectionReason::ExplicitHint
                } else {
                    ModelSelectionReason::Priority
                },
            };
            events.push(
                RuntimeEventType::ModelSelected,
                json!({
                    "provider_id": model.provider_id,
                    "provider_model_id": model.provider_model_id,
                    "model_key": model.model_key,
                    "reason": model.reason
                }),
            );

            // Phase bound: credential resolution (DB round-trip + AES-256-GCM decrypt) must
            // fit inside what is left of the total execution deadline. No attempt row and no
            // permit exist yet, so a breach needs no cleanup beyond the existing error arm.
            let credential = match bounded_phase(
                execution_deadline,
                self.resolve_credential(&command, &candidate),
            )
            .await
            {
                Ok(credential) => credential,
                Err(failure) => {
                    last_failure = Some(failure.clone());
                    if policy.allow_fallback && failure.fallback_eligible {
                        events.push(
                            RuntimeEventType::FallbackSelected,
                            json!({ "from_provider_id": candidate.provider_id, "failure_class": failure.class }),
                        );
                        continue;
                    }
                    return Ok(failed_outcome(
                        command,
                        Some(route),
                        Some(model),
                        attempts,
                        failure,
                    ));
                }
            };

            let provider = ResolvedProviderConfiguration {
                provider_id: candidate.provider_id,
                provider_version: candidate.provider_version,
                provider_type: candidate.provider_type,
                display_name: candidate.provider_display_name.clone(),
                base_url: candidate.base_url.clone(),
            };

            match self
                .state
                .circuits
                .before_call(
                    candidate.provider_id,
                    candidate.provider_model_id,
                    &candidate.runtime_policy,
                )
                .await
            {
                Ok(_) => {}
                Err(failure) => {
                    last_failure = Some(failure.clone());
                    if policy.allow_fallback && failure.fallback_eligible {
                        continue;
                    }
                    return Ok(failed_outcome(
                        command,
                        Some(route),
                        Some(model),
                        attempts,
                        failure,
                    ));
                }
            }

            // Phase bound: runtime construction. A cache miss builds a Rig client, which can
            // block on DNS/TLS setup. Still no attempt row and no permit, so the existing
            // error arm remains the whole cleanup story.
            let handle = match bounded_phase(
                execution_deadline,
                self.runtime_handle(&provider, &candidate, &credential),
            )
            .await
            {
                Ok(handle) => handle,
                Err(failure) => {
                    last_failure = Some(failure.clone());
                    if policy.allow_fallback && failure.fallback_eligible {
                        continue;
                    }
                    return Ok(failed_outcome(
                        command,
                        Some(route),
                        Some(model),
                        attempts,
                        failure,
                    ));
                }
            };

            let runtime_policy = effective_runtime_policy(&policy, &candidate.runtime_policy);
            let mut retries = 0usize;
            loop {
                let Some(remaining) = remaining_execution_time(execution_deadline) else {
                    let failure = deadline_failure();
                    return Ok(failed_outcome(
                        command,
                        Some(route),
                        Some(model),
                        attempts,
                        failure,
                    ));
                };
                if total_attempts >= self.state.settings.runtime.maximum_total_upstream_attempts {
                    let failure = ExecutionFailure::new(
                        ExecutionFailureClass::DeadlineExceeded,
                        "maximum upstream attempts reached",
                    );
                    return Ok(failed_outcome(
                        command,
                        Some(route),
                        Some(model),
                        attempts,
                        failure,
                    ));
                }
                total_attempts += 1;
                let attempt_number = total_attempts as i32;
                let attempt_id = Uuid::now_v7();
                let started = Instant::now();
                self.runtime_repo
                    .insert_attempt_started(&ExecutionAttemptInsert {
                        id: attempt_id,
                        request_id: command.request_id.clone(),
                        execution_id: command.execution_id,
                        attempt_number,
                        application_id: command.application_id,
                        external_tenant_id: command.external_tenant_id.clone(),
                        external_user_id: command.external_user_id.clone(),
                        route_id: route.route_id,
                        provider_id: candidate.provider_id,
                        provider_model_id: candidate.provider_model_id,
                        credential_id: credential.credential.credential_id,
                        metadata: json!({
                            "policy_id": candidate.policy_id,
                            "credential_source": credential.decision.source,
                        }),
                    })
                    .await?;
                events.push(
                    RuntimeEventType::ProviderAttemptStarted,
                    json!({
                        "attempt_id": attempt_id,
                        "attempt_number": attempt_number,
                        "provider_id": candidate.provider_id,
                        "provider_model_id": candidate.provider_model_id
                    }),
                );
                self.audit_runtime_event(
                    &command,
                    "provider.attempt.started",
                    AuditResult::Success,
                    json!({
                        "attempt_id": attempt_id,
                        "attempt_number": attempt_number,
                        "provider_id": candidate.provider_id,
                        "provider_model_id": candidate.provider_model_id
                    }),
                )
                .await?;

                let permits = match self
                    .state
                    .concurrency
                    .acquire(
                        candidate.provider_id,
                        candidate.runtime_policy.max_concurrent_requests.max(1) as usize,
                        command.options.stream,
                        candidate.runtime_policy.max_concurrent_streams.max(1) as usize,
                        command.application_id,
                        command.external_user_id.as_deref(),
                    )
                    .await
                {
                    Ok(permits) => permits,
                    Err(exhaustion) => {
                        let failure: ExecutionFailure = exhaustion.into();
                        self.complete_failed_attempt(
                            attempt_id,
                            started,
                            &failure,
                            UsageSummary::default(),
                            None,
                            json!({}),
                        )
                        .await?;
                        self.audit_runtime_event(
                            &command,
                            "provider.attempt.failed",
                            AuditResult::Failed,
                            json!({ "attempt_id": attempt_id, "failure_class": failure.class }),
                        )
                        .await?;
                        attempts.push(attempt_summary(
                            attempt_id,
                            attempt_number,
                            &candidate,
                            credential.credential.credential_id,
                            Some(failure.class),
                            started,
                            UsageSummary::default(),
                        ));
                        last_failure = Some(failure.clone());
                        break;
                    }
                };

                let request = match build_completion_request(&command, agent_profile.as_ref()) {
                    Ok(request) => request,
                    Err(failure) => {
                        drop(permits);
                        self.complete_failed_attempt(
                            attempt_id,
                            started,
                            &failure,
                            UsageSummary::default(),
                            None,
                            json!({}),
                        )
                        .await?;
                        self.audit_runtime_event(
                            &command,
                            "provider.attempt.failed",
                            AuditResult::Failed,
                            json!({ "attempt_id": attempt_id, "failure_class": failure.class }),
                        )
                        .await?;
                        attempts.push(attempt_summary(
                            attempt_id,
                            attempt_number,
                            &candidate,
                            credential.credential.credential_id,
                            Some(failure.class),
                            started,
                            UsageSummary::default(),
                        ));
                        return Ok(failed_outcome(
                            command,
                            Some(route),
                            Some(model),
                            attempts,
                            failure,
                        ));
                    }
                };

                let cancellation = events.cancellation();
                // Execution-attempt span (plan 05, Module 2). Attached with `Instrument`
                // rather than an `enter()` guard because the attempt body awaits: a guard
                // held across an await point would re-parent whatever else the runtime
                // schedules onto this thread.
                //
                // Attributes are an explicit whitelist of identifiers and closed-set enum
                // labels — no prompt text, no request or response body, no credential
                // material, and nothing `Debug`-formatted. `provider_type` reuses
                // `provider_type_label` so the span attribute and the metric label cannot
                // drift apart.
                let attempt_span = tracing::debug_span!(
                    "execution_attempt",
                    attempt_id = %attempt_id,
                    attempt_number,
                    execution_id = %command.execution_id,
                    provider_id = %candidate.provider_id,
                    provider_model_id = %candidate.provider_model_id,
                    provider_type = provider_type_label(candidate.provider_type),
                    model_key = %candidate.model_key,
                    stream = command.options.stream,
                );
                let execution = async {
                    if command.options.stream {
                        execute_rig_stream(
                            handle.clone(),
                            request,
                            events,
                            Duration::from_millis(
                                candidate.runtime_policy.stream_idle_timeout_ms.max(1) as u64,
                            ),
                            StreamMetricsContext {
                                metrics: &self.state.metrics,
                                provider_type: candidate.provider_type,
                                attempt_started: started,
                            },
                        )
                        .await
                    } else {
                        execute_rig_completion(handle.clone(), request).await
                    }
                }
                .instrument(attempt_span);
                let attempt_timeout =
                    phase_budget(remaining, Duration::from_millis(runtime_policy.timeout_ms));
                let bounded_by_total_deadline =
                    attempt_timeout < Duration::from_millis(runtime_policy.timeout_ms);
                let result = tokio::select! {
                    _ = cancellation.cancelled() => Ok(Err(cancelled_failure().into())),
                    result = tokio::time::timeout(attempt_timeout, execution) => result,
                };
                drop(permits);

                match result {
                    Ok(Ok(output)) => {
                        let latency_ms = elapsed_ms(started);
                        // Captured on the same basis as `latency_ms` — i.e. the provider call
                        // itself — so the histogram is not inflated by the terminal
                        // persistence writes that follow.
                        let attempt_latency = started.elapsed();
                        // Phase bound: terminal persistence. Unlike the two phases above, an
                        // attempt row already exists in `started` AND the provider call has
                        // already succeeded, so the three writes are bounded as one logical
                        // unit and a breach is reported as its own audited condition instead
                        // of being folded into a plain deadline failure.
                        let terminal_persistence = async {
                            self.runtime_repo
                                .update_attempt(
                                    attempt_id,
                                    &ExecutionAttemptUpdate {
                                        status: AttemptStatus::Succeeded,
                                        failure_class: None,
                                        provider_status_code: None,
                                        latency_ms: Some(latency_ms),
                                        usage: output.usage.clone(),
                                        provider_request_id: output.provider_request_id.clone(),
                                        metadata: json!({}),
                                    },
                                )
                                .await?;
                            self.runtime_repo
                                .insert_usage_record(&UsageRecordInsert {
                                    id: Uuid::now_v7(),
                                    request_id: command.request_id.clone(),
                                    execution_id: command.execution_id,
                                    attempt_id,
                                    application_id: command.application_id,
                                    external_tenant_id: command.external_tenant_id.clone(),
                                    external_user_id: command.external_user_id.clone(),
                                    provider_id: candidate.provider_id,
                                    provider_model_id: candidate.provider_model_id,
                                    credential_id: credential.credential.credential_id,
                                    usage: output.usage.clone(),
                                    metadata: json!({ "cost_estimation": "unavailable" }),
                                })
                                .await?;
                            self.runtime_repo
                                .touch_credential_used(credential.credential.credential_id)
                                .await?;
                            Ok::<(), AppError>(())
                        };
                        let persisted = tokio::time::timeout(
                            terminal_persistence_budget(execution_deadline),
                            terminal_persistence,
                        )
                        .await;
                        match persisted {
                            Ok(result) => result?,
                            Err(_) => {
                                let failure = terminal_persistence_deadline_failure();
                                // **What "committed" means here, and why it is read rather
                                // than asserted (finding F38).**
                                //
                                // `EventCollector::output_committed` is this module's own
                                // definition of the word: it flips on the first chunk that is
                                // *accepted by the consumer*, so it is `true` on a streamed
                                // answer the caller has already seen and `false` on every
                                // non-streaming execution, where the caller receives nothing
                                // but the outcome. It says nothing about the database — the
                                // three terminal writes are precisely what timed out.
                                //
                                // This used to be the literal `true` in both the audit entry
                                // and the `ExecutionFailed` event, which was wrong in both
                                // available senses on the non-streaming path: nothing had
                                // reached the caller, and the write group had not completed.
                                // The `tracing` line below always said "may", and it was the
                                // honest one.
                                let delivered_to_caller = events.output_committed();
                                tracing::error!(
                                    request_id = %command.request_id,
                                    execution_id = %command.execution_id,
                                    attempt_id = %attempt_id,
                                    provider_id = %candidate.provider_id,
                                    provider_model_id = %candidate.provider_model_id,
                                    latency_ms,
                                    delivered_to_caller,
                                    "terminal persistence exceeded the execution deadline after a successful provider call; output may already be committed"
                                );
                                // The database is by definition slow at this point, so the
                                // audit write is itself bounded and best-effort: it must not
                                // become a second unbounded await on the way out.
                                let audit = self.audit_runtime_event(
                                    &command,
                                    "execution.terminal_persistence_deadline_exceeded",
                                    AuditResult::Failed,
                                    json!({
                                        "attempt_id": attempt_id,
                                        "attempt_number": attempt_number,
                                        "provider_id": candidate.provider_id,
                                        "provider_model_id": candidate.provider_model_id,
                                        "latency_ms": latency_ms,
                                        "failure_class": failure.class,
                                        "output_committed": delivered_to_caller,
                                        "terminal_state_persisted": false
                                    }),
                                );
                                match tokio::time::timeout(TERMINAL_PERSISTENCE_AUDIT_BUDGET, audit)
                                    .await
                                {
                                    Ok(Ok(())) => {}
                                    Ok(Err(err)) => tracing::error!(
                                        error = %err,
                                        "failed to record the terminal-persistence deadline audit entry"
                                    ),
                                    Err(_) => tracing::error!(
                                        "recording the terminal-persistence deadline audit entry timed out"
                                    ),
                                }
                                attempts.push(attempt_summary(
                                    attempt_id,
                                    attempt_number,
                                    &candidate,
                                    credential.credential.credential_id,
                                    Some(failure.class),
                                    started,
                                    output.usage.clone(),
                                ));
                                events.push(
                                    RuntimeEventType::ExecutionFailed,
                                    json!({
                                        "failure_class": failure.class,
                                        "phase": "terminal_persistence",
                                        "output_committed": delivered_to_caller,
                                        "terminal_state_persisted": false
                                    }),
                                );
                                // **Finding F38 — the outcome keeps what the provider produced.**
                                //
                                // This is the one arm reached from `Ok(Ok(output))` that does
                                // not return `Succeeded`, and it used to call `failed_outcome`,
                                // which hardcodes `output_text: None`, `structured_output: None`
                                // and `usage: UsageSummary::default()`. Three values that were
                                // live in `output` were dropped, while the `attempt_summary`
                                // pushed a few statements above recorded the usage in full. The
                                // outcome and its own `attempts` array contradicted each other
                                // inside a single serialised document; that asymmetry was the
                                // whole finding.
                                //
                                // **Decision: report the provider's result, keep the failure.**
                                // `status` stays `Failed` and the retry/fallback clamp is
                                // untouched — terminal state genuinely did not persist and this
                                // execution must never be re-run. But `output_text`,
                                // `structured_output` and `usage` are facts about a provider
                                // call that *succeeded*, and discarding them made the failure
                                // report less true, not safer:
                                //
                                // - `UsageSummary::default()` is all-`None`, i.e. "unknown",
                                //   not "zero". Retaining the real counts replaces an absence
                                //   of information with information; it does not overwrite a
                                //   deliberate claim that nothing was spent.
                                // - `terminal_update_from_outcome` in `application/public.rs`
                                //   runs on the `Failed` branch too and copies `usage`,
                                //   `output_text.len()` and the output hash straight onto the
                                //   `responses` row. Dropping them wrote "no tokens, zero
                                //   bytes, no hash" for a request the provider answered and
                                //   will invoice. Retaining them makes that row faithful, and
                                //   makes `responses.usage` populated while `usage_records` has
                                //   no row the detectable signature of exactly this condition —
                                //   which is otherwise invisible.
                                // - On the streaming path the caller has already received every
                                //   delta. An outcome reporting no output described something
                                //   the caller could see had not happened.
                                //
                                // **What this deliberately does not do.** No `usage_records`
                                // row is written, so nothing is double-counted; billing that
                                // reads `usage_records` still under-counts this execution, and
                                // that under-count is now *detectable* rather than silent.
                                //
                                // **Reversal condition.** Zero the usage again the day
                                // `ExecutionOutcome.usage` (or `responses.usage`) becomes an
                                // input to customer invoicing rather than a reporting surface.
                                // Today its only readers are `terminal_update_from_outcome` and
                                // the runtime diagnostic endpoint, both reporting; invoicing
                                // reads `usage_records`. Once a billing job sums the response
                                // rows, a `Failed` response carrying usage would charge a
                                // caller who received an HTTP error, and the deployment must
                                // then decide explicitly whether this arm is chargeable instead
                                // of inheriting the answer from a struct literal.
                                //
                                // **Second reversal condition, for the text.** Two consumers —
                                // `ConversationService::run_extraction` and
                                // `summarize_conversation` — infer "the model answered" from
                                // `output_text`/`structured_output` being `Some`, never from
                                // `status`. Retaining the text therefore lets both proceed on a
                                // reply that is genuinely present, which is the correct answer
                                // to the question they are asking. If a *delivery* path (rather
                                // than an interpretation path) ever starts treating
                                // `output_text.is_some()` as "show this to the end user" without
                                // checking `status`, this arm must stop carrying text and the
                                // two inference sites must read `status` instead — which is also
                                // the third precondition of F29's own reversal condition.
                                return Ok(ExecutionOutcome {
                                    request_id: command.request_id,
                                    execution_id: command.execution_id,
                                    status: execution_status_for_failure(failure.class),
                                    output_text: Some(output.text),
                                    structured_output: output.structured_output,
                                    usage: output.usage,
                                    route: Some(route),
                                    model: Some(model),
                                    attempts,
                                    failure: Some(failure),
                                });
                            }
                        }
                        self.state
                            .circuits
                            .on_success(candidate.provider_id, candidate.provider_model_id)
                            .await;
                        // Additive side-effect recording, in the same position and spirit as
                        // the circuit-breaker call above: no control flow depends on it.
                        self.state.metrics.record_execution_latency(
                            candidate.provider_type,
                            ExecutionStatus::Succeeded,
                            None,
                            attempt_latency,
                        );
                        self.state.metrics.record_provider_outcome(
                            candidate.provider_type,
                            &candidate.model_key,
                            ExecutionStatus::Succeeded,
                            None,
                        );
                        self.audit_runtime_event(
                            &command,
                            "provider.attempt.completed",
                            AuditResult::Success,
                            json!({ "attempt_id": attempt_id, "latency_ms": latency_ms }),
                        )
                        .await?;
                        attempts.push(attempt_summary(
                            attempt_id,
                            attempt_number,
                            &candidate,
                            credential.credential.credential_id,
                            None,
                            started,
                            output.usage.clone(),
                        ));
                        for event in output.events {
                            events.push_existing(event);
                        }
                        events.push(
                            RuntimeEventType::ExecutionCompleted,
                            json!({ "attempt_id": attempt_id }),
                        );
                        self.audit_execution_success(&command).await?;
                        return Ok(ExecutionOutcome {
                            request_id: command.request_id,
                            execution_id: command.execution_id,
                            status: ExecutionStatus::Succeeded,
                            output_text: Some(output.text),
                            structured_output: output.structured_output,
                            usage: output.usage,
                            route: Some(route),
                            model: Some(model),
                            attempts,
                            failure: None,
                        });
                    }
                    Ok(Err(FailedAttempt { failure, usage })) => {
                        self.state
                            .circuits
                            .on_failure(
                                candidate.provider_id,
                                candidate.provider_model_id,
                                &candidate.runtime_policy,
                                failure.class,
                            )
                            .await;
                        self.state.metrics.record_execution_latency(
                            candidate.provider_type,
                            execution_status_for_failure(failure.class),
                            Some(failure.class),
                            started.elapsed(),
                        );
                        self.state.metrics.record_provider_outcome(
                            candidate.provider_type,
                            &candidate.model_key,
                            execution_status_for_failure(failure.class),
                            Some(failure.class),
                        );
                        self.complete_failed_attempt(
                            attempt_id,
                            started,
                            &failure,
                            usage.clone(),
                            None,
                            json!({}),
                        )
                        .await?;
                        // **A billed provider call is metered even when its reply is refused
                        // (issue #80 review).** `usage` is all-`None` for every failure raised
                        // before or instead of a complete reply, and the row is skipped then —
                        // the shape this arm has always had. It is populated only where the
                        // provider answered, was charged for answering, and Moira then refused
                        // the answer: without this row that request burns provider tokens with
                        // nothing in `usage_records`, on a path a caller reaches by sending a
                        // schema to a backend that does not honour it. Before the flip the same
                        // request succeeded and was metered here-equivalent by the success arm,
                        // so this restores the row rather than adding one.
                        //
                        // Written *after* the attempt row is completed, so the foreign key it
                        // carries always resolves, and unbounded like the neighbouring writes in
                        // this arm rather than under the success arm's terminal-persistence
                        // budget: there is no committed output to protect here.
                        if usage_was_reported(&usage) {
                            self.runtime_repo
                                .insert_usage_record(&UsageRecordInsert {
                                    id: Uuid::now_v7(),
                                    request_id: command.request_id.clone(),
                                    execution_id: command.execution_id,
                                    attempt_id,
                                    application_id: command.application_id,
                                    external_tenant_id: command.external_tenant_id.clone(),
                                    external_user_id: command.external_user_id.clone(),
                                    provider_id: candidate.provider_id,
                                    provider_model_id: candidate.provider_model_id,
                                    credential_id: credential.credential.credential_id,
                                    usage: usage.clone(),
                                    // The failure class travels with the row so a billing job
                                    // can tell a metered refusal from a metered answer without
                                    // joining back to `execution_attempts`.
                                    metadata: json!({
                                        "cost_estimation": "unavailable",
                                        "attempt_outcome": "failed",
                                        "failure_class": failure.class
                                    }),
                                })
                                .await?;
                        }
                        attempts.push(attempt_summary(
                            attempt_id,
                            attempt_number,
                            &candidate,
                            credential.credential.credential_id,
                            Some(failure.class),
                            started,
                            usage,
                        ));
                        events.push(
                            RuntimeEventType::ProviderAttemptFailed,
                            json!({ "attempt_id": attempt_id, "failure_class": failure.class }),
                        );
                        self.audit_runtime_event(
                            &command,
                            "provider.attempt.failed",
                            AuditResult::Failed,
                            json!({ "attempt_id": attempt_id, "failure_class": failure.class }),
                        )
                        .await?;
                        last_failure = Some(failure.clone());
                        if failure.retryable && retries < runtime_policy.max_retries {
                            retries += 1;
                            match sleep_for_retry(
                                retries,
                                &candidate.runtime_policy,
                                execution_deadline,
                                &cancellation,
                            )
                            .await
                            {
                                RetryWait::Ready => continue,
                                RetryWait::Deadline => last_failure = Some(deadline_failure()),
                                RetryWait::Cancelled => last_failure = Some(cancelled_failure()),
                            }
                        }
                        break;
                    }
                    Err(_) => {
                        let failure = attempt_timeout_failure(
                            bounded_by_total_deadline
                                || remaining_execution_time(execution_deadline).is_none(),
                            events.output_committed(),
                        );
                        self.state
                            .circuits
                            .on_failure(
                                candidate.provider_id,
                                candidate.provider_model_id,
                                &candidate.runtime_policy,
                                failure.class,
                            )
                            .await;
                        self.state.metrics.record_execution_latency(
                            candidate.provider_type,
                            execution_status_for_failure(failure.class),
                            Some(failure.class),
                            started.elapsed(),
                        );
                        self.state.metrics.record_provider_outcome(
                            candidate.provider_type,
                            &candidate.model_key,
                            execution_status_for_failure(failure.class),
                            Some(failure.class),
                        );
                        self.complete_failed_attempt(
                            attempt_id,
                            started,
                            &failure,
                            UsageSummary::default(),
                            None,
                            json!({ "timeout_ms": runtime_policy.timeout_ms }),
                        )
                        .await?;
                        attempts.push(attempt_summary(
                            attempt_id,
                            attempt_number,
                            &candidate,
                            credential.credential.credential_id,
                            Some(failure.class),
                            started,
                            UsageSummary::default(),
                        ));
                        last_failure = Some(failure.clone());
                        if failure.retryable && retries < runtime_policy.max_retries {
                            retries += 1;
                            match sleep_for_retry(
                                retries,
                                &candidate.runtime_policy,
                                execution_deadline,
                                &cancellation,
                            )
                            .await
                            {
                                RetryWait::Ready => continue,
                                RetryWait::Deadline => last_failure = Some(deadline_failure()),
                                RetryWait::Cancelled => last_failure = Some(cancelled_failure()),
                            }
                        }
                        break;
                    }
                }
            }

            if let Some(failure) = &last_failure
                && (!policy.allow_fallback || !failure.fallback_eligible)
            {
                self.audit_execution(&command, "execution.failed", AuditResult::Failed, failure)
                    .await?;
                return Ok(failed_outcome(
                    command,
                    Some(route),
                    Some(model),
                    attempts,
                    failure.clone(),
                ));
            }
            events.push(
                RuntimeEventType::FallbackSelected,
                json!({ "from_provider_id": candidate.provider_id }),
            );
        }

        let failure = last_failure.unwrap_or_else(|| {
            ExecutionFailure::new(ExecutionFailureClass::NoEligibleModel, "no eligible model")
        });
        self.audit_execution(&command, "execution.failed", AuditResult::Failed, &failure)
            .await?;
        events.push(
            RuntimeEventType::ExecutionFailed,
            json!({ "failure_class": failure.class }),
        );
        Ok(failed_outcome(
            command,
            Some(route),
            None,
            attempts,
            failure,
        ))
    }

    async fn validate_command(
        &self,
        command: &mut ExecutionCommand,
    ) -> Result<Option<ExecutionFailure>, AppError> {
        if command.messages.is_empty() {
            return Ok(Some(ExecutionFailure::new(
                ExecutionFailureClass::InvalidExecutionRequest,
                "execution command must contain at least one Rig message",
            )));
        }
        if command.application_id.is_none() {
            command.application_id = command.identity.application_id;
        }
        if let Some(bound_application_id) = command.identity.application_id
            && let Some(requested_application_id) = command.application_id
            && bound_application_id != requested_application_id
        {
            return Ok(Some(ExecutionFailure::new(
                ExecutionFailureClass::ApplicationUnavailable,
                "caller is not bound to the requested application",
            )));
        }
        if let Some(application_id) = command.application_id {
            match self
                .runtime_repo
                .ensure_application_active(application_id)
                .await
            {
                Ok(()) => {}
                Err(AppError::Forbidden(_)) => {
                    return Ok(Some(ExecutionFailure::new(
                        ExecutionFailureClass::ApplicationUnavailable,
                        "application is unavailable",
                    )));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(None)
    }

    async fn resolve_credential(
        &self,
        command: &ExecutionCommand,
        candidate: &ModelCandidate,
    ) -> Result<RuntimeResolvedCredential, ExecutionFailure> {
        if command.credential_hint.is_some()
            && !has_runtime_scope(command, "moira:execution:override-credential")
        {
            return Err(ExecutionFailure::new(
                ExecutionFailureClass::CredentialForbidden,
                "explicit credential override is not authorized",
            ));
        }
        let credential = self
            .runtime_repo
            .resolve_runtime_credential(
                candidate.provider_id,
                supported_credential_types(candidate.provider_type),
                command.application_id,
                command.external_tenant_id.as_deref(),
                command.external_user_id.as_deref(),
                command.credential_hint,
            )
            .await
            .map_err(|_| {
                ExecutionFailure::new(
                    ExecutionFailureClass::CredentialNotFound,
                    "credential lookup failed",
                )
            })?
            .ok_or_else(|| {
                ExecutionFailure::new(
                    if command.credential_hint.is_some() {
                        ExecutionFailureClass::CredentialForbidden
                    } else {
                        ExecutionFailureClass::CredentialNotFound
                    },
                    "no eligible provider credential",
                )
            })?;

        let record = credential.record;
        let aad = credential_aad(CredentialAadParts {
            credential_id: record.id,
            provider_id: record.provider_id,
            credential_type: credential_type_to_db(&record.credential_type),
            scope_type: scope_type_to_db(&record.scope_type),
            external_tenant_id: record.external_tenant_id.as_deref(),
            application_id: record.application_id,
            external_user_id: record.external_user_id.as_deref(),
            encryption_version: record.encryption_version,
        });
        let plaintext = self
            .state
            .cipher
            .decrypt(&credential.encrypted, aad.as_bytes())
            .map_err(|_| {
                ExecutionFailure::new(
                    ExecutionFailureClass::CredentialDecryptionFailed,
                    "provider credential could not be decrypted",
                )
            })?;
        let value: Value = serde_json::from_slice(&plaintext).map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::ProviderConfigurationInvalid,
                "provider credential payload is invalid",
            )
        })?;
        let secret = secret_from_credential_payload(record.credential_type, &value)?;
        Ok(RuntimeResolvedCredential {
            credential: ResolvedCredential {
                credential_id: record.id,
                credential_version: record.version,
                credential_type: record.credential_type,
                secret: SecretString::new(secret),
                config: value,
            },
            decision: CredentialDecision {
                credential_id: record.id,
                credential_type: record.credential_type,
                source: credential.source,
            },
        })
    }

    async fn runtime_handle(
        &self,
        provider: &ResolvedProviderConfiguration,
        candidate: &ModelCandidate,
        credential: &RuntimeResolvedCredential,
    ) -> Result<Arc<RuntimeModelHandle>, ExecutionFailure> {
        let key = RuntimeCacheKey {
            provider_id: provider.provider_id,
            provider_version: provider.provider_version,
            model_id: candidate.provider_model_id,
            model_version: candidate.model_version,
            credential_id: credential.credential.credential_id,
            credential_version: credential.credential.credential_version,
            runtime_policy_version: candidate.runtime_policy.version,
        };
        self.state
            .runtime_handles
            .get_or_insert_with(key, || async {
                self.factory
                    .build_completion_model(
                        provider,
                        &candidate.model_key,
                        &credential.credential,
                        &candidate.runtime_policy,
                    )
                    .await
            })
            .await
            .map_err(|err| {
                ExecutionFailure::new(
                    ExecutionFailureClass::ProviderConfigurationInvalid,
                    err.to_string(),
                )
            })
    }

    async fn complete_failed_attempt(
        &self,
        attempt_id: Uuid,
        started: Instant,
        failure: &ExecutionFailure,
        usage: UsageSummary,
        provider_status_code: Option<i32>,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.runtime_repo
            .update_attempt(
                attempt_id,
                &ExecutionAttemptUpdate {
                    status: attempt_status_for_failure(failure.class),
                    failure_class: Some(failure.class),
                    provider_status_code,
                    latency_ms: Some(elapsed_ms(started)),
                    usage,
                    provider_request_id: None,
                    metadata,
                },
            )
            .await
    }

    async fn audit_execution_success(&self, command: &ExecutionCommand) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(AuditLogInsert {
                request_id: Some(command.request_id.clone()),
                actor_type: Some(command.identity.actor_type.clone()),
                actor_subject: command.identity.subject.clone(),
                delegated_subject: None,
                external_user_id: command.external_user_id.clone(),
                external_tenant_id: command.external_tenant_id.clone(),
                application_id: command.application_id,
                resource_type: "execution".to_string(),
                resource_id: Some(command.execution_id.to_string()),
                action: "execution.completed".to_string(),
                result: AuditResult::Success,
                source_ip: None,
                user_agent: None,
                metadata: json!({}),
            })
            .await
    }

    async fn audit_execution(
        &self,
        command: &ExecutionCommand,
        action: &str,
        result: AuditResult,
        failure: &ExecutionFailure,
    ) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(AuditLogInsert {
                request_id: Some(command.request_id.clone()),
                actor_type: Some(command.identity.actor_type.clone()),
                actor_subject: command.identity.subject.clone(),
                delegated_subject: None,
                external_user_id: command.external_user_id.clone(),
                external_tenant_id: command.external_tenant_id.clone(),
                application_id: command.application_id,
                resource_type: "execution".to_string(),
                resource_id: Some(command.execution_id.to_string()),
                action: action.to_string(),
                result,
                source_ip: None,
                user_agent: None,
                metadata: json!({ "failure_class": failure.class }),
            })
            .await
    }

    /// F50 — the route names an agent profile that no longer resolves.
    ///
    /// # What this does, and what now happens after it
    ///
    /// [`AgentProfileResolution::classify`] reads the row without a `status`/`deleted_at`
    /// filter. Disabling a profile or soft-deleting it leaves `route_definitions.agent_profile_id`
    /// pointing at the row — the FK is `on delete set null` and `soft_delete_agent_profile`
    /// only writes `status = 'deleted', deleted_at = now()`, never a `DELETE` — so the route
    /// keeps naming a profile the runtime will not use. Before the observability shipped there
    /// was **no failure, no `warn!`, no runtime event and no audit row**: the request simply
    /// proceeded without the profile's `preamble`, `temperature` and `max_tokens` and reported
    /// `succeeded`. A preamble is where guardrails live, so the failure mode was an unguarded
    /// model answering production traffic.
    ///
    /// **Issue #79 decided that fail-closed, so the caller is now also refused** — see
    /// [`agent_profile_failure`]. This function is unchanged in purpose: it is the *operator's*
    /// half, and it still runs on the refusal path because the caller's error is neither durable
    /// nor allowed to name the route's internals.
    ///
    /// # It cannot fire for a route that has no profile
    ///
    /// Called only from the `Some(id)` arm above, so "this route was never given a profile"
    /// — the normal case, and the one every fixture in the tree exercises — stays exactly as
    /// silent as it was, and keeps executing. The two are distinguishable because
    /// `route.agent_profile_id` is the thing that differs, and it is read before the lookup
    /// rather than inferred from it.
    ///
    /// # Three signals, because they have three different consumers
    ///
    /// The `warn!` reaches whoever is tailing logs. The runtime event is structured, carries
    /// the ids, and is returned verbatim by `POST /api/v1/admin/runtime/diagnose` — it is
    /// deliberately *not* mapped onto the public SSE contract, see `map_runtime_event`. The
    /// audit row is the durable one: it survives log rotation and is queryable by
    /// `resource_id = execution_id` alongside `execution.started`.
    ///
    /// All three carry `reason`, which is the same distinction the caller's error code makes:
    /// an operator reading `agent_profile.unavailable` and a caller reading
    /// `agent_profile_disabled` must be looking at the same fact.
    async fn announce_dangling_agent_profile(
        &self,
        command: &ExecutionCommand,
        route: &RouteDecision,
        agent_profile_id: Uuid,
        resolution: &AgentProfileResolution,
        events: &mut EventCollector,
    ) -> Result<(), AppError> {
        let reason = agent_profile_reason(resolution);
        warn!(
            execution_id = %command.execution_id,
            request_id = %command.request_id,
            route_id = %route.route_id,
            route_key = %route.route_key,
            %agent_profile_id,
            reason,
            "route references an agent profile that is disabled or deleted; the execution is \
             refused"
        );
        let detail = json!({
            "agent_profile_id": agent_profile_id,
            "agent_profile_key": agent_profile_key(resolution),
            "reason": reason,
            "route_id": route.route_id,
            "route_key": route.route_key,
        });
        events.push(RuntimeEventType::AgentProfileUnavailable, detail.clone());
        self.audit_runtime_event(
            command,
            "agent_profile.unavailable",
            AuditResult::Failed,
            detail,
        )
        .await
    }

    async fn audit_runtime_event(
        &self,
        command: &ExecutionCommand,
        action: &str,
        result: AuditResult,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(AuditLogInsert {
                request_id: Some(command.request_id.clone()),
                actor_type: Some(command.identity.actor_type.clone()),
                actor_subject: command.identity.subject.clone(),
                delegated_subject: None,
                external_user_id: command.external_user_id.clone(),
                external_tenant_id: command.external_tenant_id.clone(),
                application_id: command.application_id,
                resource_type: "execution".to_string(),
                resource_id: Some(command.execution_id.to_string()),
                action: action.to_string(),
                result,
                source_ip: None,
                user_agent: None,
                metadata,
            })
            .await
    }
}

#[async_trait]
impl ExecutionService for MoiraExecutionService {
    async fn execute(&self, command: ExecutionCommand) -> Result<ExecutionOutcome, AppError> {
        let (outcome, _) = self.execute_with_events(command).await?;
        Ok(outcome)
    }

    async fn execute_stream(
        &self,
        command: ExecutionCommand,
    ) -> Result<ExecutionStreamHandle, AppError> {
        let (tx, rx) = mpsc::channel(self.state.settings.runtime.internal_stream_queue_capacity);
        let (outcome_tx, outcome_rx) = oneshot::channel();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let mut events = EventCollector::streaming(&command, tx, task_cancellation);
            events.push(RuntimeEventType::ExecutionStarted, json!({}));
            let outcome = match service.execute_inner(command, &mut events).await {
                Ok(outcome) => Ok(outcome),
                Err(err) => {
                    let failure = ExecutionFailure::new(
                        ExecutionFailureClass::InternalError,
                        err.to_string(),
                    );
                    events.push(
                        RuntimeEventType::ExecutionFailed,
                        json!({ "failure_class": failure.class }),
                    );
                    Err(failure)
                }
            };
            let _ = outcome_tx.send(outcome);
        });
        Ok(ExecutionStreamHandle::new(rx, outcome_rx, cancellation))
    }
}

#[async_trait]
trait ExecutionPolicyService: Send + Sync {
    async fn evaluate(
        &self,
        command: &ExecutionCommand,
    ) -> Result<EffectiveExecutionPolicy, ExecutionFailure>;
}

struct DefaultExecutionPolicyService<'a> {
    state: &'a AppState,
}

impl<'a> DefaultExecutionPolicyService<'a> {
    fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ExecutionPolicyService for DefaultExecutionPolicyService<'_> {
    async fn evaluate(
        &self,
        command: &ExecutionCommand,
    ) -> Result<EffectiveExecutionPolicy, ExecutionFailure> {
        if command.route_hint.is_some()
            && !has_runtime_scope(command, "moira:execution:override-route")
        {
            return Err(ExecutionFailure::new(
                ExecutionFailureClass::RouteForbidden,
                "route override is not authorized",
            ));
        }
        if command.provider_hint.is_some()
            && !has_runtime_scope(command, "moira:execution:override-provider")
        {
            return Err(ExecutionFailure::new(
                ExecutionFailureClass::ModelForbidden,
                "provider override is not authorized",
            ));
        }
        if command.model_hint.is_some()
            && !has_runtime_scope(command, "moira:execution:override-model")
        {
            return Err(ExecutionFailure::new(
                ExecutionFailureClass::ModelForbidden,
                "model override is not authorized",
            ));
        }
        let defaults = &self.state.settings.runtime;
        let timeout_ms = command
            .options
            .timeout_ms
            .unwrap_or(defaults.default_execution_timeout_seconds * 1_000)
            .min(defaults.maximum_execution_timeout_seconds * 1_000);
        Ok(EffectiveExecutionPolicy {
            timeout_ms,
            max_retries: command
                .options
                .max_retries
                .unwrap_or(defaults.maximum_retries_per_candidate)
                .min(defaults.maximum_retries_per_candidate),
            max_fallbacks: command
                .options
                .max_fallbacks
                .unwrap_or(defaults.maximum_provider_fallback_candidates)
                .min(defaults.maximum_provider_fallback_candidates),
            required_capabilities: command.options.required_capabilities.clone(),
            allow_fallback: command.options.allow_fallback,
        })
    }
}

#[async_trait]
trait TaskRouter: Send + Sync {
    async fn select_route(
        &self,
        command: &ExecutionCommand,
    ) -> Result<RouteDecision, ExecutionFailure>;
}

struct DefaultTaskRouter<'a> {
    repo: &'a PgRuntimeRepository,
}

impl<'a> DefaultTaskRouter<'a> {
    fn new(repo: &'a PgRuntimeRepository) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl TaskRouter for DefaultTaskRouter<'_> {
    async fn select_route(
        &self,
        command: &ExecutionCommand,
    ) -> Result<RouteDecision, ExecutionFailure> {
        if let Some(route_key) = command.route_hint.as_deref() {
            let route = self
                .repo
                .get_active_route_by_key(route_key)
                .await
                .map_err(|_| {
                    ExecutionFailure::new(
                        ExecutionFailureClass::RouteNotFound,
                        "route lookup failed",
                    )
                })?
                .ok_or_else(|| {
                    ExecutionFailure::new(
                        ExecutionFailureClass::RouteNotFound,
                        "route hint did not match an active route",
                    )
                })?;
            return Ok(RouteDecision {
                route_id: route.id,
                route_key: route.route_key,
                reason: RouteSelectionReason::ExplicitHint,
                agent_profile_id: route.agent_profile_id,
            });
        }

        if first_text(command).to_ascii_lowercase().contains("code")
            && let Some(route) =
                self.repo
                    .get_active_route_by_key("coding")
                    .await
                    .map_err(|_| {
                        ExecutionFailure::new(
                            ExecutionFailureClass::RouteNotFound,
                            "route lookup failed",
                        )
                    })?
        {
            return Ok(RouteDecision {
                route_id: route.id,
                route_key: route.route_key,
                reason: RouteSelectionReason::RuleMatch,
                agent_profile_id: route.agent_profile_id,
            });
        }

        let route = self
            .repo
            .get_default_route()
            .await
            .map_err(|_| {
                ExecutionFailure::new(
                    ExecutionFailureClass::RouteNotFound,
                    "default route lookup failed",
                )
            })?
            .ok_or_else(|| {
                ExecutionFailure::new(
                    ExecutionFailureClass::RouteNotFound,
                    "no active default route",
                )
            })?;
        Ok(RouteDecision {
            route_id: route.id,
            route_key: route.route_key,
            reason: RouteSelectionReason::GlobalDefault,
            agent_profile_id: route.agent_profile_id,
        })
    }
}

#[async_trait]
trait ModelRouter: Send + Sync {
    async fn select_candidates(
        &self,
        command: &ExecutionCommand,
        policy: &EffectiveExecutionPolicy,
        route: &RouteDecision,
    ) -> Result<Vec<ModelCandidate>, ExecutionFailure>;
}

struct DefaultModelRouter<'a> {
    repo: &'a PgRuntimeRepository,
    state: &'a AppState,
}

impl<'a> DefaultModelRouter<'a> {
    fn new(repo: &'a PgRuntimeRepository, state: &'a AppState) -> Self {
        Self { repo, state }
    }
}

#[async_trait]
impl ModelRouter for DefaultModelRouter<'_> {
    async fn select_candidates(
        &self,
        command: &ExecutionCommand,
        policy: &EffectiveExecutionPolicy,
        route: &RouteDecision,
    ) -> Result<Vec<ModelCandidate>, ExecutionFailure> {
        let mut candidates = self
            .repo
            .list_model_candidates(
                route.route_id,
                command.application_id,
                command.external_tenant_id.as_deref(),
                self.state
                    .settings
                    .runtime
                    .maximum_eligible_model_candidates as i64,
            )
            .await
            .map_err(|_| {
                ExecutionFailure::new(
                    ExecutionFailureClass::NoEligibleModel,
                    "model candidate lookup failed",
                )
            })?;
        candidates.retain(|candidate| {
            command
                .provider_hint
                .is_none_or(|provider_id| candidate.provider_id == provider_id)
                && command
                    .model_hint
                    .is_none_or(|model_id| candidate.provider_model_id == model_id)
                && capabilities_match(
                    candidate.provider_type,
                    &candidate.capabilities,
                    &policy.required_capabilities,
                )
        });
        if candidates.is_empty() {
            return Err(ExecutionFailure::new(
                ExecutionFailureClass::NoEligibleModel,
                "no eligible model candidate matched policy",
            ));
        }
        Ok(candidates)
    }
}

#[derive(Debug, Clone)]
struct RuntimeResolvedCredential {
    credential: ResolvedCredential,
    decision: CredentialDecision,
}

#[derive(Debug, Clone)]
struct EffectiveRuntimePolicy {
    timeout_ms: u64,
    max_retries: usize,
}

#[derive(Debug, Clone)]
struct ExecutionRunOutput {
    text: String,
    structured_output: Option<Value>,
    usage: UsageSummary,
    provider_request_id: Option<String>,
    events: Vec<RuntimeEventEnvelope>,
}

struct EventCollector {
    request_id: String,
    execution_id: Uuid,
    next_sequence: u64,
    events: Vec<RuntimeEventEnvelope>,
    live_tx: Option<mpsc::Sender<Result<RuntimeEventEnvelope, ExecutionFailure>>>,
    cancellation: CancellationToken,
    delivery_failure: Option<ExecutionFailure>,
    output_committed: bool,
}

impl EventCollector {
    fn new(command: &ExecutionCommand) -> Self {
        Self {
            request_id: command.request_id.clone(),
            execution_id: command.execution_id,
            next_sequence: 1,
            events: Vec::new(),
            live_tx: None,
            cancellation: CancellationToken::new(),
            delivery_failure: None,
            output_committed: false,
        }
    }

    fn streaming(
        command: &ExecutionCommand,
        live_tx: mpsc::Sender<Result<RuntimeEventEnvelope, ExecutionFailure>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            request_id: command.request_id.clone(),
            execution_id: command.execution_id,
            next_sequence: 1,
            events: Vec::new(),
            live_tx: Some(live_tx),
            cancellation,
            delivery_failure: None,
            output_committed: false,
        }
    }

    fn push(&mut self, event_type: RuntimeEventType, payload: Value) {
        let event = RuntimeEventEnvelope {
            request_id: self.request_id.clone(),
            execution_id: self.execution_id,
            sequence: self.next_sequence,
            timestamp: chrono::Utc::now(),
            event_type,
            payload,
        };
        self.next_sequence += 1;
        if self.live_tx.is_some() {
            self.forward_now(event);
        } else {
            self.events.push(event);
        }
    }

    fn push_existing(&mut self, mut event: RuntimeEventEnvelope) {
        event.sequence = self.next_sequence;
        self.next_sequence += 1;
        if self.live_tx.is_some() {
            self.forward_now(event);
        } else {
            self.events.push(event);
        }
    }

    async fn push_stream(
        &mut self,
        event_type: RuntimeEventType,
        payload: Value,
        send_timeout: Duration,
    ) -> Result<(), ExecutionFailure> {
        if let Some(failure) = self.delivery_failure.clone() {
            return Err(failure);
        }
        let event = RuntimeEventEnvelope {
            request_id: self.request_id.clone(),
            execution_id: self.execution_id,
            sequence: self.next_sequence,
            timestamp: chrono::Utc::now(),
            event_type,
            payload,
        };
        self.next_sequence += 1;

        if let Some(tx) = &self.live_tx {
            tokio::select! {
                _ = self.cancellation.cancelled() => {
                    return Err(cancelled_failure());
                }
                result = tokio::time::timeout(send_timeout, tx.send(Ok(event.clone()))) => {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => return Err(cancelled_failure()),
                        Err(_) => {
                            return Err(ExecutionFailure::new(
                                ExecutionFailureClass::StreamBackpressureExceeded,
                                "stream consumer did not accept output before the delivery deadline",
                            ));
                        }
                    }
                }
            }
        }
        if self.live_tx.is_none() {
            self.events.push(event);
        }
        Ok(())
    }

    fn forward_now(&mut self, event: RuntimeEventEnvelope) {
        let Some(tx) = &self.live_tx else {
            return;
        };
        if self.delivery_failure.is_some() {
            return;
        }
        match tx.try_send(Ok(event)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.delivery_failure = Some(cancelled_failure());
                self.cancellation.cancel();
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.delivery_failure = Some(ExecutionFailure::new(
                    ExecutionFailureClass::StreamBackpressureExceeded,
                    "stream consumer did not keep pace with execution lifecycle events",
                ));
                self.cancellation.cancel();
            }
        }
    }

    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn delivery_failure(&self) -> Option<ExecutionFailure> {
        self.delivery_failure.clone()
    }

    fn mark_output_committed(&mut self) {
        self.output_committed = true;
    }

    fn output_committed(&self) -> bool {
        self.output_committed
    }

    fn into_events(self) -> Vec<RuntimeEventEnvelope> {
        self.events
    }
}

/// The whole of what a caller is told when a schema-carrying request came back as something that
/// is not JSON — issue #80.
///
/// One constant, no interpolation, because this string is copied verbatim into the public error
/// envelope and into `responses.failure_message`. Naming the provider, the model, the reply's
/// length or where the parse gave up would either leak deployment topology to a caller who named
/// only a route, or leak the provider's own bytes into Moira's error surface. What the caller can
/// act on is here: they asked for a schema, and what came back was not JSON.
const STRUCTURED_OUTPUT_NOT_JSON: &str = "the model's reply to a schema-constrained request was not JSON, so no structured output \
     could be produced";

/// Parses a schema-constrained reply into [`ExecutionRunOutput::structured_output`] — finding F29.
///
/// # Why the parse lives here rather than at the Rig boundary
///
/// There is no value to forward. `rig_core` 0.40's `CompletionResponse` is
/// `{choice, usage, raw_response, message_id}` and `AssistantContent` is
/// `Text | ToolCall | Reasoning | Image` — **no structured variant** — so populating this field
/// means parsing text as JSON, and the only question is where. `output_from_response` in
/// `src/orchestration/runtime_factory.rs` is the wrong place because
/// [`execute_rig_stream`] never constructs a `RuntimeCompletionOutput` at all: it accumulates
/// text itself. Parsing at the boundary would cover the non-streaming path only and force a
/// divergent second implementation for streams — which is precisely the "second
/// response-narrowing site" `.agents/skills/moira-rig-completions/SKILL.md` forbids. One helper
/// called from both run paths is exactly one parse site covering both.
///
/// # `wants_structured` is the whole safety property, not a fast path
///
/// Populating this field unconditionally **corrupts conversation summarization.** Summarization
/// sends no `output_schema`, `summarization::parse_summary` accepts any non-empty prose, and
/// `ConversationService::summarize_conversation` prefers `structured_output` over `output_text`
/// via `.map(|value| value.to_string())`. So a summary that merely *happened* to be valid JSON
/// would be stored as `Value::to_string()` of itself — reflowed, and quote-and-backslash-escaped
/// when the reply is a bare JSON string — silently changing `summary_hash`, which is documented
/// as a content address of the summary body. Observed, not theorised: an ungated build stored
/// `{"decision":"…"}` in place of the pretty-printed bytes the model sent
/// (`a_summary_that_is_valid_json_is_stored_verbatim`). The caller asking for a schema is what
/// makes re-serialisation safe, because a caller asking for a schema wants the value, not the
/// bytes.
///
/// # A reply that is not JSON is a failure, not a `None` — issue #80, decided 2026-08-06
///
/// **The flip is taken.** A schema-carrying request whose reply does not parse now ends the
/// execution with [`ExecutionFailureClass::StructuredOutputInvalid`], which `failure_http_status`
/// maps to **422**. There is no setting that restores the silent `None`, deliberately: a flag
/// would be a second code path plus a chance to be inert, and this repo has a measured example of
/// exactly that (`accept_legacy_hashes`).
///
/// **What the decision is actually about.** The old behaviour left `structured_output: null` on a
/// `succeeded` outcome, which is the same document a model that legitimately answered with an
/// empty value produces — so no consumer of an [`ExecutionOutcome`] could tell "the provider did
/// not comply" from "the answer was empty", and the cheap reading, *treat null as empty*, is the
/// wrong one every time. **On the public plane it was worse:** `PublicResponse` carries no
/// structured-output field at all, so a caller who asked for a schema and received prose got
/// `200 completed` with no signal whatsoever. After the flip both surfaces agree, and the public
/// one carries the whole decision: a failure is a 422 and cannot be mistaken for an answer. See
/// "empty is not a failure" below for the boundary.
///
/// **Why this was blocked and no longer is.** Three preconditions, each verified against the tree
/// rather than assumed, and all three discharged before this change:
///
/// 1. `StructuredOutputInvalid` was in **neither** `is_retryable` nor `is_fallback_eligible` nor
///    `is_circuit_failure` — by omission, with nothing recording whether that was a decision.
///    **Done:** the exclusion is now a recorded disposition in `src/orchestration/controls.rs`,
///    guarded in both directions by
///    `every_failure_class_has_a_recorded_retry_fallback_and_circuit_disposition`. The disposition
///    is unchanged by this flip and was written with this third emitter already in view: a class
///    carries exactly one disposition, so admitting it to `is_fallback_eligible` for the reply
///    case would also let one 2 MB caller schema walk the whole fallback chain.
/// 2. On DeepSeek the schema never reaches the wire — Rig's `SUPPORTS_RESPONSE_FORMAT = false`
///    drops it — so *every* structured request on that route would have hard-failed.
///    **Done:** F39 landed, and a model that cannot carry a schema is no longer routed a
///    schema-carrying request. What fails here is a model declining to comply, never a provider
///    structurally unable to.
/// 3. `ConversationService::run_extraction` detected failure by `output_text` being `None` and
///    never inspected `execution.status`, so a hard failure would have reclassified an
///    unparseable extraction reply from `structured_output_invalid` to `extraction_call_failed`.
///    **Done:** `extraction_failure_class` reads the execution's own class, so
///    `memory_extraction_runs.failure_class` says `structured_output_invalid` before **and**
///    after the flip — written by `parse_candidates` refusing prose before, by the execution now.
///
/// # Empty is not a failure, and the two are distinguishable — but only in one direction
///
/// A model that legitimately answers "nothing" under a schema sends `null`, `{}` or `[]`. All
/// three parse, so all three still succeed and arrive as `structured_output: null`, `{}` or `[]`.
/// Only bytes that are **not JSON at all** fail.
///
/// Two honest limits, stated rather than implied:
///
/// - **Moira does not validate against the schema; it parses JSON.** A reply that is valid JSON
///   but violates the caller's schema still succeeds, exactly as before. Enforcement is the
///   provider's job (`strict: true` reaches every provider that receives a schema at all), and
///   Moira has no validator. The failure raised here therefore says "not JSON", which is what was
///   measured, rather than "does not conform", which was not.
/// - **`Some(Value::Null)` and `None` serialise identically.** A model answering the JSON literal
///   `null` is indistinguishable on the wire from a request that carried no schema. That
///   ambiguity is *why* the flip matters rather than an argument against it: after it, a null
///   `structured_output` on a `200` can only mean an answer, never a failure, because every
///   failure is a `422`.
///
/// # What a failed parse deliberately does not do
///
/// It does not put the provider's bytes in the failure message. `failure.message` is copied
/// verbatim into the public error envelope by `PublicExecutionService`, so echoing the reply
/// would hand a caller — and every log that records the envelope — untrusted provider output
/// under Moira's own error surface. The message is [`STRUCTURED_OUTPUT_NOT_JSON`], a constant.
/// The prose is dropped with the rest of the outcome by `failed_outcome`, which is the same
/// reason: a 422 must not return something that reads like an answer.
///
/// It also does not retry or fall back. The disposition above is unchanged, so the failure is
/// terminal on the first non-conforming reply.
///
/// **Recorded consequence.** `memory_extraction::strip_code_fence` — the tolerance for a model
/// that wraps its JSON in a ```` ```json ```` fence — is no longer reachable from an execution:
/// extraction always sends a schema, so a fenced reply now fails here before `parse_candidates`
/// is asked. That tolerance is left in place rather than deleted in this change, and it is left
/// *strict* here rather than duplicated: what counts as a parse is unchanged by the flip, which
/// is the whole of what was decided. *Reversal condition:* if a real deployment measures fenced
/// replies from schema-receiving backends, move the fence tolerance to this one site — do not add
/// a second one.
///
/// # Strict, and deliberately not a scavenger
///
/// `serde_json::from_str` over the trimmed text and nothing else. Rig's own balanced-brace scan
/// is **not** copied, and no code fence is stripped: `memory_extraction::parse_candidates`
/// documents the refusal to hunt JSON inside prose ("a heuristic extractor over untrusted text
/// is a parser differential waiting to happen") and owns the one real-world tolerance — a
/// ```` ```json ```` fence — on the `output_text` it already falls back to. Duplicating that
/// tolerance here would give the tree two parsers with two accept-sets over the same bytes.
fn structured_output_from_text(
    wants_structured: bool,
    text: &str,
) -> Result<Option<Value>, ExecutionFailure> {
    if !wants_structured {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(value) => Ok(Some(value)),
        // The `serde_json::Error` is dropped rather than formatted in. It carries a line and
        // column into the provider's reply, which is a description of bytes this message must not
        // describe; the condition is the same one however far into the reply it was detected.
        Err(_) => Err(ExecutionFailure::new(
            ExecutionFailureClass::StructuredOutputInvalid,
            STRUCTURED_OUTPUT_NOT_JSON,
        )),
    }
}

/// A failed provider attempt, carrying whatever the provider reported spending before it failed.
///
/// # Why the usage travels with the failure
///
/// Issue #80 introduced the first failure that is raised **after** a complete, billed provider
/// reply: the model answered, the provider charged for the answer, and Moira refuses it because
/// it is not JSON. Before the flip that same request succeeded, so it wrote an
/// `execution_attempts.usage` and a `usage_records` row through the success arm. Returning a bare
/// [`ExecutionFailure`] would have dropped both — provider tokens spent with nothing metered,
/// on a path a caller can steer traffic onto by sending a schema to a backend that does not
/// honour it (`openai_compatible` and `local` are admitted unverified — see
/// `docs/openai-compatibility.md` — and route, provider and model are caller-supplied). That is
/// a revenue and quota hole rather than a reporting nicety, which is why the tokens are carried
/// rather than recorded as a known cost.
///
/// [`UsageSummary::default()`] is all-`None`, i.e. **unknown, not zero** — the reading
/// `insert_usage_record` is skipped on. Every failure raised before or instead of a complete
/// reply keeps that default through the [`From`] impl below, so `?` still means "no reply, so
/// nothing measured" and only a site with real counts in hand has to say so.
struct FailedAttempt {
    failure: ExecutionFailure,
    usage: UsageSummary,
}

impl From<ExecutionFailure> for FailedAttempt {
    fn from(failure: ExecutionFailure) -> Self {
        Self {
            failure,
            usage: UsageSummary::default(),
        }
    }
}

/// Whether the provider reported any token counts at all.
///
/// The distinction is `UsageSummary`'s own: every field is an `Option`, and all-`None` means the
/// provider said nothing rather than that it charged nothing. A `usage_records` row built from
/// all-`None` would be a billing row asserting zero tokens for a call that certainly used some,
/// which is worse than the absence it replaces.
fn usage_was_reported(usage: &UsageSummary) -> bool {
    usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cached_input_tokens.is_some()
        || usage.reasoning_tokens.is_some()
        || usage.total_tokens.is_some()
}

async fn execute_rig_completion(
    handle: Arc<RuntimeModelHandle>,
    request: CompletionRequest,
) -> Result<ExecutionRunOutput, FailedAttempt> {
    let wants_structured = request.output_schema.is_some();
    let output = handle.completion(request).await?;
    let structured_output = match structured_output_from_text(wants_structured, &output.text) {
        Ok(structured_output) => structured_output,
        // The provider answered and billed for the answer; only the shape was refused. The
        // counts are in hand at exactly this statement, and this is the one place they can be
        // kept without the attempt loop having to guess.
        Err(failure) => {
            return Err(FailedAttempt {
                failure,
                usage: output.usage,
            });
        }
    };
    Ok(ExecutionRunOutput {
        text: output.text,
        structured_output,
        usage: output.usage,
        provider_request_id: output.provider_request_id,
        events: Vec::new(),
    })
}

/// Everything the streaming path needs to record time-to-first-token, grouped so the
/// function signature does not grow a three-argument metrics tail.
struct StreamMetricsContext<'a> {
    metrics: &'a MetricsRegistry,
    provider_type: ProviderType,
    /// The same `Instant` the attempt's `latency_ms` is measured from, so TTFT is always
    /// less than or equal to the attempt latency recorded for the same attempt.
    attempt_started: Instant,
}

/// Records TTFT exactly once per attempt, on the first output-bearing chunk.
fn record_first_token(recorded: &mut bool, stream_metrics: &StreamMetricsContext<'_>) {
    if *recorded {
        return;
    }
    *recorded = true;
    stream_metrics.metrics.record_ttft(
        stream_metrics.provider_type,
        stream_metrics.attempt_started.elapsed(),
    );
}

/// The **only** place a `RuntimeItemStream` is drained into runtime events.
///
/// F44 — there used to be a second one, `RuntimeModelHandle::stream` in
/// `src/orchestration/runtime_factory.rs`: ~66 lines that drove `start_stream` through the same
/// `RuntimeStreamItem` match and produced a `RuntimeStreamOutput { text, usage,
/// provider_request_id, events }`. It had **zero callers** — `.stream(` occurred exactly once in
/// the whole tree and that occurrence was Rig's own `CompletionModel::stream` inside
/// `start_stream_with_model` — and it was kept compiling only by the re-export in
/// `src/orchestration/mod.rs`. It is deleted, along with `RuntimeStreamOutput`,
/// `RuntimeEventSeed` and `next_event`, which existed only to serve it.
///
/// The hazard was divergence, not the dead lines: this loop has since grown idle timeouts,
/// backpressure, cancellation, TTFT metrics and `mark_output_committed`, and the duplicate had
/// none of them. Anyone fixing streaming had a coin-flip chance of fixing the wrong one.
///
/// Deleting it also restored the module boundary. That cluster was the sole reason
/// `runtime_factory.rs` imported `RuntimeEventEnvelope`, `RuntimeEventType` and `serde_json::json`
/// at all; with it gone, the file compiles without the runtime-event vocabulary, which is what
/// `moira-rig-integration` says the Rig seam should look like — Rig primitives there, runtime
/// events here.
///
/// **Nothing mechanical prevents a second drain loop being written again.** `dead_code` cannot
/// see it: the items were `pub` in a library crate, and `pub` items are exempt from that lint
/// regardless of whether anything calls them, which is exactly how ~95 lines survived. Saying so
/// is more useful than a brittle source-scan pretending otherwise.
async fn execute_rig_stream(
    handle: Arc<RuntimeModelHandle>,
    request: CompletionRequest,
    events: &mut EventCollector,
    idle_timeout: Duration,
    stream_metrics: StreamMetricsContext<'_>,
) -> Result<ExecutionRunOutput, FailedAttempt> {
    if let Some(failure) = events.delivery_failure() {
        return Err(failure.into());
    }
    // Captured before `request` is moved into `start_stream` below.
    let wants_structured = request.output_schema.is_some();
    let cancellation = events.cancellation();
    let mut stream = tokio::select! {
        _ = cancellation.cancelled() => return Err(cancelled_failure().into()),
        result = handle.start_stream(request) => result?,
    };
    let mut text = String::new();
    let mut usage = UsageSummary::default();
    let mut provider_request_id = None;
    let mut committed = false;
    // TTFT is recorded on the first *output-bearing* chunk, which is exactly the point
    // `committed` first flips. Usage and final-metadata chunks are not output and must not
    // count as a first token.
    let mut ttft_recorded = false;

    loop {
        let item = tokio::select! {
            _ = cancellation.cancelled() => return Err(cancelled_failure().into()),
            result = tokio::time::timeout(idle_timeout, stream.next()) => {
                match result {
                    Ok(item) => item,
                    Err(_) => {
                        let mut failure = ExecutionFailure::new(
                            ExecutionFailureClass::ProviderTimeout,
                            "provider stream exceeded the idle timeout",
                        );
                        if committed {
                            failure.retryable = false;
                            failure.fallback_eligible = false;
                        }
                        return Err(failure.into());
                    }
                }
            }
        };

        let Some(item) = item else {
            break;
        };
        let item = match item {
            Ok(item) => item,
            Err(mut failure) => {
                if committed {
                    failure.retryable = false;
                    failure.fallback_eligible = false;
                }
                // Deliberately **not** carrying `usage` here, unlike the structured-output site
                // below. A stream that breaks mid-flight was never a metered success — it failed
                // this way before issue #80 as well, so no metering is being lost — and this
                // failure *is* retryable and fallback-eligible, so the counts a retry reports
                // would be added to a partial figure whose relationship to the provider's own
                // invoice nothing here can establish. Widening it is a separate decision with a
                // separate blast radius.
                return Err(failure.into());
            }
        };
        match item {
            RuntimeStreamItem::TextDelta { text: delta } => {
                events
                    .push_stream(
                        RuntimeEventType::OutputTextDelta,
                        json!({ "text": delta }),
                        idle_timeout,
                    )
                    .await?;
                text.push_str(&delta);
                committed = true;
                record_first_token(&mut ttft_recorded, &stream_metrics);
                events.mark_output_committed();
            }
            RuntimeStreamItem::ToolCallStarted {
                internal_call_id,
                name,
                arguments,
            } => {
                events
                    .push_stream(
                        RuntimeEventType::ToolCallStarted,
                        json!({
                            "internal_call_id": internal_call_id,
                            "name": name,
                            "arguments": arguments
                        }),
                        idle_timeout,
                    )
                    .await?;
                committed = true;
                record_first_token(&mut ttft_recorded, &stream_metrics);
                events.mark_output_committed();
            }
            RuntimeStreamItem::ToolCallDelta {
                id,
                internal_call_id,
                content,
            } => {
                events
                    .push_stream(
                        RuntimeEventType::ToolCallDelta,
                        json!({
                            "id": id,
                            "internal_call_id": internal_call_id,
                            "content": content
                        }),
                        idle_timeout,
                    )
                    .await?;
                committed = true;
                record_first_token(&mut ttft_recorded, &stream_metrics);
                events.mark_output_committed();
            }
            RuntimeStreamItem::UsageUpdated {
                usage: updated_usage,
            } => {
                usage = updated_usage;
                events
                    .push_stream(
                        RuntimeEventType::UsageUpdated,
                        json!({ "usage": usage }),
                        idle_timeout,
                    )
                    .await?;
            }
            RuntimeStreamItem::FinalMetadata {
                provider_request_id: request_id,
            } => provider_request_id = request_id,
        }
    }

    // Issue #80. On this path the caller may already have received every delta, and the failure
    // is raised anyway: the deltas were text, and the caller asked for a value. `committed` does
    // not clamp anything here because `StructuredOutputInvalid` is already neither retryable nor
    // fallback-eligible, so there is no re-run for a committed stream to be protected from.
    //
    // The stream ran to completion, so `usage` holds whatever the provider's final usage chunk
    // reported: the same counts the success arm two lines below would have metered. They travel
    // with the failure for the same reason they would have been metered — the provider will
    // invoice for this call either way.
    let structured_output = match structured_output_from_text(wants_structured, &text) {
        Ok(structured_output) => structured_output,
        Err(failure) => return Err(FailedAttempt { failure, usage }),
    };
    Ok(ExecutionRunOutput {
        text,
        structured_output,
        usage,
        provider_request_id,
        events: Vec::new(),
    })
}

fn cancelled_failure() -> ExecutionFailure {
    ExecutionFailure::new(
        ExecutionFailureClass::RequestCancelled,
        "execution stream was cancelled by its consumer",
    )
}

fn build_completion_request(
    command: &ExecutionCommand,
    agent_profile: Option<&AgentProfileRecord>,
) -> Result<CompletionRequest, ExecutionFailure> {
    let chat_history = rig_chat_history(&command.messages)?;
    let output_schema = command
        .options
        .output_schema
        .clone()
        .map(serde_json::from_value::<rig_core::schemars::Schema>)
        .transpose()
        .map_err(|_| {
            ExecutionFailure::new(
                ExecutionFailureClass::StructuredOutputInvalid,
                "structured output schema is invalid",
            )
        })?;
    Ok(CompletionRequest {
        model: None,
        preamble: agent_profile.and_then(|profile| profile.preamble.clone()),
        chat_history,
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: command
            .options
            .temperature
            .or_else(|| agent_profile.and_then(|profile| profile.temperature)),
        max_tokens: command.options.max_tokens.or_else(|| {
            agent_profile.and_then(|profile| profile.max_tokens.map(|value| value as u64))
        }),
        tool_choice: None,
        additional_params: if command.metadata.is_null() {
            None
        } else {
            Some(json!({ "moira": { "request_id": command.request_id } }))
        },
        output_schema,
    })
}

fn failed_outcome(
    command: ExecutionCommand,
    route: Option<RouteDecision>,
    model: Option<ModelDecision>,
    attempts: Vec<ProviderAttemptSummary>,
    failure: ExecutionFailure,
) -> ExecutionOutcome {
    ExecutionOutcome {
        request_id: command.request_id,
        execution_id: command.execution_id,
        status: execution_status_for_failure(failure.class),
        output_text: None,
        structured_output: None,
        usage: usage_reported_by(&attempts),
        route,
        model,
        attempts,
        failure: Some(failure),
    }
}

/// What a failed outcome reports as its usage: whatever its own attempts reported.
///
/// **Derived rather than passed in, so the two cannot disagree.** Finding F38 was exactly that
/// asymmetry in the other direction — an outcome hardcoding `UsageSummary::default()` next to an
/// `attempts` array that carried the real counts, inside one serialised document. Since the issue
/// #80 review a *failed* attempt can carry counts too (a billed reply Moira refused), so the
/// hardcoded default would have re-created the contradiction on the new path. Reading the array
/// makes it impossible to state a total the document itself contradicts.
///
/// The last attempt that reported anything, not a sum: `ExecutionOutcome.usage` is the same
/// "what this execution's answering call cost" figure the success arm sets from a single
/// attempt, and every earlier attempt already has its own `usage_records` row. All-`None` when
/// no attempt reported anything — unknown, which is what it has always meant here.
fn usage_reported_by(attempts: &[ProviderAttemptSummary]) -> UsageSummary {
    attempts
        .iter()
        .rev()
        .find(|attempt| usage_was_reported(&attempt.usage))
        .map(|attempt| attempt.usage.clone())
        .unwrap_or_default()
}

/// The refusal a route with an unusable agent profile produces — F50, decided fail-closed on
/// issue #79.
///
/// # Why two classes and not one
///
/// "The operator switched this profile off" and "no live row has this id" are different
/// conditions with different remedies — re-enable versus re-create — and a single
/// `agent_profile_unavailable` code would force whoever received it to go and look. They also get
/// different HTTP statuses (`404` and `409`, see `failure_http_status`), so a client can branch
/// on the status alone without parsing anything.
///
/// # What the message must contain
///
/// The id, always: it is the value written in `route_definitions.agent_profile_id` and the one
/// thing that identifies the profile when the row is gone. The `profile_key` too whenever a row
/// exists, because that is what an operator recognises in the console. And the route key, because
/// the caller named a route, not a profile — without it the error names something the caller has
/// never heard of. **The point of this text is that whoever receives it can fix the deployment
/// without being given access to the server's logs.** None of it is a secret: these are
/// configuration identifiers already visible on the admin plane, and the profile's `preamble` —
/// the one field that could carry sensitive prompt content — is deliberately not here.
fn agent_profile_failure(
    route: &RouteDecision,
    agent_profile_id: Uuid,
    resolution: &AgentProfileResolution,
) -> ExecutionFailure {
    match resolution {
        // Not constructible: the caller only reaches this for a non-`Active` resolution. Kept as
        // a real arm rather than an `unreachable!` so a future reshuffle degrades into the safe
        // answer — refusing an active profile is a visible bug, serving an unusable one is not.
        AgentProfileResolution::Active(profile) => ExecutionFailure::new(
            ExecutionFailureClass::AgentProfileNotFound,
            format!(
                "route '{}' resolved agent profile '{}' ({agent_profile_id}) but the execution \
                 refused it; this is a bug in Moira, not in your configuration",
                route.route_key, profile.profile_key
            ),
        ),
        AgentProfileResolution::Disabled(profile) => ExecutionFailure::new(
            ExecutionFailureClass::AgentProfileDisabled,
            format!(
                "route '{}' requires agent profile '{}' ({agent_profile_id}), which is disabled; \
                 re-enable that profile or point the route at an active one",
                route.route_key, profile.profile_key
            ),
        ),
        AgentProfileResolution::Missing => ExecutionFailure::new(
            ExecutionFailureClass::AgentProfileNotFound,
            format!(
                "route '{}' requires agent profile {agent_profile_id}, which no longer exists; \
                 create the profile or point the route at an existing one",
                route.route_key
            ),
        ),
    }
}

/// The `reason` the operator-side signals carry, matching the caller's error code.
fn agent_profile_reason(resolution: &AgentProfileResolution) -> &'static str {
    match resolution {
        AgentProfileResolution::Active(_) => "active",
        AgentProfileResolution::Disabled(_) => "disabled",
        AgentProfileResolution::Missing => "missing",
    }
}

/// The profile's key when a row still exists, so the operator signal names what the console shows.
fn agent_profile_key(resolution: &AgentProfileResolution) -> Option<&str> {
    match resolution {
        AgentProfileResolution::Active(profile) | AgentProfileResolution::Disabled(profile) => {
            Some(profile.profile_key.as_str())
        }
        AgentProfileResolution::Missing => None,
    }
}

fn caller_identity_from_actor(actor: &Actor) -> CallerRuntimeIdentity {
    CallerRuntimeIdentity {
        actor_type: format!("{:?}", actor.actor_type),
        subject: actor.subject.clone(),
        external_user_id: actor.external_user_id.clone(),
        external_tenant_id: actor.external_tenant_id.clone(),
        application_id: actor.internal_application_id,
        scopes: actor.scopes.clone(),
    }
}

fn has_runtime_scope(command: &ExecutionCommand, required: &str) -> bool {
    let has_required = command
        .identity
        .scopes
        .iter()
        .any(|scope| scope.as_str() == required);
    let admin = command
        .identity
        .scopes
        .iter()
        .any(|scope| scope.as_str() == "moira:admin")
        && command.identity.actor_type != format!("{:?}", ActorType::ConsumerKey);
    has_required || admin
}

fn effective_runtime_policy(
    policy: &EffectiveExecutionPolicy,
    runtime: &ProviderRuntimePolicyRecord,
) -> EffectiveRuntimePolicy {
    EffectiveRuntimePolicy {
        timeout_ms: policy.timeout_ms.min(runtime.request_timeout_ms as u64),
        max_retries: policy.max_retries.min(runtime.retry_limit as usize),
    }
}

/// The one capability key whose configured value Rig is able to contradict (finding F39).
///
/// `application/public.rs` pushes this string for every non-`text` response format. It is
/// duplicated there rather than shared because `public.rs` may not import `rig_core`
/// (`.agents/skills/moira-rig-integration/SKILL.md`), and this module is where the
/// reconciliation has to live.
const STRUCTURED_OUTPUT_CAPABILITY: &str = "structured_output";

/// Whether Rig 0.40 will actually put a request's `output_schema` on the wire for this provider.
///
/// **Read from Rig, not restated.** Every provider Moira builds through the OpenAI-compatible
/// arm answers with Rig's own public associated const,
/// `openai::completion::OpenAICompatibleProvider::SUPPORTS_RESPONSE_FORMAT`. A `rig-core` bump
/// that flips one of those constants therefore flips Moira's admission decision with no edit
/// here — for those four provider types the config/wire divergence F39 describes is not
/// *representable*, which is strictly stronger than a table plus a test that notices it rotted.
///
/// This matters because the drop is otherwise invisible. When the const is false,
/// `providers/openai/completion/mod.rs` discards `output_schema` with only a `tracing::warn!`
/// and builds the request anyway, so nothing observable at Moira's layer distinguishes "the
/// schema was sent and the model ignored it" from "the schema was never sent". The const is the
/// only signal available *before* the request goes out.
///
/// `Anthropic` and `Gemini` do not implement `OpenAICompatibleProvider` — they map
/// `output_schema` natively onto their own request shapes (`anthropic/completion.rs`
/// `output_config`, `gemini/completion.rs` `generation_config`) and expose no constant to read.
/// `true` is restated for those two and pinned by
/// `the_two_native_providers_still_map_output_schema` below; that test is the thing that reds if
/// a bump makes the restatement false.
///
/// `Custom` never constructs a model at all — `build_completion_model` returns
/// `AppError::Config` — so no schema can reach any provider on that arm.
fn provider_emits_output_schema(provider_type: ProviderType) -> bool {
    use rig_core::providers::openai::OpenAICompatibleProvider;

    match provider_type {
        ProviderType::OpenAi | ProviderType::OpenAiCompatible | ProviderType::Local => {
            <rig_core::providers::openai::OpenAICompletionsExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT
        }
        ProviderType::AzureOpenAi => {
            <rig_core::providers::azure::AzureExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT
        }
        ProviderType::DeepSeek => {
            <rig_core::providers::deepseek::DeepSeekExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT
        }
        ProviderType::Anthropic | ProviderType::Gemini => true,
        ProviderType::Custom => false,
    }
}

/// Whether a routing candidate can satisfy every capability the policy requires.
///
/// The configured capability JSON is necessary but **not sufficient** for `structured_output`:
/// it is an operator's claim about a model, and for some provider types Rig will drop the schema
/// regardless of what the row says. Reconciling here — at the one site that already answers
/// "does this candidate have capability X" — keeps the answer single-sourced and lets an
/// unqualified candidate fall out of routing rather than fail mid-flight.
///
/// The reconciliation only ever **subtracts**. A row that declares `structured_output: false`
/// stays unusable for structured requests even on a provider Rig would honour, because that
/// declaration is also an operator decision to disable it.
fn capabilities_match(
    provider_type: ProviderType,
    capabilities: &Value,
    required: &[String],
) -> bool {
    required.iter().all(|required| {
        if required == STRUCTURED_OUTPUT_CAPABILITY && !provider_emits_output_schema(provider_type)
        {
            return false;
        }
        capabilities
            .get(required)
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || capabilities
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(required)))
    })
}

fn supported_credential_types(
    provider_type: ProviderType,
) -> &'static [crate::domain::CredentialType] {
    use crate::domain::CredentialType;
    match provider_type {
        ProviderType::AzureOpenAi => &[CredentialType::AzureOpenAi, CredentialType::ApiKey],
        ProviderType::OpenAi | ProviderType::OpenAiCompatible | ProviderType::Local => {
            &[CredentialType::ApiKey, CredentialType::BearerToken]
        }
        ProviderType::Anthropic | ProviderType::Gemini | ProviderType::DeepSeek => {
            &[CredentialType::ApiKey]
        }
        ProviderType::Custom => &[],
    }
}

fn secret_from_credential_payload(
    credential_type: crate::domain::CredentialType,
    value: &Value,
) -> Result<String, ExecutionFailure> {
    // The field mapping lives in `crate::security::credential_secret_field` so the embedding
    // path cannot grow a second, divergent copy of it.
    let Some(key) = crate::security::credential_secret_field(credential_type) else {
        return Err(ExecutionFailure::new(
            ExecutionFailureClass::ProviderConfigurationInvalid,
            "credential type is not executable as a completion credential",
        ));
    };
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ExecutionFailure::new(
                ExecutionFailureClass::ProviderConfigurationInvalid,
                "credential payload does not contain the required secret field",
            )
        })
}

fn first_text(command: &ExecutionCommand) -> String {
    command
        .messages
        .iter()
        .find_map(DomainMessage::first_text)
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

fn attempt_summary(
    attempt_id: Uuid,
    attempt_number: i32,
    candidate: &ModelCandidate,
    credential_id: Uuid,
    failure_class: Option<ExecutionFailureClass>,
    started: Instant,
    usage: UsageSummary,
) -> ProviderAttemptSummary {
    ProviderAttemptSummary {
        attempt_id,
        attempt_number,
        provider_id: candidate.provider_id,
        provider_model_id: candidate.provider_model_id,
        credential_id,
        status: failure_class
            .map(attempt_status_for_failure)
            .unwrap_or(AttemptStatus::Succeeded),
        failure_class,
        latency_ms: Some(elapsed_ms(started)),
        usage,
    }
}

fn attempt_status_for_failure(failure_class: ExecutionFailureClass) -> AttemptStatus {
    if failure_class == ExecutionFailureClass::RequestCancelled {
        AttemptStatus::Cancelled
    } else {
        AttemptStatus::Failed
    }
}

fn execution_status_for_failure(failure_class: ExecutionFailureClass) -> ExecutionStatus {
    if failure_class == ExecutionFailureClass::RequestCancelled {
        ExecutionStatus::Cancelled
    } else {
        ExecutionStatus::Failed
    }
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryWait {
    Ready,
    Deadline,
    Cancelled,
}

async fn sleep_for_retry(
    retry_number: usize,
    policy: &ProviderRuntimePolicyRecord,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> RetryWait {
    let Some(remaining) = remaining_execution_time(deadline) else {
        return RetryWait::Deadline;
    };
    let base = policy.retry_base_delay_ms.max(0) as u64;
    let max = policy.retry_max_delay_ms.max(0) as u64;
    let exponential = base.saturating_mul(2_u64.saturating_pow(retry_number as u32));
    let delay = exponential.min(max);
    if delay > 0 {
        let delay = Duration::from_millis(delay);
        if delay >= remaining {
            tokio::select! {
                _ = cancellation.cancelled() => return RetryWait::Cancelled,
                _ = tokio::time::sleep(remaining) => return RetryWait::Deadline,
            }
        }
        tokio::select! {
            _ = cancellation.cancelled() => return RetryWait::Cancelled,
            _ = tokio::time::sleep(delay) => {}
        }
    }
    if cancellation.is_cancelled() {
        RetryWait::Cancelled
    } else if remaining_execution_time(deadline).is_some() {
        RetryWait::Ready
    } else {
        RetryWait::Deadline
    }
}

fn remaining_execution_time(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
}

/// Floor applied to the terminal-persistence budget.
///
/// The three terminal writes run *after* the provider call has already succeeded. Giving
/// them only the literal leftover budget would orphan `execution_attempts` rows in
/// `started` whenever the deadline happens to expire during the provider response — a
/// durability regression introduced by the very bound that is meant to improve
/// durability. The phase therefore stays bounded (never indefinite) but is guaranteed a
/// usable minimum.
const TERMINAL_PERSISTENCE_MIN_BUDGET: Duration = Duration::from_millis(2_000);

/// Bound on the best-effort audit write that records a terminal-persistence breach.
const TERMINAL_PERSISTENCE_AUDIT_BUDGET: Duration = Duration::from_millis(1_000);

/// Runs a pre-attempt phase under whatever is left of the total execution deadline.
///
/// The remaining budget is computed *inside* this helper, so every call site necessarily
/// re-reads the clock instead of reusing a `remaining` captured before an earlier phase
/// consumed part of the budget. Both current callers (`resolve_credential`,
/// `runtime_handle`) run before any attempt row or concurrency permit exists, so a breach
/// needs no cleanup beyond the failure their own error arms already handle.
async fn bounded_phase<T, F>(deadline: Instant, phase: F) -> Result<T, ExecutionFailure>
where
    F: Future<Output = Result<T, ExecutionFailure>>,
{
    let Some(remaining) = remaining_execution_time(deadline) else {
        return Err(deadline_failure());
    };
    match tokio::time::timeout(remaining, phase).await {
        Ok(result) => result,
        Err(_) => Err(deadline_failure()),
    }
}

/// Effective budget for a phase that also has its own configured timeout.
fn phase_budget(remaining: Duration, phase_timeout: Duration) -> Duration {
    remaining.min(phase_timeout)
}

/// Budget for the terminal-persistence group: the live remaining budget, floored so the
/// phase is never handed a zero (which `tokio::time::timeout` would treat as "already
/// elapsed", not "unbounded", but which would still guarantee an orphaned attempt row).
fn terminal_persistence_budget(deadline: Instant) -> Duration {
    remaining_execution_time(deadline)
        .unwrap_or(Duration::ZERO)
        .max(TERMINAL_PERSISTENCE_MIN_BUDGET)
}

/// Failure raised when terminal persistence overruns the deadline.
///
/// Built through `attempt_timeout_failure(bounded_by_total_deadline = true,
/// output_committed = true)` so it inherits the existing "never retry and never fall back"
/// clamp rather than introducing a parallel scheme. The message is specialised so the
/// condition is distinguishable from a plain `deadline_failure()` in logs, audit metadata,
/// and the outcome envelope.
///
/// **The `true` here is not the same claim the audit entry makes** (finding F38). The clamp
/// argument is unconditionally `true` because the *provider call already completed and its
/// tokens are already spent*, so a retry or a fallback would buy a second answer at a second
/// cost — that is true whether or not a single byte reached the caller. The
/// `"output_committed"` key in the audit entry and the `ExecutionFailed` event answers the
/// different question "has the caller seen this output", and is read from
/// [`EventCollector::output_committed`] rather than asserted. Keep the two apart: making this
/// argument conditional would let a non-streaming execution be retried against a provider
/// that has already billed for the answer.
fn terminal_persistence_deadline_failure() -> ExecutionFailure {
    let mut failure = attempt_timeout_failure(true, true);
    failure.message =
        "execution exceeded its total deadline while persisting terminal state".to_string();
    failure
}

fn deadline_failure() -> ExecutionFailure {
    ExecutionFailure::new(
        ExecutionFailureClass::DeadlineExceeded,
        "execution exceeded its total deadline",
    )
}

fn attempt_timeout_failure(
    bounded_by_total_deadline: bool,
    output_committed: bool,
) -> ExecutionFailure {
    let mut failure = if bounded_by_total_deadline {
        deadline_failure()
    } else {
        ExecutionFailure::new(
            ExecutionFailureClass::ProviderTimeout,
            "provider request exceeded effective deadline",
        )
    };
    if output_committed {
        failure.retryable = false;
        failure.fallback_eligible = false;
    }
    failure
}

pub async fn execute_diagnostic(
    state: AppState,
    actor: &Actor,
    ctx: &RequestContext,
    request: DiagnosticExecutionRequest,
) -> Result<DiagnosticExecutionResponse, AppError> {
    if !state.settings.runtime.diagnostic_endpoint_enabled {
        return Err(AppError::NotFound(
            "runtime diagnostic endpoint".to_string(),
        ));
    }
    state.authz.require(actor, "moira:runtime:diagnose")?;
    let service = MoiraExecutionService::new(state)?;
    let command = MoiraExecutionService::command_from_diagnostic(actor, ctx, request);
    let (outcome, events) = service.execute_with_events(command).await?;
    Ok(DiagnosticExecutionResponse { outcome, events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ExecutionOptions;
    use crate::{app::AppState, config::Settings, security::ActorType};

    /// Guard for assertions whose only failure mode would otherwise be an infinite await.
    /// Generous enough never to fire on a loaded machine, short enough that a regression
    /// surfaces as a test failure rather than as a CI job timeout.
    const UNBOUNDED_PHASE_GUARD: Duration = Duration::from_secs(5);

    #[test]
    fn cancellation_uses_terminal_cancelled_states() {
        assert_eq!(
            attempt_status_for_failure(ExecutionFailureClass::RequestCancelled),
            AttemptStatus::Cancelled
        );
        assert_eq!(
            execution_status_for_failure(ExecutionFailureClass::RequestCancelled),
            ExecutionStatus::Cancelled
        );
        assert_eq!(
            attempt_status_for_failure(ExecutionFailureClass::ProviderUnavailable),
            AttemptStatus::Failed
        );
    }

    /// Finding F39. A `structured_output: true` capability row is an operator's claim; on
    /// DeepSeek it is false whatever the row says, because Rig drops the schema before the wire.
    ///
    /// The cheapest edit that breaks the property is deleting the `STRUCTURED_OUTPUT_CAPABILITY`
    /// early return in `capabilities_match` — that edit turns this case red.
    #[test]
    fn a_deepseek_candidate_cannot_satisfy_structured_output_however_it_is_configured() {
        let required = vec![STRUCTURED_OUTPUT_CAPABILITY.to_string()];

        // Both spellings the capability JSON supports — the bool key and the array form — so a
        // fix that reconciled only one of them is caught.
        for capabilities in [
            json!({ "structured_output": true }),
            json!({ "capabilities": ["structured_output"] }),
        ] {
            assert!(
                !capabilities_match(ProviderType::DeepSeek, &capabilities, &required),
                "a DeepSeek row must not satisfy structured_output: {capabilities}"
            );
        }
    }

    /// The other half of the same property: the reconciliation must not disqualify providers
    /// whose schema Rig really does send, or every structured request would lose its routing.
    #[test]
    fn the_providers_rig_sends_a_schema_for_still_satisfy_structured_output() {
        let required = vec![STRUCTURED_OUTPUT_CAPABILITY.to_string()];
        let capabilities = json!({ "structured_output": true });

        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatible,
            ProviderType::Local,
            ProviderType::AzureOpenAi,
            ProviderType::Anthropic,
            ProviderType::Gemini,
        ] {
            assert!(
                capabilities_match(provider_type, &capabilities, &required),
                "{provider_type:?} sends the schema and must stay eligible"
            );
        }
    }

    /// The reconciliation only ever subtracts, and only for the one key it owns.
    ///
    /// Two ways a plausible implementation goes wrong: reconciling *upward* (letting the
    /// provider type grant a capability the row denies), and applying the provider-type check to
    /// every capability rather than to `structured_output` alone. `vision` is the witness for
    /// the second — it is the only other key `public.rs` ever pushes.
    #[test]
    fn the_reconciliation_subtracts_only_and_touches_no_other_capability() {
        // Declared false stays false even where Rig would send the schema.
        assert!(
            !capabilities_match(
                ProviderType::OpenAi,
                &json!({ "structured_output": false }),
                &[STRUCTURED_OUTPUT_CAPABILITY.to_string()],
            ),
            "an operator's explicit false must survive the reconciliation"
        );

        // `vision` is unaffected on the provider whose structured output is dropped.
        assert!(
            capabilities_match(
                ProviderType::DeepSeek,
                &json!({ "vision": true }),
                &["vision".to_string()],
            ),
            "the reconciliation must not spill onto other capabilities"
        );

        // A DeepSeek row keeps every capability except the reconciled one.
        assert!(
            !capabilities_match(
                ProviderType::DeepSeek,
                &json!({ "vision": true, "structured_output": true }),
                &[
                    "vision".to_string(),
                    STRUCTURED_OUTPUT_CAPABILITY.to_string()
                ],
            ),
            "one unsatisfiable capability must disqualify the candidate"
        );
    }

    /// **Anti-rot tripwire for the `rig-core` pin.**
    ///
    /// `provider_emits_output_schema` reads Rig's own `SUPPORTS_RESPONSE_FORMAT` for every
    /// OpenAI-compatible arm, so a bump that changes Rig's behaviour changes Moira's silently
    /// and correctly. That is the right default, but "silently" also means nobody re-reads F39
    /// or the deliberately lenient F29 parse that depends on it. This test states rig 0.40.0's
    /// truth table literally, so a bump that moves any entry reds here and forces that read.
    ///
    /// A red in this test is **not** a defect: it means Rig changed. Verify the new constant in
    /// the vendored crate, update the expectation, and revisit the F29 reversal condition in
    /// `plans/reports/EXECUTION-LEDGER.md`.
    #[test]
    fn rig_0_40_still_drops_the_schema_for_deepseek_and_sends_it_for_everyone_else() {
        assert!(
            !provider_emits_output_schema(ProviderType::DeepSeek),
            "rig-core changed: DeepSeek now sends response_format — re-read finding F39"
        );
        assert!(
            !provider_emits_output_schema(ProviderType::Custom),
            "custom providers never construct a model, so no schema can reach a wire"
        );
        for provider_type in [
            ProviderType::OpenAi,
            ProviderType::OpenAiCompatible,
            ProviderType::Local,
            ProviderType::AzureOpenAi,
            ProviderType::Anthropic,
            ProviderType::Gemini,
        ] {
            assert!(
                provider_emits_output_schema(provider_type),
                "rig-core changed: {provider_type:?} no longer sends the schema — re-read F39"
            );
        }
    }

    /// **Finding F48 — a guard on the precondition, not on the behaviour.**
    ///
    /// `rig-core` 0.40's OpenAI encoder computes
    ///
    /// ```text
    /// should_apply_response_format =
    ///     output_schema.is_some() && supports_response_format
    ///     && (tools.is_empty() || history_has_tool_result)
    /// ```
    ///
    /// (`providers/openai/completion/mod.rs`). The third clause discards `output_schema` on
    /// **turn 1 of any tool-calling conversation, for every OpenAI-family provider**, and —
    /// unlike the `supports_response_format == false` path a few lines above it — emits **no
    /// `warn!` at all**. Rig documents the caveat itself (issue #1928) and Moira's own
    /// `composes_native_output_with_tools` comment restates it. **That behaviour is Rig's and is
    /// deliberately not changed here.**
    ///
    /// It cannot bite today: [`build_completion_request`] hardcodes `tools: Vec::new()`, and the
    /// public plane refuses caller-declared tools outright — `application/public.rs` answers
    /// `unsupported_tool` / *"client-defined tools are not registered in this phase"* before an
    /// `ExecutionCommand` is ever built. So the drop is unreachable, which is exactly why it
    /// would ship unnoticed.
    ///
    /// This case therefore guards the *precondition*. It hands the request Moira really builds
    /// to Rig's real encoder and asserts the schema survived onto the wire body. The moment
    /// `tools` stops being empty, Rig drops `response_format` and this reds — naming F48, so
    /// whoever enables tool calling confronts the silent drop instead of discovering it in
    /// production.
    ///
    /// **Three things this fixture does on purpose**, each answering "what is the cheapest edit
    /// that breaks the property while leaving the guard green?":
    ///
    /// 1. **The profile carries a `tool_policy`.** `AgentProfileRecord::tool_policy` is the
    ///    field a tool-calling implementation reads first, so the cheapest enabling edit is
    ///    "populate `tools` from the profile". A fixture passing `agent_profile: None` would
    ///    sail straight through it.
    /// 2. **Both `stream` settings.** `build_completion_request` ignores `stream` today; an
    ///    implementation that enabled tools on one path only would otherwise stay green.
    /// 3. **Rig's encoder, not a restatement of Rig's predicate.** Re-implementing
    ///    `should_apply_response_format` in the test module would be the F16 shape — a correct
    ///    predicate, tested against itself. `CompletionRequest::try_from((model, request))` is
    ///    Rig's own public conversion, configured (`supports_response_format: true`,
    ///    `supports_tools: true`) exactly as the OpenAI family is.
    ///
    /// **Measured, not assumed: this case is the only coverage of the *drop* in point 1.** The
    /// mutation above was run against the whole suite, and `tests/structured_output.rs` — which
    /// reads `response_format.type` off the body that actually reached a mock provider, and which
    /// looks like it should be the stronger guard — stayed **green** through it. At the time,
    /// every fixture in the tree left `route_definitions.agent_profile_id` NULL, so
    /// `agent_profile` was `None` on every integration path and no end-to-end test had ever
    /// built a request from a profile at all. (The column is on `route_definitions`;
    /// `RoutingPolicyRecord` has no such field, and F48's original wording said
    /// `routing_policies`. Corrected under F49.)
    ///
    /// **`tests/agent_profile_wire.rs` (finding F49) now closes that fixture hole, and it still
    /// does not replace this case.** Its
    /// `an_agent_profiles_tool_policy_does_not_become_a_tool_list_on_the_wire` does go red under
    /// the same mutation — but it reds saying *tools appeared on the wire*, which is not the
    /// dangerous half. No case in that file sends an `output_schema`, so none of them can
    /// observe `response_format` disappearing alongside the tools. This case is the only one
    /// that names the silent drop. Do not delete it in favour of either suite.
    ///
    /// The one gap that remains: a tool list attached to the request *after*
    /// `build_completion_request` returns, between the build and `handle.completion(request)`.
    /// That the wire tests would catch, because it needs no profile.
    #[test]
    fn moiras_request_still_carries_its_schema_onto_rigs_openai_wire_body() {
        use rig_core::providers::openai::completion::CompletionRequest as OpenAiWireRequest;

        let profile = AgentProfileRecord {
            id: Uuid::now_v7(),
            profile_key: "f48-guard".to_string(),
            display_name: "F48 guard".to_string(),
            preamble: Some("you are a guard".to_string()),
            temperature: Some(0.0),
            max_tokens: Some(64),
            // The field a tool-calling implementation reads first. See point 1 above.
            tool_policy: json!({
                "tools": [{
                    "name": "lookup",
                    "description": "look something up",
                    "parameters": { "type": "object", "properties": {} }
                }]
            }),
            context_policy: Value::Null,
            memory_policy: Value::Null,
            status: crate::domain::ResourceStatus::Active,
            metadata: Value::Null,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
            version: 1,
        };

        for stream in [false, true] {
            let command = ExecutionCommand {
                request_id: "f48".to_string(),
                execution_id: Uuid::now_v7(),
                identity: CallerRuntimeIdentity {
                    actor_type: format!("{:?}", ActorType::SystemKey),
                    subject: None,
                    external_user_id: None,
                    external_tenant_id: None,
                    application_id: None,
                    scopes: vec!["moira:admin".to_string()],
                },
                application_id: None,
                external_tenant_id: None,
                external_user_id: None,
                messages: vec![DomainMessage::user("hello")],
                route_hint: None,
                provider_hint: None,
                model_hint: None,
                credential_hint: None,
                options: ExecutionOptions {
                    stream,
                    output_schema: Some(json!({
                        "title": "Answer",
                        "type": "object",
                        "properties": { "a": { "type": "integer" } },
                        "required": ["a"]
                    })),
                    ..ExecutionOptions::default()
                },
                // Non-null so `additional_params` is already `Some`, which forces Rig's
                // *merge* branch rather than its plain-assignment branch.
                metadata: json!({ "moira": { "purpose": "f48_guard" } }),
            };

            let request = build_completion_request(&command, Some(&profile))
                .expect("the guard's own request must be buildable");
            assert!(
                request.output_schema.is_some(),
                "the fixture must actually carry a schema, or this guard proves nothing"
            );

            let wire = OpenAiWireRequest::try_from(("gpt-4o".to_string(), request))
                .expect("rig-core must encode a schema-carrying request");
            let params = wire.additional_params.unwrap_or(Value::Null);
            assert!(
                params.get("response_format").is_some(),
                "finding F48: rig-core dropped output_schema before the wire (stream={stream}). \
                 `should_apply_response_format` also requires `tools.is_empty() || \
                 history_has_tool_result`, so this is what a non-empty tool list looks like on \
                 turn 1 — and rig emits no warning for it. Enabling tool calling means deciding \
                 what happens to structured output on turn 1 first; see F48 in \
                 plans/reports/EXECUTION-LEDGER.md. Encoded params: {params}"
            );
        }
    }

    #[test]
    fn provider_credential_types_match_runtime_factory_support() {
        use crate::domain::CredentialType;

        assert_eq!(
            supported_credential_types(ProviderType::Anthropic),
            &[CredentialType::ApiKey]
        );
        assert_eq!(
            supported_credential_types(ProviderType::OpenAiCompatible),
            &[CredentialType::ApiKey, CredentialType::BearerToken]
        );
    }

    #[test]
    fn timeout_after_stream_output_cannot_retry_or_fallback() {
        let failure = attempt_timeout_failure(false, true);
        assert_eq!(failure.class, ExecutionFailureClass::ProviderTimeout);
        assert!(!failure.retryable);
        assert!(!failure.fallback_eligible);

        let deadline = attempt_timeout_failure(true, true);
        assert_eq!(deadline.class, ExecutionFailureClass::DeadlineExceeded);
        assert!(!deadline.retryable);
        assert!(!deadline.fallback_eligible);
    }

    #[test]
    fn admin_scope_allows_runtime_overrides_for_non_consumers() {
        let command = ExecutionCommand {
            request_id: "req".to_string(),
            execution_id: Uuid::now_v7(),
            identity: CallerRuntimeIdentity {
                actor_type: format!("{:?}", ActorType::SystemKey),
                subject: None,
                external_user_id: None,
                external_tenant_id: None,
                application_id: None,
                scopes: vec!["moira:admin".to_string()],
            },
            application_id: None,
            external_tenant_id: None,
            external_user_id: None,
            messages: vec![DomainMessage::user("hello")],
            route_hint: Some("general".to_string()),
            provider_hint: Some(Uuid::now_v7()),
            model_hint: Some(Uuid::now_v7()),
            credential_hint: Some(Uuid::now_v7()),
            options: ExecutionOptions::default(),
            metadata: Value::Null,
        };
        assert!(has_runtime_scope(
            &command,
            "moira:execution:override-model"
        ));
    }

    #[test]
    fn consumer_admin_scope_does_not_allow_overrides() {
        let command = ExecutionCommand {
            request_id: "req".to_string(),
            execution_id: Uuid::now_v7(),
            identity: CallerRuntimeIdentity {
                actor_type: format!("{:?}", ActorType::ConsumerKey),
                subject: None,
                external_user_id: None,
                external_tenant_id: None,
                application_id: None,
                scopes: vec!["moira:admin".to_string()],
            },
            application_id: None,
            external_tenant_id: None,
            external_user_id: None,
            messages: vec![DomainMessage::user("hello")],
            route_hint: None,
            provider_hint: None,
            model_hint: None,
            credential_hint: None,
            options: ExecutionOptions::default(),
            metadata: Value::Null,
        };
        assert!(!has_runtime_scope(
            &command,
            "moira:execution:override-model"
        ));
    }

    #[test]
    fn remaining_execution_time_is_none_once_the_deadline_has_passed() {
        let past = Instant::now() - Duration::from_millis(1);
        assert!(remaining_execution_time(past).is_none());

        let exactly_now = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        assert!(remaining_execution_time(exactly_now).is_none());

        let future = Instant::now() + Duration::from_secs(30);
        assert!(remaining_execution_time(future).is_some());
    }

    #[test]
    fn remaining_execution_time_shrinks_monotonically_across_successive_phases() {
        let deadline = Instant::now() + Duration::from_secs(30);

        let before_credential = remaining_execution_time(deadline).expect("budget at phase 1");
        std::thread::sleep(Duration::from_millis(5));
        let before_runtime_handle = remaining_execution_time(deadline).expect("budget at phase 2");
        std::thread::sleep(Duration::from_millis(5));
        let before_terminal_persistence =
            remaining_execution_time(deadline).expect("budget at phase 3");

        assert!(
            before_runtime_handle < before_credential,
            "phase 2 must recompute the budget, not reuse phase 1's"
        );
        assert!(
            before_terminal_persistence < before_runtime_handle,
            "phase 3 must recompute the budget, not reuse phase 2's"
        );
    }

    #[test]
    fn phase_budget_is_the_minimum_of_remaining_budget_and_phase_timeout() {
        assert_eq!(
            phase_budget(Duration::from_millis(200), Duration::from_millis(5_000)),
            Duration::from_millis(200),
            "the total deadline must win when it is the tighter bound"
        );
        assert_eq!(
            phase_budget(Duration::from_millis(5_000), Duration::from_millis(200)),
            Duration::from_millis(200),
            "the per-phase timeout must win when it is the tighter bound"
        );
        assert_eq!(
            phase_budget(Duration::from_millis(200), Duration::from_millis(200)),
            Duration::from_millis(200)
        );
    }

    #[test]
    fn terminal_persistence_timeout_maps_to_the_output_committed_failure_class() {
        let failure = terminal_persistence_deadline_failure();
        let plain = deadline_failure();

        assert_eq!(failure.class, ExecutionFailureClass::DeadlineExceeded);
        assert!(
            !failure.retryable,
            "committed output must never be re-executed"
        );
        assert!(
            !failure.fallback_eligible,
            "committed output must never be sent to a fallback provider"
        );
        assert_ne!(
            failure.message, plain.message,
            "a terminal-persistence breach must be distinguishable from a plain deadline failure"
        );

        // The clamp is inherited from the existing output-committed pattern, not reinvented.
        let inherited = attempt_timeout_failure(true, true);
        assert_eq!(failure.class, inherited.class);
        assert_eq!(failure.retryable, inherited.retryable);
        assert_eq!(failure.fallback_eligible, inherited.fallback_eligible);
    }

    /// Finding F29 — the gate, stated as a unit fact.
    ///
    /// The three integration cases that cover this
    /// (`tests/structured_output.rs`, and `a_summary_that_is_valid_json_is_stored_verbatim`)
    /// each need a database, a mock provider and an HTTP server. This one needs none of them,
    /// so the gate cannot become unobservable if a fixture stops being reachable.
    #[test]
    fn structured_output_is_parsed_only_when_a_schema_was_requested() {
        let parsed = |wants: bool, text: &str| {
            structured_output_from_text(wants, text).expect("must not fail for this input")
        };

        // The property: identical bytes, opposite results, decided solely by the flag.
        assert_eq!(parsed(true, "{\"a\":1}"), Some(json!({ "a": 1 })));
        assert_eq!(parsed(false, "{\"a\":1}"), None);

        // The corruption shape easiest to reach in practice, and the reason the flag is not
        // merely an optimisation: a model that wraps its prose reply in quotes has emitted a
        // valid JSON *string*. Ungated, summarization would store `"\"…\""` — the quotes, and
        // any interior escaping, now part of the body and of `summary_hash` with it.
        // Summarization sends no schema, so it takes the second branch.
        assert_eq!(
            parsed(true, "\"a quoted summary\""),
            Some(json!("a quoted summary"))
        );
        assert_eq!(parsed(false, "\"a quoted summary\""), None);

        // Whitespace is trimmed before the parse, and only around the document.
        assert_eq!(parsed(true, "  \n{\"a\":1}\n  "), Some(json!({ "a": 1 })));

        // **The gate is what decides, not the bytes.** With no schema requested, a reply that
        // could never parse is still `Ok(None)` rather than a failure — summarization sends no
        // schema and every reply it gets is prose, so a failure here would break every
        // summarization in the tree.
        assert_eq!(parsed(false, "I cannot do that."), None);
    }

    /// Issue #80 — the flip, stated as a unit fact, and its boundary.
    ///
    /// The integration cases in `tests/structured_output.rs` each need a database, a mock
    /// provider and an HTTP server; this one needs none of them, so the property stays observable
    /// even if a fixture stops being reachable. It is separate from the gate test above because
    /// the two answer different questions — "was a schema asked for" and "what happens when the
    /// reply does not parse" — and a single test that went red would not say which.
    #[test]
    fn a_schema_carrying_reply_that_is_not_json_is_a_failure_rather_than_a_none() {
        for reply in [
            "I cannot do that.",
            // Empty is the case worth naming: it is what a model sends when it has nothing to
            // say, and it is **not** an empty *result*. A schema-carrying request that wants to
            // answer "nothing" answers `null`, `{}` or `[]` — all three parse, and all three are
            // asserted below as successes. Zero bytes are not a JSON document at all, so they
            // fail, which is the same answer the caller would otherwise have had to infer from a
            // 200 with a null field.
            "",
            // Strict, not a scavenger. Both of these are what Rig's balanced-brace scan and
            // `memory_extraction::strip_code_fence` would accept. Neither is accepted here: what
            // counts as a parse is exactly what it was before the flip, because the decision was
            // about what happens on a failed parse, not about widening the accept-set.
            "here you go: {\"a\":1}",
            "```json\n{\"a\":1}\n```",
        ] {
            let failure = structured_output_from_text(true, reply)
                .expect_err("a schema-carrying reply that is not JSON must fail");
            assert_eq!(
                failure.class,
                ExecutionFailureClass::StructuredOutputInvalid,
                "reply {reply:?}"
            );
            assert!(
                !failure.retryable && !failure.fallback_eligible,
                "the disposition in src/orchestration/controls.rs must reach the failure this \
                 site constructs: {failure:?}"
            );
            assert!(
                !failure.message.contains(reply.trim()) || reply.trim().is_empty(),
                "the failure message must not echo the provider's reply: {}",
                failure.message
            );
        }

        // The boundary the decision draws. An *empty answer* is an answer: all three of these
        // parse, so they still succeed and are reported as values rather than as failures.
        assert_eq!(
            structured_output_from_text(true, "null").expect("null is a JSON document"),
            Some(Value::Null)
        );
        assert_eq!(
            structured_output_from_text(true, "{}").expect("{} is a JSON document"),
            Some(json!({}))
        );
        assert_eq!(
            structured_output_from_text(true, "[]").expect("[] is a JSON document"),
            Some(json!([]))
        );
    }

    #[tokio::test]
    async fn zero_or_negative_remaining_budget_never_produces_an_unbounded_timeout() {
        let expired = Instant::now() - Duration::from_secs(1);

        // A pre-attempt phase with no budget left fails closed instead of running unbounded.
        //
        // The assertion is itself bounded. Without the guard the only symptom of a
        // regression here is `bounded_phase` awaiting `pending` forever, which in CI reads
        // as a job timeout — infrastructure flakiness — rather than as a caught regression.
        // The guard turns that into a fast, legible test failure.
        let never_completes = std::future::pending::<Result<(), ExecutionFailure>>();
        let failure = tokio::time::timeout(
            UNBOUNDED_PHASE_GUARD,
            bounded_phase(expired, never_completes),
        )
        .await
        .expect("bounded_phase must fail closed on an expired deadline, not await the phase")
        .expect_err("an expired deadline must not admit a new phase");
        assert_eq!(failure.class, ExecutionFailureClass::DeadlineExceeded);

        // Terminal persistence is floored, so it is bounded and non-zero, never "no limit".
        let budget = terminal_persistence_budget(expired);
        assert_eq!(budget, TERMINAL_PERSISTENCE_MIN_BUDGET);
        assert!(!budget.is_zero());

        // And `Duration::ZERO` really does mean "already elapsed" to tokio, not "no limit".
        assert!(
            tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn bounded_phase_passes_through_a_phase_that_finishes_inside_its_budget() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let value = bounded_phase(deadline, async { Ok::<u8, ExecutionFailure>(7) })
            .await
            .expect("a fast phase must not be cut short");
        assert_eq!(value, 7);

        let failure = bounded_phase(deadline, async {
            Err::<u8, ExecutionFailure>(ExecutionFailure::new(
                ExecutionFailureClass::CredentialNotFound,
                "no eligible provider credential",
            ))
        })
        .await
        .expect_err("the phase's own failure must survive the wrapper");
        assert_eq!(failure.class, ExecutionFailureClass::CredentialNotFound);
        assert!(
            failure.fallback_eligible,
            "wrapping must not flatten a fallback-eligible failure into a deadline failure"
        );
    }

    #[tokio::test]
    async fn terminal_persistence_budget_uses_the_live_remaining_budget_when_it_exceeds_the_floor()
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        let budget = terminal_persistence_budget(deadline);
        assert!(budget > TERMINAL_PERSISTENCE_MIN_BUDGET);
        assert!(budget <= Duration::from_secs(30));
    }

    #[tokio::test]
    async fn router_builds_without_database_for_type_checking() {
        let state = AppState::new(Settings::default(), None).await.unwrap();
        assert!(MoiraExecutionService::new(state).is_err());
    }
}
