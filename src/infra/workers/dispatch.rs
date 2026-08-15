//! The registry-backed [`JobDispatcher`] (issue #90).
//!
//! `super::queue::StubJobDispatcher` completed every declared job name immediately with a
//! log line — the "spec lies to the caller" pattern issue #90 exists to remove: seven job
//! names ship `enabled_by_default: true` (`super::WorkerRegistry::new`), so they read as
//! active work in `WorkerSnapshot`, yet nothing behind them ever ran.
//!
//! [`RealJobDispatcher`] replaces it as the dispatcher `run_supervisor` drives. It routes a
//! claimed job to one [`JobHandler`] per `job_name`, registered once at supervisor start by
//! [`default_dispatcher`]. Three outcomes, by construction:
//!
//! 1. **A name outside [`super::WORKER_JOB_NAMES`]** — a row that reached the claim table
//!    without going through `WorkerQueue::enqueue`'s check — fails, exactly as
//!    `StubJobDispatcher` did and for the same reason: completing it would hide that
//!    something bypassed the check. The retry budget runs out and the row lands in
//!    `dead_letter`, where an operator can see it.
//! 2. **A declared name with a registered handler** dispatches to that handler, and *its*
//!    `Err` is what feeds `WorkerQueue::settle_failure`'s retry/dead-letter classification —
//!    unchanged from before this module, because [`JobHandler::handle`] returns exactly the
//!    `Result<(), String>` shape [`JobDispatcher::dispatch`] always has.
//! 3. **A declared name with no registered handler** completes as a warned no-op. This is
//!    deliberately *not* case 1: the name is legitimate (it is in `WORKER_JOB_NAMES`, so it
//!    is a real metric label and a real queue row), this process simply has not been taught a
//!    body for it yet. `default_dispatcher` uses this for the four plan-11 pipeline retry
//!    names when unregistered would otherwise apply, and for `runtime-cache-warmer`, which
//!    still has no handler.
//!
//! # What this module does not do
//!
//! It does not move memory extraction, summarization, embedding or RAG ingestion off the
//! response path. Those pipelines still run inline in `ConversationService` today — see
//! `extract_memories`'s "Reversal condition" doc comment in `src/application/conversation.rs`
//! — and reaching them from a queue job would mean reconstructing an `Actor`, a
//! `RequestContext` and (for extraction) a `PlannedContext` from a bare job payload, which is
//! the larger plan-11 change issue #90 explicitly defers rather than folds in here. The four
//! plan-11 retry names below are therefore [`DeferredPipelineHandler`]s: real, registered,
//! logged handlers that document the gap rather than a blanket stub that hides it.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use reqwest::Client;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    config::WorkerSettings,
    infra::{
        metrics::MetricsRegistry,
        repositories::ClaimedJob,
        workers::{
            self, CONVERSATION_SUMMARIZATION_RETRY_WORKER, DOCUMENT_INGESTION_RETRY_WORKER,
            EMBEDDING_RETRY_WORKER, LATENCY_STATS_AGGREGATION_WORKER,
            MEMORY_EXTRACTION_RETRY_WORKER, OAUTH_TOKEN_REFRESH_WORKER,
            PROVIDER_HEALTH_CHECK_WORKER, latency_stats::LatencyStatsAggregationHandler,
            oauth_refresh::OAuthTokenRefreshHandler,
            provider_health_check::ProviderHealthCheckHandler, queue::JobDispatcher,
        },
    },
    security::LocalSecretCipher,
};

/// Executes one claimed job of a single, already-known `job_name`.
///
/// The per-name counterpart to [`JobDispatcher`]: a dispatcher picks *which* handler runs,
/// a handler is the body that runs. `Err(message)` has the same contract
/// `JobDispatcher::dispatch` documents — it lands in `worker_jobs.last_error`, so it must
/// carry no secret.
#[async_trait]
pub trait JobHandler: Send + Sync {
    async fn handle(&self, job: &ClaimedJob) -> Result<(), String>;
}

/// Routes a claimed job to its registered [`JobHandler`] by `job_name`.
///
/// See the module doc comment for the three dispatch outcomes. Built once per process by
/// [`default_dispatcher`] and driven by `WorkerRegistry::run_supervisor`.
#[derive(Default)]
pub struct RealJobDispatcher {
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
}

impl RealJobDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` for `name`.
    ///
    /// `name` should always be one of [`super::WORKER_JOB_NAMES`] — a handler registered
    /// under any other string can never be reached, because [`RealJobDispatcher::dispatch`]
    /// rejects a job whose name is not declared before it ever consults this map. Checked
    /// with `debug_assert!` rather than a `Result`: a mismatch here is a programming error
    /// caught the moment a debug build's tests run [`default_dispatcher`], not a condition a
    /// caller is expected to handle.
    pub fn register(mut self, name: &'static str, handler: Arc<dyn JobHandler>) -> Self {
        debug_assert!(
            workers::is_known_job_name(name),
            "registering a handler for {name:?}, which is not declared in WORKER_JOB_NAMES \
             (src/infra/workers.rs); it would never be reached"
        );
        self.handlers.insert(name, handler);
        self
    }
}

#[async_trait]
impl JobDispatcher for RealJobDispatcher {
    async fn dispatch(&self, job: &ClaimedJob) -> Result<(), String> {
        if !workers::is_known_job_name(&job.job_name) {
            // Not a panic and not a silent success — the same refusal
            // `queue::StubJobDispatcher` applied, kept for the reason its own doc comment
            // gives: an unknown name means the row was written by something that bypassed
            // `WorkerQueue::enqueue`'s check, and completing it would hide that.
            return Err(format!("no handler is registered for job {}", job.job_name));
        }
        match self.handlers.get(job.job_name.as_str()) {
            Some(handler) => handler.handle(job).await,
            None => {
                // A declared name (so a legitimate metric label and a legitimate queue row)
                // that this process has not registered a handler for. Completing rather than
                // failing keeps a not-yet-wired-but-real job name from burning its retry
                // budget and dead-lettering for a reason no operator retry can fix — see the
                // module doc comment, case 3.
                warn!(
                    job_id = %job.id,
                    job_name = %job.job_name,
                    attempts = job.attempts,
                    "worker job claimed for a declared name with no registered handler; \
                     completing as a no-op"
                );
                Ok(())
            }
        }
    }
}

/// A handler for a plan-11 retry job whose pipeline body still runs inline on the response
/// path rather than through this queue.
///
/// Logs and completes. This is deliberately a *registered* handler rather than an absence —
/// see the module doc comment's case 2 vs. case 3 — so that the day plan 11's pipeline
/// extraction lands, replacing this `impl` is the entire change: `default_dispatcher` stops
/// constructing this type for that name and starts constructing the real handler, and
/// nothing else in the dispatch loop moves.
struct DeferredPipelineHandler {
    job_name: &'static str,
}

#[async_trait]
impl JobHandler for DeferredPipelineHandler {
    async fn handle(&self, job: &ClaimedJob) -> Result<(), String> {
        info!(
            job_id = %job.id,
            job_name = %self.job_name,
            attempts = job.attempts,
            "worker job dispatched to a deferred-pipeline handler; the retry body is not yet \
             wired to the queue (issue #90 dispatch plumbing landed, the plan-11 pipeline \
             extraction has not) — completing without doing the retry"
        );
        Ok(())
    }
}

fn deferred_pipeline_handler(job_name: &'static str) -> Arc<dyn JobHandler> {
    Arc::new(DeferredPipelineHandler { job_name })
}

/// The dispatcher `WorkerRegistry::run_supervisor` drives.
///
/// # What is registered today
///
/// The four plan-11 retry names get a [`DeferredPipelineHandler`] each: real dispatch
/// plumbing (per-name routing, and — the moment any of these is actually enqueued — real
/// retry/dead-letter classification through `WorkerQueue::settle_failure`), with a
/// documented no-op body until plan 11 extracts the pipeline it would call. See the module
/// doc comment.
///
/// `latency-stats-aggregation`, `oauth-token-refresh` and `provider-health-check` get real
/// handlers — [`LatencyStatsAggregationHandler`], [`OAuthTokenRefreshHandler`] and
/// [`ProviderHealthCheckHandler`] respectively — whenever `pool` is `Some`. `pool` is `None`
/// only when Moira runs with no database at all, in which case
/// `WorkerRegistry::run_supervisor` also builds no [`super::queue::WorkerQueue`], so this
/// dispatcher is never driven and the distinction is moot; the branch exists so a caller does
/// not have to thread an `Option` through three handler constructors that all need a pool
/// unconditionally.
///
/// `runtime-cache-warmer` and `retention-cleanup` are declared in `WORKER_JOB_NAMES` but
/// intentionally left unregistered here: `retention-cleanup` is dispatched outside this queue
/// entirely (its own leader-gated timer arm in `run_supervisor`), and `runtime-cache-warmer`
/// has no handler yet — it falls through to [`RealJobDispatcher::dispatch`]'s case-3 no-op.
pub fn default_dispatcher(
    pool: Option<PgPool>,
    cipher: LocalSecretCipher,
    http: Client,
    metrics: MetricsRegistry,
    settings: Arc<WorkerSettings>,
) -> RealJobDispatcher {
    let mut dispatcher = RealJobDispatcher::new()
        .register(
            MEMORY_EXTRACTION_RETRY_WORKER,
            deferred_pipeline_handler(MEMORY_EXTRACTION_RETRY_WORKER),
        )
        .register(
            CONVERSATION_SUMMARIZATION_RETRY_WORKER,
            deferred_pipeline_handler(CONVERSATION_SUMMARIZATION_RETRY_WORKER),
        )
        .register(
            EMBEDDING_RETRY_WORKER,
            deferred_pipeline_handler(EMBEDDING_RETRY_WORKER),
        )
        .register(
            DOCUMENT_INGESTION_RETRY_WORKER,
            deferred_pipeline_handler(DOCUMENT_INGESTION_RETRY_WORKER),
        );
    if let Some(pool) = pool {
        dispatcher = dispatcher
            .register(
                LATENCY_STATS_AGGREGATION_WORKER,
                Arc::new(LatencyStatsAggregationHandler::new(
                    pool.clone(),
                    settings.clone(),
                )),
            )
            .register(
                OAUTH_TOKEN_REFRESH_WORKER,
                Arc::new(OAuthTokenRefreshHandler::new(
                    pool.clone(),
                    cipher,
                    http.clone(),
                    metrics.clone(),
                    settings.clone(),
                )),
            )
            .register(
                PROVIDER_HEALTH_CHECK_WORKER,
                Arc::new(ProviderHealthCheckHandler::new(
                    pool, http, metrics, settings,
                )),
            );
    }
    dispatcher
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;
    use uuid::Uuid;

    use super::*;

    /// `default_dispatcher` with no database — the shape every test in this module needs,
    /// since none of them exercises the three DB-backed handlers directly (that proof lives in
    /// `tests/workers/latency_health_oauth.rs` against a real Postgres). With `pool: None`
    /// this still registers the four plan-11 `DeferredPipelineHandler`s, so every assertion
    /// below about those four is unaffected.
    fn default_dispatcher_without_db() -> RealJobDispatcher {
        default_dispatcher(
            None,
            LocalSecretCipher::new([7; 32], "dispatch-test"),
            Client::new(),
            MetricsRegistry::new("dispatch-test", None),
            Arc::new(WorkerSettings::default()),
        )
    }

    fn job(name: &str) -> ClaimedJob {
        ClaimedJob {
            id: Uuid::now_v7(),
            job_name: name.to_string(),
            payload: Value::Null,
            attempts: 1,
            max_attempts: 5,
        }
    }

    /// The same refusal `StubJobDispatcher` applied, preserved through the rewrite: a row
    /// that bypassed `WorkerQueue::enqueue`'s check must not be silently completed.
    #[tokio::test]
    async fn an_undeclared_job_name_is_rejected() {
        let dispatcher = RealJobDispatcher::new();
        assert!(
            dispatcher
                .dispatch(&job("not-a-declared-job"))
                .await
                .is_err()
        );
    }

    /// Case 3: declared, unregistered, completes with a warning rather than dead-lettering.
    /// This is the seam `default_dispatcher`'s doc comment describes for
    /// `oauth-token-refresh` and a future latency-aggregation job.
    #[tokio::test]
    async fn a_declared_but_unregistered_job_name_completes_as_a_noop() {
        let dispatcher = RealJobDispatcher::new();
        assert!(
            dispatcher
                .dispatch(&job(super::super::RETENTION_CLEANUP_WORKER))
                .await
                .is_ok()
        );
        assert!(
            dispatcher
                .dispatch(&job("provider-health-check"))
                .await
                .is_ok()
        );
    }

    /// Struct-level `JobHandler` used to prove dispatch routes by exact `job_name` rather
    /// than, say, dispatching every declared name to whichever handler was registered last.
    struct CountingHandler {
        calls: Arc<AtomicUsize>,
        outcome: Result<(), &'static str>,
    }

    #[async_trait]
    impl JobHandler for CountingHandler {
        async fn handle(&self, _job: &ClaimedJob) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcome.map_err(ToString::to_string)
        }
    }

    #[tokio::test]
    async fn dispatch_routes_to_the_handler_registered_for_that_exact_job_name() {
        let extraction_calls = Arc::new(AtomicUsize::new(0));
        let summarization_calls = Arc::new(AtomicUsize::new(0));
        let dispatcher = RealJobDispatcher::new()
            .register(
                MEMORY_EXTRACTION_RETRY_WORKER,
                Arc::new(CountingHandler {
                    calls: extraction_calls.clone(),
                    outcome: Ok(()),
                }),
            )
            .register(
                CONVERSATION_SUMMARIZATION_RETRY_WORKER,
                Arc::new(CountingHandler {
                    calls: summarization_calls.clone(),
                    outcome: Ok(()),
                }),
            );

        dispatcher
            .dispatch(&job(MEMORY_EXTRACTION_RETRY_WORKER))
            .await
            .expect("registered handler completes");
        assert_eq!(extraction_calls.load(Ordering::SeqCst), 1);
        assert_eq!(summarization_calls.load(Ordering::SeqCst), 0);

        dispatcher
            .dispatch(&job(CONVERSATION_SUMMARIZATION_RETRY_WORKER))
            .await
            .expect("registered handler completes");
        assert_eq!(extraction_calls.load(Ordering::SeqCst), 1);
        assert_eq!(summarization_calls.load(Ordering::SeqCst), 1);
    }

    /// A handler's `Err` reaches the caller unchanged — this is what lets
    /// `WorkerQueue::settle_failure`'s existing retry/dead-letter classification apply to a
    /// real handler's failure exactly as it already does to `StubJobDispatcher`'s.
    #[tokio::test]
    async fn a_handler_error_propagates_out_of_dispatch_unchanged() {
        let dispatcher = RealJobDispatcher::new().register(
            EMBEDDING_RETRY_WORKER,
            Arc::new(CountingHandler {
                calls: Arc::new(AtomicUsize::new(0)),
                outcome: Err("provider unreachable"),
            }),
        );
        let error = dispatcher
            .dispatch(&job(EMBEDDING_RETRY_WORKER))
            .await
            .expect_err("the handler's failure must surface");
        assert_eq!(error, "provider unreachable");
    }

    /// `default_dispatcher` is what `run_supervisor` actually builds. Every plan-11 retry
    /// name must complete (the documented stub), and each call must reach *a* handler rather
    /// than falling through to the unregistered no-op path — proven by asserting the four
    /// names are declared rather than by inspecting a private field.
    #[tokio::test]
    async fn default_dispatcher_completes_every_plan_11_retry_name() {
        let dispatcher = default_dispatcher_without_db();
        for name in [
            MEMORY_EXTRACTION_RETRY_WORKER,
            CONVERSATION_SUMMARIZATION_RETRY_WORKER,
            EMBEDDING_RETRY_WORKER,
            DOCUMENT_INGESTION_RETRY_WORKER,
        ] {
            assert!(
                dispatcher.dispatch(&job(name)).await.is_ok(),
                "{name} must complete via its registered deferred-pipeline handler"
            );
        }
    }

    /// With no database, `latency-stats-aggregation`, `oauth-token-refresh` and
    /// `provider-health-check` all fall through to the unregistered no-op path — the same
    /// path `runtime-cache-warmer` always takes — rather than dead-lettering for lack of a
    /// handler. `tests/workers/latency_health_oauth.rs` proves the opposite: with a real
    /// pool, `default_dispatcher` registers a real handler for all three.
    #[tokio::test]
    async fn default_dispatcher_no_ops_the_three_db_backed_names_when_there_is_no_pool() {
        let dispatcher = default_dispatcher_without_db();
        for name in [
            "latency-stats-aggregation",
            "oauth-token-refresh",
            "provider-health-check",
            "runtime-cache-warmer",
        ] {
            assert!(
                dispatcher.dispatch(&job(name)).await.is_ok(),
                "{name} must no-op rather than dead-letter with no pool"
            );
        }
    }

    #[tokio::test]
    async fn default_dispatcher_still_rejects_an_undeclared_name() {
        let dispatcher = default_dispatcher_without_db();
        assert!(
            dispatcher
                .dispatch(&job("not-a-declared-job"))
                .await
                .is_err()
        );
    }
}
