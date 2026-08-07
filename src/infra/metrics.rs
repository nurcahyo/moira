//! Moira's Prometheus surface.
//!
//! # Why the `metrics` facade instead of hand-rolled exposition
//!
//! Cumulative-bucket semantics (an observation increments every bucket at or above its
//! value), `le="+Inf"` handling, Prometheus label escaping and the `_bucket`/`_sum`/`_count`
//! exposition layout all come from a tested library instead of being re-solved in-tree,
//! where each one is an easy and *silent* bug. The `metrics` instrumentation macros also
//! keep the recording call sites free of formatting concerns.
//!
//! # Recorder ownership — deliberately per-registry, never process-global
//!
//! [`PrometheusBuilder::install`] / [`metrics::set_global_recorder`] are **not** used. The
//! integration suite builds multiple `AppState`s inside one test process; a process-global
//! recorder would merge series across unrelated app instances and would panic on the second
//! install. Instead each [`MetricsRegistry`] owns its own [`PrometheusRecorder`] plus that
//! recorder's [`PrometheusHandle`], both behind the same `Arc<MetricsInner>` the registry
//! already used, so the registry stays `Clone + Send + Sync`. Every `record_*` method wraps
//! its macro calls in [`metrics::with_local_recorder`]; each closure is synchronous, so the
//! thread-local the helper installs is always live for the duration of the recording call.
//!
//! **No `metrics::*` macro is called anywhere outside this module.** Call sites only see the
//! `record_*` methods.
//!
//! # Upkeep
//!
//! `build_recorder()` does not spawn the upkeep task that `install()` would. That is correct
//! here: no idle timeout is configured, so `recency_mask` is `MetricKindMask::NONE` and no
//! series is ever expired, and `PrometheusHandle::render` drains pending histogram samples
//! into their distributions itself. Nothing is left un-run.
//!
//! # Cardinality is the security-relevant property
//!
//! The exporter escapes label *values* correctly, which is why the old hand-rolled
//! `sanitize_label_value` could be deleted — but escaping is not cardinality control: the
//! exporter will happily render a UUID. An unbounded label set is a memory-exhaustion vector
//! in the scrape path. The discipline therefore rests on two rules, both test-guarded below:
//!
//! 1. HTTP route labels come from [`axum::extract::MatchedPath`] (a route *template*), never
//!    from the raw URI.
//! 2. Every other label value comes from a closed set — a domain enum, an HTTP status class,
//!    a known method, or an admin-configured provider/model identifier — never from caller
//!    input and never from provider error text.

use std::{sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use metrics::{
    counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
    with_local_recorder,
};
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use sqlx::PgPool;

use crate::{
    domain::{ExecutionFailureClass, ExecutionStatus, ProviderType},
    infra::workers::{
        LEADER_GATED_WORKERS, WORKER_JOB_NAMES,
        retention::{TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES},
    },
};

// ---------------------------------------------------------------------------------------
// Metric family names.
//
// Every name in the first block existed before the exporter swap and is re-emitted verbatim,
// so no scraper breaks. The second block is new and purely additive.
// ---------------------------------------------------------------------------------------

const HTTP_REQUESTS_TOTAL: &str = "moira_http_requests_total";
const HTTP_STATUS_CLASS_TOTAL: &str = "moira_http_response_status_class_total";
const HTTP_LATENCY_MICROS_TOTAL: &str = "moira_http_latency_micros_total";
const PUBLIC_RESPONSES_CREATED_TOTAL: &str = "moira_public_responses_created_total";
const PUBLIC_STREAMS_STARTED_TOTAL: &str = "moira_public_streams_started_total";
const WORKER_TICKS_TOTAL: &str = "moira_worker_ticks_total";
const RETENTION_RUNS_TOTAL: &str = "moira_retention_runs_total";
const RETENTION_ROWS_DELETED_TOTAL: &str = "moira_retention_rows_deleted_total";
const REDIS_ENABLED: &str = "moira_redis_enabled";
const WORKERS_ENABLED: &str = "moira_workers_enabled";

const HTTP_REQUEST_DURATION_SECONDS: &str = "moira_http_request_duration_seconds";
const EXECUTION_DURATION_SECONDS: &str = "moira_execution_duration_seconds";
const EXECUTION_TTFT_SECONDS: &str = "moira_execution_ttft_seconds";
const PROVIDER_OUTCOME_TOTAL: &str = "moira_provider_outcome_total";
const DB_POOL_CONNECTIONS: &str = "moira_db_pool_connections";

// Plan 10 wave 2. Every one is seeded at zero below: the exporter renders only
// families that have been registered, so an unseeded family is a *removal* from
// the scrape body until the first observation, and an alert written against it
// fires on "no data" rather than on the condition it was written for.
const WORKER_JOBS_CLAIMED_TOTAL: &str = "moira_worker_jobs_claimed_total";
const WORKER_JOBS_COMPLETED_TOTAL: &str = "moira_worker_jobs_completed_total";
const WORKER_JOBS_FAILED_TOTAL: &str = "moira_worker_jobs_failed_total";
const WORKER_JOBS_DEAD_LETTER_TOTAL: &str = "moira_worker_jobs_dead_letter_total";
const WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL: &str = "moira_worker_queue_enqueue_rejected_total";
const WORKER_LEADER_HELD: &str = "moira_worker_leader_held";
const REDIS_OPERATION_FAILURES_TOTAL: &str = "moira_redis_operation_failures_total";
const RUNTIME_INVALIDATIONS_TOTAL: &str = "moira_runtime_invalidations_total";

// Plan 11 wave 1. Seeded for the same reason as the wave-2 families above: an alert on
// "chunks stopped being written" must fire on the condition, not on "no data".
//
// All three are label-free or closed-label by construction. There is deliberately no
// `application_id`, `collection_id` or `document_id` label anywhere here — per-application
// ingestion diagnostics belong in `rag_ingestion_runs` rows, which is what that table is
// for, and an application id as a label value is the unbounded-cardinality scrape-path
// memory-exhaustion vector this module's header describes.
const RAG_CHUNKS_WRITTEN_TOTAL: &str = "moira_rag_chunks_written_total";
const RAG_EMBEDDINGS_WRITTEN_TOTAL: &str = "moira_rag_embeddings_written_total";
const RAG_INGESTION_RUNS_TOTAL: &str = "moira_rag_ingestion_runs_total";
const EMBEDDING_BATCH_LATENCY_SECONDS: &str = "moira_embedding_batch_latency_seconds";

// Plan 11 wave 2. Same rules: closed label set, counter seeded, histogram not.
const RETRIEVAL_RUNS_TOTAL: &str = "moira_retrieval_runs_total";
const RETRIEVAL_LATENCY_SECONDS: &str = "moira_retrieval_latency_seconds";

// Plan 11 Sub-Phase F — automatic memory extraction.
//
// Three families, and the split is deliberate. `..._runs_total` reuses the shared
// `succeeded`/`failed` `outcome` domain and adds no new label *value*, exactly as
// `RAG_INGESTION_RUNS_TOTAL` does. The other two are **label-free**: the interesting breakdown
// is *why* a candidate was refused, and that is per-application, per-policy detail which
// `ALLOWED_LABEL_KEYS` admits no dimension for. It goes on `memory_extraction_runs.metadata`,
// which is the row this counter is the aggregate of.
//
// Written at all because extraction is invisible by construction: it never surfaces an error to
// the caller, so without these families "extraction silently stopped writing memories" and
// "nobody has enabled extraction" look identical on a dashboard.
const MEMORY_EXTRACTION_RUNS_TOTAL: &str = "moira_memory_extraction_runs_total";
const MEMORY_EXTRACTION_WRITTEN_TOTAL: &str = "moira_memory_extraction_written_total";
const MEMORY_EXTRACTION_REJECTED_TOTAL: &str = "moira_memory_extraction_rejected_total";

// Plan 11 Sub-Phase E — conversation summarization.
//
// One family, reusing the shared `succeeded`/`failed` `outcome` domain and adding no new label
// value. There is deliberately **no `conversation_id` label and no per-skip-reason label**: the
// first is unbounded by construction and the second is unbounded per application, which is the
// rule `ALLOWED_LABEL_KEYS` enforces and this module's header explains.
//
// A run is counted only when the summarizer was actually invoked — a turn on an application with
// `summarization_enabled` off, or one whose backlog is below the thresholds, counts nothing.
// Without that, the family would measure how many conversations exist rather than how
// summarization is behaving.
//
// Unlike extraction, summarization is *partly* visible: the manual endpoint returns its failure.
// The automatic path does not, so a summarizer that started failing on every turn would
// otherwise be invisible until a conversation's context quietly stopped carrying a summary.
const SUMMARIZATION_RUNS_TOTAL: &str = "moira_summarization_runs_total";

// Finding F57 — summaries stored with a reasoning model's chain of thought inlined.
//
// A separate label-free family rather than a third `outcome` value on the one above, because the
// run genuinely *succeeded*: folding it into that domain would make "how many summaries were
// produced" a sum of two series, and would break the binary `succeeded` argument that keeps the
// two outcomes from drifting apart. It is a property of the reply, not an outcome of the run.
//
// No `conversation_id` and no sample of the text, for the reasons the header gives: the first is
// unbounded, and the second is the conversation's content on a scrape path. The per-conversation
// answer is the `inline_reasoning` field on the `conversation.summary.created` audit row.
const SUMMARIZATION_INLINE_REASONING_TOTAL: &str = "moira_summarization_inline_reasoning_total";

// Plan 09 wave 2 — the admin-invitation surface, which grants full `moira:admin` on
// redemption and had no operational signal at all before this.
//
// The label domain is closed by [`ADMIN_INVITE_OUTCOMES`] below. Note what is **not** a
// label: the invitee's email address, their domain, the invite id, or the issuer. Each is
// unbounded cardinality on a scrape path, and the first two are PII in an endpoint whose
// whole job is to be scraped and retained. "Which invite" belongs in `audit_logs`, which
// is the table that already records it.
const ADMIN_INVITE_OUTCOMES_TOTAL: &str = "moira_admin_invite_outcomes_total";
const ADMIN_IDENTITY_GRANT_EVENTS_TOTAL: &str = "moira_admin_identity_grant_events_total";

// The content encryption keyring (`src/security/data_keys.rs`).
//
// Two label-free families, and the pair is the point. A failed keyring refresh is *not* a
// failure: the previous snapshot is still correct, merely stale, so the process keeps serving
// and nothing on the request path changes. That makes it invisible by construction — exactly
// the shape of problem that is discovered weeks later, at the worst possible moment, when a key
// rotated on another replica is one this one has never seen.
//
// So the counter says "a refresh failed" and the gauge says "and this is how stale I therefore
// am". The counter alone cannot distinguish one blip from a keyring that has been wedged since
// Tuesday; the gauge alone cannot say whether staleness is because refreshes are failing or
// because nothing has changed. Alert on the gauge, diagnose with the counter.
//
// **What the gauge cannot see, stated plainly.** It is written by the refresh path — zero on
// success, the previous snapshot's age on failure — and is *not* recomputed at scrape time,
// because the keyring holds the registry rather than the other way round. So a process whose
// refresh task died outright stops writing it and the value **freezes** rather than growing.
// It detects a refresh that keeps failing, which is the common case; it does not detect a tick
// that stopped ticking. Deriving it at scrape time would fix that and would mean the registry
// holding a handle to the thing that holds it, which is a cycle this module will not take on
// for one gauge. The honest companion signal is a process restart or a missing series, not a
// wider reading of this one.
const CONTENT_KEYRING_REFRESH_FAILED_TOTAL: &str = "moira_content_keyring_refresh_failed_total";
const CONTENT_KEYRING_AGE_SECONDS: &str = "moira_content_keyring_age_seconds";

// How many data keys this replica holds, per lifecycle state. Written by the keyring on every
// successful load, which is the only moment the answer can change for this process.
//
// It is what makes a rotation observable without a shell on the pod: `add` moves a key into
// `pending`, `promote` moves one into `active` and one into `retiring`, and `abandon` moves one
// into `abandoned` — so the four series together are a live picture of `moira keyring status`
// across the fleet, including the two conditions worth alerting on. `active == 0` means no
// replica can seal new content; `abandoned > 0` means somebody acknowledged permanent data
// loss, which should never pass unnoticed.
//
// `retired` is deliberately absent from the label domain rather than seeded at zero: a retired
// key is not loaded, so this process has nothing to count and a permanent `0` would invite the
// reading that none exist.
//
// The three envelope counters `docs/decision-encryption-at-rest.md` names alongside this one —
// seal, open, and open-failed — are NOT here. They were deferred because a counter wired to
// nothing is the silently-inert shape (#125) this whole release exists to avoid, and were to
// "arrive with their callers".
//
// **Their callers have now arrived and they have not.** The write and read paths for all five
// sealed columns landed in issues #139, #140 and #141; no seal, open or open-failed counter was
// added with them. So this is no longer a scheduling note, it is a disclosed observability gap:
// a deployment can seal and open content today and see nothing but `moira_content_keyring_keys`.
// The failure paths are not silent — every refusal emits a coded WARN and a coded error
// (`src/security/content_access.rs`) — but there is no rate, and an operator cannot alert on
// "opens started failing" without parsing logs. Closing it is a metrics-design change (three
// counters, their label domains, and the cardinality argument for each) and belongs to its own
// review, not to a write-path PR that would have added it as an afterthought.
const CONTENT_KEYRING_KEYS: &str = "moira_content_keyring_keys";

/// The closed `outcome` label domain for `moira_admin_invite_outcomes_total`.
///
/// `created` and `redeemed` are the two successes; everything else is a denial reason
/// derived from a catalogued error code by `application::identity::denial_reason`, plus
/// `other` as the terminal bucket so an uncatalogued failure widens no label domain at
/// runtime. Seeded at zero so an alert on "redemptions stopped succeeding" fires on the
/// condition rather than on absent data.
const ADMIN_INVITE_OUTCOMES: &[&str] = &[
    "created",
    "redeemed",
    "not_found",
    "expired",
    "consumed",
    "revoked",
    "email_mismatch",
    "domain_mismatch",
    "domain_not_allowed",
    "email_not_verified",
    "email_required",
    "other",
];

/// The closed `event` label domain for `moira_admin_identity_grant_events_total`.
const ADMIN_IDENTITY_GRANT_EVENTS: &[&str] = &["granted", "revoked", "ownership_transferred"];

/// The two `outcome` values a RAG ingestion run can carry.
///
/// Both are members of the existing closed outcome set produced by `execution_outcome_label`,
/// so this family adds no new label *value* to the `outcome` key — only a new family that uses
/// it. Introducing a third, ingestion-specific string would fork a shared label's domain.
const RAG_INGESTION_OUTCOMES: &[&str] = &["succeeded", "failed"];

/// Route label used when a request matched no route (404s, and anything rejected before
/// routing completed). Deliberately a constant: falling back to the raw URI here is exactly
/// the unbounded-label bug this module exists to prevent.
const ROUTE_UNMATCHED: &str = "unmatched";

/// Second-denominated per Prometheus convention, even though the legacy cumulative counter
/// is in microseconds. The range covers what a request-serving process actually sees, from a
/// fast health probe to a long provider call.
const HTTP_LATENCY_BUCKETS_SECONDS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Execution latency is dominated by provider round-trips, so the tail extends far past the
/// HTTP buckets — a two-minute completion is a normal observation, not an outlier.
const EXECUTION_LATENCY_BUCKETS_SECONDS: &[f64] = &[
    0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0,
];

/// Time-to-first-token is the latency an end user actually perceives on a stream, so the
/// resolution is concentrated below one second.
const TTFT_BUCKETS_SECONDS: &[f64] = &[0.025, 0.05, 0.1, 0.2, 0.4, 0.8, 1.5, 3.0, 5.0, 10.0, 20.0];

/// An embedding run covers every batch for one document, so its tail is set by document size
/// rather than by a single round trip — a 2000-chunk document at 32 chunks per batch is 63
/// sequential provider calls.
const EMBEDDING_BATCH_BUCKETS_SECONDS: &[f64] =
    &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0];

/// A retrieval pass sits directly in the response path, before the first token, so its
/// resolution is concentrated well below one second — an operator's question is "is retrieval
/// adding perceptible latency", and buckets sized for a provider round-trip could not answer it.
/// The tail still reaches the embedding call's own timeout.
const RETRIEVAL_LATENCY_BUCKETS_SECONDS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

#[derive(Clone, Debug)]
pub struct MetricsRegistry {
    inner: Arc<MetricsInner>,
}

#[derive(Debug)]
struct MetricsInner {
    recorder: PrometheusRecorder,
    handle: PrometheusHandle,
    /// Sampled at render time only — see [`MetricsRegistry::render_prometheus`]. Held here
    /// rather than passed in because `render_prometheus`'s arity is fixed by the `/metrics`
    /// handler, which this change leaves byte-untouched on purpose.
    pool: Option<PgPool>,
}

impl MetricsRegistry {
    /// `service_name` becomes a builder-time **global label**, which is what keeps every
    /// family carrying the same `service="…"` label it carried before the exporter swap.
    ///
    /// `pool` is optional because Moira can run without a database (the router builds and
    /// serves health/metrics either way); when it is absent the DB-pool gauges stay at zero
    /// rather than disappearing, so the family is present from process start.
    pub fn new(service_name: &str, pool: Option<PgPool>) -> Self {
        let recorder = build_recorder(service_name);
        let handle = recorder.handle();
        let inner = Arc::new(MetricsInner {
            recorder,
            handle,
            pool,
        });
        let registry = Self { inner };
        registry.describe_and_seed_families();
        registry
    }

    /// Registers help text for every family and seeds the pre-existing counters at zero.
    ///
    /// Seeding matters for compatibility: the hand-rolled renderer printed every legacy
    /// family unconditionally, including the ones still at zero. The exporter only renders
    /// metrics that have been registered, so a fresh process would otherwise expose a
    /// *smaller* body than before — a removal, not an addition. Zero-increment registration
    /// restores exactly the previous behaviour.
    ///
    /// Histograms are deliberately **not** seeded: their label values are dynamic, so any
    /// seed would have to invent a fake series. They appear on first observation, which is
    /// standard Prometheus practice.
    fn describe_and_seed_families(&self) {
        with_local_recorder(&self.inner.recorder, || {
            describe_counter!(
                HTTP_REQUESTS_TOTAL,
                "Total HTTP requests observed by Moira."
            );
            describe_counter!(
                HTTP_STATUS_CLASS_TOTAL,
                "Total HTTP responses by low-cardinality status class."
            );
            describe_counter!(
                HTTP_LATENCY_MICROS_TOTAL,
                "Cumulative HTTP response latency in microseconds. Superseded by the \
                 moira_http_request_duration_seconds histogram; kept so scrapers computing an \
                 average from this cumulative sum keep working."
            );
            describe_counter!(
                PUBLIC_RESPONSES_CREATED_TOTAL,
                "Total public non-streaming responses created."
            );
            describe_counter!(
                PUBLIC_STREAMS_STARTED_TOTAL,
                "Total public response streams started."
            );
            describe_counter!(
                WORKER_TICKS_TOTAL,
                "Total background worker maintenance ticks."
            );
            describe_counter!(
                RETENTION_RUNS_TOTAL,
                "Total retention cleanup sweeps completed by this process."
            );
            describe_counter!(
                RETENTION_ROWS_DELETED_TOTAL,
                "Total expired rows deleted by the retention cleanup worker, by table."
            );
            describe_gauge!(
                REDIS_ENABLED,
                "Whether Redis coordination is enabled for this process."
            );
            describe_gauge!(
                WORKERS_ENABLED,
                "Whether background workers are enabled for this process."
            );
            describe_histogram!(
                HTTP_REQUEST_DURATION_SECONDS,
                "HTTP response latency in seconds, by matched route template, method and \
                 status class."
            );
            describe_histogram!(
                EXECUTION_DURATION_SECONDS,
                "Provider execution attempt latency in seconds, by provider type and outcome."
            );
            describe_histogram!(
                EXECUTION_TTFT_SECONDS,
                "Time from the start of a provider attempt to its first streamed token, in \
                 seconds, by provider type."
            );
            describe_counter!(
                PROVIDER_OUTCOME_TOTAL,
                "Provider execution attempt outcomes, by provider type, model key and outcome."
            );
            describe_gauge!(
                DB_POOL_CONNECTIONS,
                "PostgreSQL connection-pool occupancy, sampled once per scrape."
            );

            counter!(HTTP_REQUESTS_TOTAL).increment(0);
            for class in ["2xx", "3xx", "4xx", "5xx"] {
                counter!(HTTP_STATUS_CLASS_TOTAL, "status_class" => class).increment(0);
            }
            counter!(HTTP_LATENCY_MICROS_TOTAL).increment(0);
            counter!(PUBLIC_RESPONSES_CREATED_TOTAL).increment(0);
            counter!(PUBLIC_STREAMS_STARTED_TOTAL).increment(0);
            counter!(WORKER_TICKS_TOTAL).increment(0);
            counter!(RETENTION_RUNS_TOTAL).increment(0);
            for table in [TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES] {
                counter!(RETENTION_ROWS_DELETED_TOTAL, "table" => table).increment(0);
            }
            describe_counter!(
                WORKER_JOBS_CLAIMED_TOTAL,
                "Durable worker-queue jobs claimed by this process, by job name."
            );
            describe_counter!(
                WORKER_JOBS_COMPLETED_TOTAL,
                "Durable worker-queue jobs completed successfully, by job name."
            );
            describe_counter!(
                WORKER_JOBS_FAILED_TOTAL,
                "Durable worker-queue job attempts that failed and were rescheduled, by job name."
            );
            describe_counter!(
                WORKER_JOBS_DEAD_LETTER_TOTAL,
                "Durable worker-queue jobs that exhausted max_attempts and were dead-lettered, \
                 by job name."
            );
            describe_counter!(
                WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL,
                "Enqueue attempts refused because the queue's pending-depth cap was reached."
            );
            describe_gauge!(
                WORKER_LEADER_HELD,
                "Whether this process currently holds the singleton-worker leader lock, by job \
                 name. Per-process by construction — each process owns its own registry — so \
                 there is deliberately no replica label."
            );
            describe_counter!(
                REDIS_OPERATION_FAILURES_TOTAL,
                "Redis coordination operations that failed, by operation. Always zero when \
                 Redis is disabled, which is the default."
            );
            describe_counter!(
                RUNTIME_INVALIDATIONS_TOTAL,
                "Runtime-config cache invalidations applied by this process, by the channel \
                 that delivered them. The postgres channel is authoritative; the redis \
                 channel is an additive second signal and is absent unless Redis is enabled."
            );

            describe_counter!(
                RAG_CHUNKS_WRITTEN_TOTAL,
                "RAG document chunks written by the ingestion pipeline."
            );
            describe_counter!(
                RAG_EMBEDDINGS_WRITTEN_TOTAL,
                "RAG chunk embeddings written by the ingestion pipeline. Stays at zero for \
                 applications that have not enabled rag_embeddings_enabled, which is the \
                 default."
            );
            describe_counter!(
                RAG_INGESTION_RUNS_TOTAL,
                "RAG document ingestion runs, by outcome. A run is counted once per document \
                 version that produced at least one chunk or recorded a failure."
            );
            describe_histogram!(
                EMBEDDING_BATCH_LATENCY_SECONDS,
                "Wall-clock time for one document's complete embedding run, across every \
                 batch, in seconds. Recorded on failure as well as success."
            );
            describe_counter!(
                RETRIEVAL_RUNS_TOTAL,
                "Context-planner retrieval passes, by outcome. Under the default \
                 failure_behavior a failed retrieval is invisible to the caller, so this \
                 counter and the retrieval_runs table are the only places it surfaces."
            );
            describe_histogram!(
                RETRIEVAL_LATENCY_SECONDS,
                "Wall-clock time for one retrieval pass — query embedding plus both candidate \
                 queries plus ranking — in seconds. Recorded on failure as well as success."
            );
            describe_counter!(
                MEMORY_EXTRACTION_RUNS_TOTAL,
                "Automatic memory-extraction runs, by outcome. A run is counted once per \
                 assistant turn that actually called the extractor; a turn on an application \
                 with automatic_extraction_enabled off — the default — counts nothing."
            );
            describe_counter!(
                MEMORY_EXTRACTION_WRITTEN_TOTAL,
                "Memory records written by automatic extraction. Counts inserts only: a \
                 candidate recognised as a duplicate of an existing memory confirms that row \
                 and is not counted here."
            );
            describe_counter!(
                MEMORY_EXTRACTION_REJECTED_TOTAL,
                "Extraction candidates refused by the application's memory policy — \
                 disallowed type or sensitivity, confidence below the minimum, or content that \
                 failed validation. Per-reason detail is on memory_extraction_runs.metadata; a \
                 reason label would be unbounded per application."
            );
            describe_counter!(
                SUMMARIZATION_RUNS_TOTAL,
                "Conversation-summarization runs, by outcome. A run is counted once per \
                 invocation that reached the model — a turn below the trigger thresholds, or on \
                 an application with summarization_enabled off (the default), counts nothing. \
                 Carries no conversation id: per-conversation detail is the \
                 conversation_summaries row itself."
            );
            describe_counter!(
                SUMMARIZATION_INLINE_REASONING_TOTAL,
                "Successful summarization runs whose reply opened with an inline reasoning \
                 block, which is stored verbatim as the summary. A non-zero rate means the \
                 backend is a reasoning model serving its chain of thought as ordinary text: \
                 start vLLM with --reasoning-parser so the block arrives in its own field, \
                 which Moira already discards. Every run counted here is also counted as a \
                 succeeded run on moira_summarization_runs_total."
            );
            describe_counter!(
                ADMIN_INVITE_OUTCOMES_TOTAL,
                "Admin-invitation outcomes, by a bounded outcome/denial-reason label. Carries \
                 no invitee email, domain, invite id or issuer: those are unbounded \
                 cardinality on a scrape path, and the first two are PII. Per-invite detail \
                 lives in audit_logs."
            );
            describe_counter!(
                ADMIN_IDENTITY_GRANT_EVENTS_TOTAL,
                "Admin-identity grant lifecycle events, by event. A grant carries full \
                 moira:admin, so a sudden change in this family's rate is the signal an \
                 operator wants first."
            );
            describe_counter!(
                CONTENT_KEYRING_REFRESH_FAILED_TOTAL,
                "Content-keyring refreshes that failed. A failure is not an outage — the \
                 previous snapshot is still correct, merely stale, and the process keeps \
                 serving — so this counter and the age gauge are the only places it surfaces."
            );
            describe_gauge!(
                CONTENT_KEYRING_AGE_SECONDS,
                "Seconds since the in-process content keyring was last loaded successfully. \
                 Zero after every successful refresh. Alert on this rather than on the failure \
                 counter: it is also the family that grows when the refresh task has died \
                 outright and is therefore raising nothing at all."
            );

            gauge!(DB_POOL_CONNECTIONS, "state" => "total").set(0.0);
            gauge!(DB_POOL_CONNECTIONS, "state" => "idle").set(0.0);
            for job_name in WORKER_JOB_NAMES {
                counter!(WORKER_JOBS_CLAIMED_TOTAL, "job_name" => *job_name).increment(0);
                counter!(WORKER_JOBS_COMPLETED_TOTAL, "job_name" => *job_name).increment(0);
                counter!(WORKER_JOBS_FAILED_TOTAL, "job_name" => *job_name).increment(0);
                counter!(WORKER_JOBS_DEAD_LETTER_TOTAL, "job_name" => *job_name).increment(0);
            }
            counter!(WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL).increment(0);
            for job_name in LEADER_GATED_WORKERS {
                gauge!(WORKER_LEADER_HELD, "job_name" => *job_name).set(0.0);
            }
            for operation in RedisOperation::ALL {
                counter!(REDIS_OPERATION_FAILURES_TOTAL, "operation" => operation.label())
                    .increment(0);
            }
            for channel in InvalidationChannel::ALL {
                counter!(RUNTIME_INVALIDATIONS_TOTAL, "channel" => channel.label()).increment(0);
            }
            counter!(RAG_CHUNKS_WRITTEN_TOTAL).increment(0);
            counter!(RAG_EMBEDDINGS_WRITTEN_TOTAL).increment(0);
            counter!(MEMORY_EXTRACTION_WRITTEN_TOTAL).increment(0);
            counter!(MEMORY_EXTRACTION_REJECTED_TOTAL).increment(0);
            counter!(SUMMARIZATION_INLINE_REASONING_TOTAL).increment(0);
            // Seeded so "keyring refreshes started failing" is an alert on a rate, not on the
            // sudden appearance of a series. The gauge is seeded too: a process that never
            // reached a keyring — no database configured — reports zero rather than nothing,
            // which is the honest answer for "how stale is the keyring I am serving from".
            counter!(CONTENT_KEYRING_REFRESH_FAILED_TOTAL).increment(0);
            gauge!(CONTENT_KEYRING_AGE_SECONDS).set(0.0);
            for outcome in RAG_INGESTION_OUTCOMES {
                counter!(RAG_INGESTION_RUNS_TOTAL, "outcome" => *outcome).increment(0);
                counter!(RETRIEVAL_RUNS_TOTAL, "outcome" => *outcome).increment(0);
                counter!(MEMORY_EXTRACTION_RUNS_TOTAL, "outcome" => *outcome).increment(0);
                counter!(SUMMARIZATION_RUNS_TOTAL, "outcome" => *outcome).increment(0);
            }
            for outcome in ADMIN_INVITE_OUTCOMES {
                counter!(ADMIN_INVITE_OUTCOMES_TOTAL, "outcome" => *outcome).increment(0);
            }
            for event in ADMIN_IDENTITY_GRANT_EVENTS {
                counter!(ADMIN_IDENTITY_GRANT_EVENTS_TOTAL, "event" => *event).increment(0);
            }
        });
    }

    // -----------------------------------------------------------------------------------
    // Pre-existing recorders. Signatures are unchanged, so no call site outside this module
    // was edited by the exporter swap.
    // -----------------------------------------------------------------------------------

    pub fn record_http_response(&self, status: StatusCode, latency: Duration) {
        let class = status_class(status);
        let micros = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX);
        with_local_recorder(&self.inner.recorder, || {
            counter!(HTTP_REQUESTS_TOTAL).increment(1);
            // `moira_http_latency_micros_total` is kept, not deprecated: removing it would
            // break any scraper deriving an average from the cumulative sum, and this
            // iteration freezes the surface rather than changing it.
            counter!(HTTP_LATENCY_MICROS_TOTAL).increment(micros);
            // 1xx (and any out-of-range status) increments no class counter, exactly as the
            // pre-swap `is_success`/`is_redirection`/… chain did.
            if matches!(class, "2xx" | "3xx" | "4xx" | "5xx") {
                counter!(HTTP_STATUS_CLASS_TOTAL, "status_class" => class).increment(1);
            }
        });
    }

    pub fn record_public_response_created(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(PUBLIC_RESPONSES_CREATED_TOTAL).increment(1);
        });
    }

    pub fn record_public_stream_started(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(PUBLIC_STREAMS_STARTED_TOTAL).increment(1);
        });
    }

    pub fn record_worker_tick(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(WORKER_TICKS_TOTAL).increment(1);
        });
    }

    /// Counts one completed retention sweep, successful or not — the sweep-rate signal is
    /// what tells an operator the worker is alive at all.
    pub fn record_retention_run(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(RETENTION_RUNS_TOTAL).increment(1);
        });
    }

    /// Counts rows deleted by the retention worker, per table.
    ///
    /// `table` is matched against the two `&'static str` constants in
    /// `crate::infra::workers::retention`, so the label set stays at cardinality 2 and an
    /// unrecognised table is dropped rather than silently folded into another table's total.
    pub fn record_retention_deleted(&self, table: &str, count: u64) {
        let table = match table {
            TABLE_IDEMPOTENCY_RECORDS => TABLE_IDEMPOTENCY_RECORDS,
            TABLE_RESPONSES => TABLE_RESPONSES,
            _ => return,
        };
        with_local_recorder(&self.inner.recorder, || {
            counter!(RETENTION_ROWS_DELETED_TOTAL, "table" => table).increment(count);
        });
    }

    // -----------------------------------------------------------------------------------
    // New recorders (P1-9b).
    // -----------------------------------------------------------------------------------

    /// Records one HTTP response into the latency histogram.
    ///
    /// `route` **must** be the matched route *template* from
    /// [`axum::extract::MatchedPath`] — `/api/v1/admin/applications/{id}`, never the resolved
    /// path. `None` means the request matched no route and is labelled
    /// [`ROUTE_UNMATCHED`]. The method is folded into a closed set so an extension method
    /// (hyper accepts any RFC 9110 token) cannot open the label set.
    pub fn record_http_latency(
        &self,
        route: Option<&str>,
        method: &Method,
        status: StatusCode,
        latency: Duration,
    ) {
        let route = route.unwrap_or(ROUTE_UNMATCHED).to_string();
        let method = method_label(method);
        let class = status_class(status);
        let seconds = latency.as_secs_f64();
        with_local_recorder(&self.inner.recorder, || {
            histogram!(
                HTTP_REQUEST_DURATION_SECONDS,
                "route" => route,
                "method" => method,
                "status_class" => class
            )
            .record(seconds);
        });
    }

    /// Records one RAG document-version ingestion run (plan 11 Sub-Phases A and B).
    ///
    /// Called once per version that actually ran the pipeline. A version with no content
    /// produces no chunks and no failure, and is deliberately **not** counted: nothing ran, and
    /// inflating the `succeeded` series with no-ops would make "ingestion is working" true by
    /// construction — the exact shape of dishonesty this pipeline exists to remove.
    pub fn record_rag_ingestion_run(&self, chunks: u64, embeddings: u64, succeeded: bool) {
        if chunks == 0 && succeeded {
            return;
        }
        let outcome = if succeeded { "succeeded" } else { "failed" };
        with_local_recorder(&self.inner.recorder, || {
            counter!(RAG_INGESTION_RUNS_TOTAL, "outcome" => outcome).increment(1);
            if chunks > 0 {
                counter!(RAG_CHUNKS_WRITTEN_TOTAL).increment(chunks);
            }
            if embeddings > 0 {
                counter!(RAG_EMBEDDINGS_WRITTEN_TOTAL).increment(embeddings);
            }
        });
    }

    /// Records one retrieval pass — plan 11 wave 2.
    ///
    /// Counted on failure as well as success, and *always* counted when retrieval was attempted:
    /// under the default `failure_behavior` a retrieval failure is invisible to the caller, so
    /// this counter and the `retrieval_runs` row are the only places it shows up at all.
    ///
    /// No per-application label, deliberately. `retrieval_runs` carries the application-level
    /// detail; see `ALLOWED_LABEL_KEYS` and this module's header for why an application id may
    /// never be a label value.
    pub fn record_retrieval_run(&self, latency: Duration, succeeded: bool) {
        let outcome = if succeeded { "succeeded" } else { "failed" };
        let seconds = latency.as_secs_f64();
        with_local_recorder(&self.inner.recorder, || {
            counter!(RETRIEVAL_RUNS_TOTAL, "outcome" => outcome).increment(1);
            histogram!(RETRIEVAL_LATENCY_SECONDS).record(seconds);
        });
    }

    /// Records one automatic memory-extraction run — plan 11 Sub-Phase F.
    ///
    /// `written` counts inserts, not accepted candidates: a candidate that turned out to
    /// duplicate an existing memory confirms that row instead, and counting it here would make
    /// "extraction is producing memories" true on an application that has produced exactly one
    /// and re-observed it a thousand times.
    ///
    /// Counted on failure as well as success, and always counted when the extractor was
    /// actually called. Extraction never surfaces an error to the caller, so this counter and
    /// the `memory_extraction_runs` row are the only places a failing extractor shows up.
    pub fn record_memory_extraction_run(&self, written: u64, rejected: u64, succeeded: bool) {
        let outcome = if succeeded { "succeeded" } else { "failed" };
        with_local_recorder(&self.inner.recorder, || {
            counter!(MEMORY_EXTRACTION_RUNS_TOTAL, "outcome" => outcome).increment(1);
            if written > 0 {
                counter!(MEMORY_EXTRACTION_WRITTEN_TOTAL).increment(written);
            }
            if rejected > 0 {
                counter!(MEMORY_EXTRACTION_REJECTED_TOTAL).increment(rejected);
            }
        });
    }

    /// Records one conversation-summarization run — plan 11 Sub-Phase E.
    ///
    /// Called only when the summarizer reached the model. A run refused by the trigger decision
    /// is not a run: counting it would make the family measure conversation traffic instead of
    /// summarizer behaviour, and would hide a genuine failure rate behind a large denominator.
    pub fn record_summarization_run(&self, succeeded: bool) {
        let outcome = if succeeded { "succeeded" } else { "failed" };
        with_local_recorder(&self.inner.recorder, || {
            counter!(SUMMARIZATION_RUNS_TOTAL, "outcome" => outcome).increment(1);
        });
    }

    /// Records one stored summary that opened with an inline reasoning block — finding F57.
    ///
    /// Called *in addition to* [`Self::record_summarization_run`] with `succeeded = true`, never
    /// instead of it. The run produced a row; what this counts is that the row's text is mostly
    /// the model's scratchpad. An operator reading only the run family would see a healthy
    /// summarizer, which is exactly the silence F57 is about.
    pub fn record_summarization_inline_reasoning(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(SUMMARIZATION_INLINE_REASONING_TOTAL).increment(1);
        });
    }

    /// Records how long one document's complete embedding run took, on success and failure
    /// alike — a timeout is exactly the observation this histogram exists to show.
    pub fn record_embedding_batch_latency(&self, latency: Duration) {
        let seconds = latency.as_secs_f64();
        with_local_recorder(&self.inner.recorder, || {
            histogram!(EMBEDDING_BATCH_LATENCY_SECONDS).record(seconds);
        });
    }

    /// Records the latency of one provider execution attempt.
    ///
    /// The outcome label is derived from the existing `ExecutionStatus`/`ExecutionFailureClass`
    /// domain enums by an exhaustive match — there is no parallel taxonomy to drift, and
    /// adding a domain variant fails to compile until it is given a label here. Provider
    /// error *text* is never a label.
    pub fn record_execution_latency(
        &self,
        provider_type: ProviderType,
        status: ExecutionStatus,
        failure_class: Option<ExecutionFailureClass>,
        latency: Duration,
    ) {
        let provider = provider_type_label(provider_type);
        let outcome = execution_outcome_label(status, failure_class);
        let seconds = latency.as_secs_f64();
        with_local_recorder(&self.inner.recorder, || {
            histogram!(
                EXECUTION_DURATION_SECONDS,
                "provider_type" => provider,
                "outcome" => outcome
            )
            .record(seconds);
        });
    }

    /// Records time-to-first-token for a streamed attempt, measured from the start of the
    /// attempt to the first output-bearing chunk.
    pub fn record_ttft(&self, provider_type: ProviderType, latency: Duration) {
        let provider = provider_type_label(provider_type);
        let seconds = latency.as_secs_f64();
        with_local_recorder(&self.inner.recorder, || {
            histogram!(EXECUTION_TTFT_SECONDS, "provider_type" => provider).record(seconds);
        });
    }

    /// Counts one provider attempt outcome.
    ///
    /// `model_key` is admin-configured runtime configuration, not caller input, so its
    /// cardinality is bounded by the operator's model catalogue.
    pub fn record_provider_outcome(
        &self,
        provider_type: ProviderType,
        model_key: &str,
        status: ExecutionStatus,
        failure_class: Option<ExecutionFailureClass>,
    ) {
        let provider = provider_type_label(provider_type);
        let outcome = execution_outcome_label(status, failure_class);
        let model_key = model_key.to_string();
        with_local_recorder(&self.inner.recorder, || {
            counter!(
                PROVIDER_OUTCOME_TOTAL,
                "provider_type" => provider,
                "model_key" => model_key,
                "outcome" => outcome
            )
            .increment(1);
        });
    }

    /// Records the outcome of one content-keyring refresh.
    ///
    /// Only the failure increments. A "refreshes attempted" counter was considered and left
    /// out: the background tick is a fixed interval, so its rate is a constant that says
    /// nothing, and the on-demand rate belongs to whatever met an unknown key rather than to
    /// the keyring.
    pub fn record_content_keyring_refresh(&self, succeeded: bool) {
        if succeeded {
            return;
        }
        with_local_recorder(&self.inner.recorder, || {
            counter!(CONTENT_KEYRING_REFRESH_FAILED_TOTAL).increment(1);
        });
    }

    /// Sets how stale the in-process content keyring is, in seconds.
    ///
    /// Set by the keyring itself rather than derived at scrape time, and that is deliberate: a
    /// process whose refresh task has died stops calling this, so the last value it wrote ages
    /// into the alert instead of being silently recomputed as fresh.
    pub fn set_content_keyring_age_seconds(&self, age_seconds: f64) {
        with_local_recorder(&self.inner.recorder, || {
            gauge!(CONTENT_KEYRING_AGE_SECONDS).set(age_seconds);
        });
    }

    /// Sets `moira_content_keyring_keys{state}` from a freshly loaded snapshot.
    ///
    /// Takes the **whole** distribution, not one `(state, count)` pair, and that is what makes
    /// it correct: a state that dropped to zero has to be *written* as zero, because a gauge
    /// nobody updates keeps its last value forever. A caller that only reported the states it
    /// found would leave `pending 1` standing after the promotion that emptied it — and the
    /// series an operator watches during a rotation would be the one that lies.
    pub fn set_content_keyring_keys(&self, counts: &[(&'static str, usize)]) {
        with_local_recorder(&self.inner.recorder, || {
            for (state, count) in counts {
                #[allow(clippy::cast_precision_loss)]
                gauge!(CONTENT_KEYRING_KEYS, "state" => *state).set(*count as f64);
            }
        });
    }

    /// Sets the DB connection-pool gauges.
    ///
    /// Called once per `/metrics` scrape rather than per request: pool introspection is
    /// cheap but pointless on the hot path, and a gauge only needs to be true at scrape time.
    pub fn record_db_pool_utilization(&self, size: u32, idle: u32) {
        with_local_recorder(&self.inner.recorder, || {
            gauge!(DB_POOL_CONNECTIONS, "state" => "total").set(f64::from(size));
            gauge!(DB_POOL_CONNECTIONS, "state" => "idle").set(f64::from(idle));
        });
    }

    // -----------------------------------------------------------------------------------
    // Plan 10 wave 2 — durable queue, leadership, Redis coordination.
    //
    // Every `job_name` argument is checked against `WORKER_JOB_NAMES` before it becomes a
    // label, exactly as `record_retention_deleted` checks its table name. An unrecognised
    // name is dropped rather than folded into another job's total: a silently merged
    // counter is a worse answer than a missing one, because it looks correct.
    // -----------------------------------------------------------------------------------

    pub fn record_worker_job_claimed(&self, job_name: &str, count: u64) {
        self.record_worker_job(WORKER_JOBS_CLAIMED_TOTAL, job_name, count);
    }

    pub fn record_worker_job_completed(&self, job_name: &str) {
        self.record_worker_job(WORKER_JOBS_COMPLETED_TOTAL, job_name, 1);
    }

    pub fn record_worker_job_failed(&self, job_name: &str) {
        self.record_worker_job(WORKER_JOBS_FAILED_TOTAL, job_name, 1);
    }

    pub fn record_worker_job_dead_lettered(&self, job_name: &str) {
        self.record_worker_job(WORKER_JOBS_DEAD_LETTER_TOTAL, job_name, 1);
    }

    fn record_worker_job(&self, family: &'static str, job_name: &str, count: u64) {
        let Some(job_name) = known_job_name(job_name) else {
            return;
        };
        with_local_recorder(&self.inner.recorder, || {
            counter!(family, "job_name" => job_name).increment(count);
        });
    }

    /// One enqueue refused by the pending-depth cap.
    ///
    /// Unlabelled on purpose: the job name is available at the call site, but a
    /// rejection is a property of the *queue*, and an operator alerting on it wants
    /// "the queue is full", not a per-job breakdown of who noticed first.
    pub fn record_worker_queue_enqueue_rejected(&self) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL).increment(1);
        });
    }

    /// Whether this process holds the leader lock for `job_name`.
    ///
    /// A gauge rather than a counter because leadership is a *state*, and it is
    /// already per-process: each process owns its own recorder, so summing the
    /// family across a cluster's scrape targets gives the number of leaders — which
    /// must be one — with no replica label needed. Adding one would put a pod UUID
    /// in a label value, which `high_cardinality_identifiers_never_appear_as_label_values`
    /// forbids.
    pub fn record_worker_leadership(&self, job_name: &str, held: bool) {
        let Some(job_name) = known_job_name(job_name) else {
            return;
        };
        with_local_recorder(&self.inner.recorder, || {
            gauge!(WORKER_LEADER_HELD, "job_name" => job_name).set(f64::from(u8::from(held)));
        });
    }

    pub fn record_redis_operation_failure(&self, operation: RedisOperation) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(REDIS_OPERATION_FAILURES_TOTAL, "operation" => operation.label()).increment(1);
        });
    }

    pub fn record_runtime_invalidation(&self, channel: InvalidationChannel) {
        with_local_recorder(&self.inner.recorder, || {
            counter!(RUNTIME_INVALIDATIONS_TOTAL, "channel" => channel.label()).increment(1);
        });
    }

    // -----------------------------------------------------------------------------------
    // Plan 09 wave 2 — admin invitations and grant lifecycle.
    // -----------------------------------------------------------------------------------

    pub fn record_admin_invite_created(&self) {
        self.record_admin_invite_outcome("created");
    }

    pub fn record_admin_invite_redeemed(&self) {
        self.record_admin_invite_outcome("redeemed");
        self.record_admin_identity_grant_event("granted");
    }

    /// One refused invitation, labelled by a **bounded** denial reason.
    ///
    /// The caller derives `reason` from the error *code*, never from the message and
    /// never from the request, so a novel failure cannot inject a label value. An
    /// unrecognised reason is folded into `other` rather than dropped: a denial that
    /// vanishes from the counter is worse than one in a catch-all bucket, because an
    /// operator watching the total would see the rate fall while denials rose.
    pub fn record_admin_invite_denied(&self, reason: &str) {
        let reason = if ADMIN_INVITE_OUTCOMES.contains(&reason)
            && reason != "created"
            && reason != "redeemed"
        {
            reason
        } else {
            "other"
        };
        self.record_admin_invite_outcome(reason);
    }

    pub fn record_admin_identity_revoked(&self) {
        self.record_admin_identity_grant_event("revoked");
    }

    pub fn record_admin_ownership_transferred(&self) {
        self.record_admin_identity_grant_event("ownership_transferred");
    }

    fn record_admin_invite_outcome(&self, outcome: &str) {
        let Some(outcome) = ADMIN_INVITE_OUTCOMES
            .iter()
            .find(|candidate| **candidate == outcome)
        else {
            return;
        };
        with_local_recorder(&self.inner.recorder, || {
            counter!(ADMIN_INVITE_OUTCOMES_TOTAL, "outcome" => *outcome).increment(1);
        });
    }

    fn record_admin_identity_grant_event(&self, event: &str) {
        let Some(event) = ADMIN_IDENTITY_GRANT_EVENTS
            .iter()
            .find(|candidate| **candidate == event)
        else {
            return;
        };
        with_local_recorder(&self.inner.recorder, || {
            counter!(ADMIN_IDENTITY_GRANT_EVENTS_TOTAL, "event" => *event).increment(1);
        });
    }

    /// Renders the Prometheus exposition body served by `GET /metrics`.
    ///
    /// The first parameter is retained purely to keep this signature — and therefore
    /// `src/http/observability.rs` — unchanged; the service name is now applied as a
    /// builder-time global label instead.
    pub fn render_prometheus(
        &self,
        _service_name: &str,
        redis_enabled: bool,
        workers_enabled: bool,
    ) -> String {
        if let Some(pool) = self.inner.pool.as_ref() {
            let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
            self.record_db_pool_utilization(pool.size(), idle);
        }
        with_local_recorder(&self.inner.recorder, || {
            gauge!(REDIS_ENABLED).set(f64::from(u8::from(redis_enabled)));
            gauge!(WORKERS_ENABLED).set(f64::from(u8::from(workers_enabled)));
        });
        self.inner.handle.render()
    }
}

/// The Redis coordination operations that can fail, as a closed label set.
///
/// An enum rather than a `&str` parameter because `operation` is a label key: a
/// caller passing an ad-hoc string would open the label set one call site at a
/// time, and nothing would fail until the scrape body did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisOperation {
    RateLimit,
    PermitAcquire,
    PermitRelease,
    Publish,
    Subscribe,
}

impl RedisOperation {
    pub(crate) const ALL: [Self; 5] = [
        Self::RateLimit,
        Self::PermitAcquire,
        Self::PermitRelease,
        Self::Publish,
        Self::Subscribe,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::PermitAcquire => "permit_acquire",
            Self::PermitRelease => "permit_release",
            Self::Publish => "publish",
            Self::Subscribe => "subscribe",
        }
    }
}

/// Which transport delivered a runtime-config invalidation.
///
/// Two arms and no third: Postgres `LISTEN/NOTIFY` is authoritative and always
/// present, Redis pub/sub is an additive second signal that exists only when
/// Redis is enabled. The pair is what lets an operator confirm the second channel
/// is actually carrying traffic — and, more usefully, that the first one still is
/// when the second is turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationChannel {
    Postgres,
    Redis,
}

impl InvalidationChannel {
    pub(crate) const ALL: [Self; 2] = [Self::Postgres, Self::Redis];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Redis => "redis",
        }
    }
}

/// `job_name` as a label value, or `None` if Moira has never declared that job.
fn known_job_name(job_name: &str) -> Option<&'static str> {
    WORKER_JOB_NAMES
        .iter()
        .find(|declared| **declared == job_name)
        .copied()
}

// `set_buckets_for_metric` has exactly one failure mode: an empty bucket slice. These
// compile-time assertions turn that runtime error into a build error, which is what makes
// the `expect` in `build_recorder` provably unreachable rather than merely unlikely.
const _: () = assert!(!HTTP_LATENCY_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!EXECUTION_LATENCY_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!TTFT_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!EMBEDDING_BATCH_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!RETRIEVAL_LATENCY_BUCKETS_SECONDS.is_empty());

fn build_recorder(service_name: &str) -> PrometheusRecorder {
    let mut builder =
        PrometheusBuilder::new().add_global_label("service", service_name.to_string());
    // Without an explicit bucket set the exporter renders histograms as *summaries*
    // (quantiles), not `_bucket` series. Buckets are therefore configured per family here,
    // once, rather than at each call site.
    for (name, buckets) in [
        (HTTP_REQUEST_DURATION_SECONDS, HTTP_LATENCY_BUCKETS_SECONDS),
        (
            EXECUTION_DURATION_SECONDS,
            EXECUTION_LATENCY_BUCKETS_SECONDS,
        ),
        (EXECUTION_TTFT_SECONDS, TTFT_BUCKETS_SECONDS),
        (
            EMBEDDING_BATCH_LATENCY_SECONDS,
            EMBEDDING_BATCH_BUCKETS_SECONDS,
        ),
        (RETRIEVAL_LATENCY_SECONDS, RETRIEVAL_LATENCY_BUCKETS_SECONDS),
    ] {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_string()), buckets)
            .expect("bucket slices are non-empty compile-time constants");
    }
    builder.build_recorder()
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// Folds an HTTP method into a closed set. `hyper` accepts any RFC 9110 token as an
/// extension method, so passing the method through verbatim would let a caller mint
/// unlimited label values.
fn method_label(method: &Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "TRACE" => "TRACE",
        "CONNECT" => "CONNECT",
        _ => "other",
    }
}

/// Exhaustive so a new [`ProviderType`] variant cannot silently ship without a label.
///
/// Shared with the execution-attempt span in `src/application/execution.rs` so the
/// metric label and the span attribute cannot drift into two taxonomies for the
/// same value.
pub(crate) fn provider_type_label(provider_type: ProviderType) -> &'static str {
    match provider_type {
        ProviderType::OpenAiCompatible => "openai_compatible",
        ProviderType::OpenAi => "openai",
        ProviderType::Anthropic => "anthropic",
        ProviderType::Gemini => "gemini",
        ProviderType::DeepSeek => "deepseek",
        ProviderType::AzureOpenAi => "azure_openai",
        ProviderType::Local => "local",
        ProviderType::Custom => "custom",
    }
}

/// The single source of the execution-outcome label set: a success/cancellation from
/// [`ExecutionStatus`], otherwise the [`ExecutionFailureClass`] rendered with the same
/// snake-case spelling it already serialises with on the wire.
fn execution_outcome_label(
    status: ExecutionStatus,
    failure_class: Option<ExecutionFailureClass>,
) -> &'static str {
    if let Some(class) = failure_class {
        return failure_class_label(class);
    }
    match status {
        ExecutionStatus::Succeeded => "succeeded",
        ExecutionStatus::Failed => "failed",
        ExecutionStatus::Cancelled => "cancelled",
    }
}

/// Exhaustive so a new [`ExecutionFailureClass`] variant cannot silently ship without a
/// label.
fn failure_class_label(class: ExecutionFailureClass) -> &'static str {
    match class {
        ExecutionFailureClass::InvalidExecutionRequest => "invalid_execution_request",
        ExecutionFailureClass::ApplicationUnavailable => "application_unavailable",
        ExecutionFailureClass::RouteNotFound => "route_not_found",
        ExecutionFailureClass::RouteForbidden => "route_forbidden",
        ExecutionFailureClass::AgentProfileNotFound => "agent_profile_not_found",
        ExecutionFailureClass::AgentProfileDisabled => "agent_profile_disabled",
        ExecutionFailureClass::ModelNotFound => "model_not_found",
        ExecutionFailureClass::ModelForbidden => "model_forbidden",
        ExecutionFailureClass::ModelCapabilityMismatch => "model_capability_mismatch",
        ExecutionFailureClass::NoEligibleModel => "no_eligible_model",
        ExecutionFailureClass::CredentialNotFound => "credential_not_found",
        ExecutionFailureClass::CredentialForbidden => "credential_forbidden",
        ExecutionFailureClass::CredentialExpired => "credential_expired",
        ExecutionFailureClass::CredentialDisabled => "credential_disabled",
        ExecutionFailureClass::CredentialDecryptionFailed => "credential_decryption_failed",
        ExecutionFailureClass::ProviderConfigurationInvalid => "provider_configuration_invalid",
        ExecutionFailureClass::ProviderUnavailable => "provider_unavailable",
        ExecutionFailureClass::ProviderRateLimited => "provider_rate_limited",
        ExecutionFailureClass::ProviderTimeout => "provider_timeout",
        ExecutionFailureClass::ProviderConnectionFailed => "provider_connection_failed",
        ExecutionFailureClass::ProviderAuthenticationFailed => "provider_authentication_failed",
        ExecutionFailureClass::ProviderInvalidResponse => "provider_invalid_response",
        ExecutionFailureClass::ProviderUpstreamError => "provider_upstream_error",
        ExecutionFailureClass::CircuitOpen => "circuit_open",
        ExecutionFailureClass::CapacityExhausted => "capacity_exhausted",
        ExecutionFailureClass::RequestCancelled => "request_cancelled",
        ExecutionFailureClass::DeadlineExceeded => "deadline_exceeded",
        ExecutionFailureClass::StructuredOutputInvalid => "structured_output_invalid",
        ExecutionFailureClass::StreamBackpressureExceeded => "stream_backpressure_exceeded",
        ExecutionFailureClass::ContentDecryptionFailed => "content_decryption_failed",
        ExecutionFailureClass::ContentEnvelopeUnsupported => "content_envelope_unsupported",
        ExecutionFailureClass::ContentKeyUnavailable => "content_key_unavailable",
        ExecutionFailureClass::ContentKeyAbandoned => "content_key_abandoned",
        ExecutionFailureClass::InternalError => "internal_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::workers::RETENTION_CLEANUP_WORKER;

    /// Every label key any Moira metric family is permitted to carry. `le` and `quantile`
    /// are the exporter's own. A new call site introducing, say, `execution_id` or `path`
    /// fails `high_cardinality_identifiers_never_appear_as_label_values`.
    const ALLOWED_LABEL_KEYS: &[&str] = &[
        "service",
        "status_class",
        "table",
        "route",
        "method",
        "provider_type",
        "outcome",
        "model_key",
        "state",
        // Plan 10 wave 2. All three are closed sets enforced in code rather than by
        // convention: `job_name` against `WORKER_JOB_NAMES`, `operation` against
        // `RedisOperation::ALL`, `channel` against `InvalidationChannel::ALL`.
        "job_name",
        "operation",
        "channel",
        // Plan 09 wave 2. Closed against `ADMIN_IDENTITY_GRANT_EVENTS`, enforced in
        // `record_admin_identity_grant_event` rather than by convention. The invitation
        // family reuses `outcome` instead of introducing a second name for the same
        // idea; this one is `event` because a grant lifecycle transition is not an
        // outcome of anything, and folding it into `outcome` would put two unrelated
        // domains behind one label key.
        "event",
        "le",
        "quantile",
    ];

    fn registry() -> MetricsRegistry {
        MetricsRegistry::new("moira-test", None)
    }

    /// Collects the label keys present on every rendered sample line.
    fn label_keys(rendered: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for line in rendered.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let Some(open) = line.find('{') else { continue };
            let Some(close) = line.rfind('}') else {
                continue;
            };
            for pair in line[open + 1..close].split(',') {
                if let Some((key, _)) = pair.split_once('=') {
                    keys.push(key.to_string());
                }
            }
        }
        keys
    }

    fn bucket_count(rendered: &str, family: &str, le: &str) -> u64 {
        let needle = format!("le=\"{le}\"");
        rendered
            .lines()
            .find(|line| line.starts_with(&format!("{family}_bucket")) && line.contains(&needle))
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no {family}_bucket line for {needle} in:\n{rendered}"))
    }

    fn scalar(rendered: &str, prefix: &str) -> f64 {
        rendered
            .lines()
            .find(|line| line.starts_with(prefix))
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no line starting with {prefix} in:\n{rendered}"))
    }

    #[test]
    fn prometheus_export_uses_low_cardinality_labels() {
        let metrics = registry();
        metrics.record_http_response(StatusCode::OK, Duration::from_micros(15));
        metrics.record_http_response(StatusCode::NOT_FOUND, Duration::from_micros(7));
        metrics.record_public_response_created();

        let rendered = metrics.render_prometheus("moira-test", true, false);
        assert!(rendered.contains("moira_http_requests_total{service=\"moira-test\"} 2"));
        assert!(rendered.contains("status_class=\"2xx\""));
        assert!(rendered.contains("status_class=\"4xx\""));
        assert!(!rendered.contains("path="));
    }

    #[test]
    fn metrics_registry_emits_every_legacy_family_name_after_the_exporter_swap() {
        let metrics = registry();
        let rendered = metrics.render_prometheus("moira-test", true, true);

        for family in [
            HTTP_REQUESTS_TOTAL,
            HTTP_STATUS_CLASS_TOTAL,
            HTTP_LATENCY_MICROS_TOTAL,
            PUBLIC_RESPONSES_CREATED_TOTAL,
            PUBLIC_STREAMS_STARTED_TOTAL,
            WORKER_TICKS_TOTAL,
            RETENTION_RUNS_TOTAL,
            RETENTION_ROWS_DELETED_TOTAL,
            REDIS_ENABLED,
            WORKERS_ENABLED,
        ] {
            assert!(
                rendered.contains(&format!("# TYPE {family} ")),
                "family {family} disappeared from /metrics:\n{rendered}"
            );
            assert!(
                rendered
                    .lines()
                    .any(|line| line.starts_with(family) && line.contains("service=\"moira-test\"")),
                "family {family} lost its service label:\n{rendered}"
            );
        }

        // The four status classes and both retention tables are present from process start,
        // exactly as the hand-rolled renderer emitted them.
        for class in ["2xx", "3xx", "4xx", "5xx"] {
            assert!(rendered.contains(&format!("status_class=\"{class}\"")));
        }
        for table in [TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES] {
            assert!(rendered.contains(&format!("table=\"{table}\"")));
        }
    }

    #[test]
    fn http_latency_micros_total_is_still_emitted_alongside_the_duration_histogram() {
        let metrics = registry();
        metrics.record_http_response(StatusCode::OK, Duration::from_micros(1_500));
        metrics.record_http_latency(
            Some("/api/v1/admin/applications/{id}"),
            &Method::GET,
            StatusCode::OK,
            Duration::from_micros(1_500),
        );

        let rendered = metrics.render_prometheus("moira-test", false, false);
        assert_eq!(
            scalar(&rendered, HTTP_LATENCY_MICROS_TOTAL),
            1_500.0,
            "the superseded cumulative counter must keep being emitted"
        );
        assert!(rendered.contains(&format!("{HTTP_REQUEST_DURATION_SECONDS}_bucket")));
        assert!(rendered.contains(&format!("# TYPE {HTTP_REQUEST_DURATION_SECONDS} histogram")));
    }

    #[test]
    fn histogram_bucket_boundaries_are_inclusive_upper_bounds() {
        let metrics = registry();
        // Exactly on the 0.05 boundary.
        metrics.record_ttft(ProviderType::OpenAi, Duration::from_millis(50));
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert_eq!(
            bucket_count(&rendered, EXECUTION_TTFT_SECONDS, "0.05"),
            1,
            "an observation equal to a boundary belongs in that bucket"
        );
        assert_eq!(
            bucket_count(&rendered, EXECUTION_TTFT_SECONDS, "0.025"),
            0,
            "it must not land in the bucket below"
        );
    }

    #[test]
    fn observation_increments_every_bucket_at_or_above_its_value() {
        let metrics = registry();
        metrics.record_ttft(ProviderType::OpenAi, Duration::from_millis(100));
        let rendered = metrics.render_prometheus("moira-test", false, false);

        for le in ["0.025", "0.05"] {
            assert_eq!(bucket_count(&rendered, EXECUTION_TTFT_SECONDS, le), 0);
        }
        for le in ["0.1", "0.2", "0.4", "0.8", "1.5", "3", "5", "10", "20"] {
            assert_eq!(
                bucket_count(&rendered, EXECUTION_TTFT_SECONDS, le),
                1,
                "buckets are cumulative, so le={le} must include the 0.1s observation"
            );
        }
    }

    #[test]
    fn histogram_sum_and_count_match_the_observations_recorded() {
        let metrics = registry();
        for millis in [100u64, 200, 400] {
            metrics.record_ttft(ProviderType::Anthropic, Duration::from_millis(millis));
        }
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert_eq!(
            scalar(&rendered, &format!("{EXECUTION_TTFT_SECONDS}_count")),
            3.0
        );
        let sum = scalar(&rendered, &format!("{EXECUTION_TTFT_SECONDS}_sum"));
        assert!(
            (sum - 0.7).abs() < 1e-9,
            "expected the sum of the observations, got {sum}"
        );
    }

    #[test]
    fn histogram_renders_a_plus_inf_bucket_equal_to_count() {
        let metrics = registry();
        // Deliberately past the largest finite bucket.
        metrics.record_ttft(ProviderType::Gemini, Duration::from_secs(45));
        metrics.record_ttft(ProviderType::Gemini, Duration::from_millis(10));
        let rendered = metrics.render_prometheus("moira-test", false, false);

        let inf = bucket_count(&rendered, EXECUTION_TTFT_SECONDS, "+Inf");
        let count = scalar(&rendered, &format!("{EXECUTION_TTFT_SECONDS}_count"));
        assert_eq!(inf, 2);
        assert_eq!(f64::from(u32::try_from(inf).unwrap()), count);
    }

    #[test]
    fn route_label_is_derived_from_the_matched_path_template_not_the_raw_uri() {
        let metrics = registry();
        metrics.record_http_latency(
            Some("/api/v1/admin/applications/{id}"),
            &Method::GET,
            StatusCode::OK,
            Duration::from_millis(3),
        );
        // No matched route (a 404): the fallback is a constant, never the raw path.
        metrics.record_http_latency(
            None,
            &Method::GET,
            StatusCode::NOT_FOUND,
            Duration::from_millis(1),
        );
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(rendered.contains("route=\"/api/v1/admin/applications/{id}\""));
        assert!(rendered.contains(&format!("route=\"{ROUTE_UNMATCHED}\"")));
    }

    #[test]
    fn high_cardinality_identifiers_never_appear_as_label_values() {
        let execution_id = "018f6b1e-0000-7000-8000-000000000001";
        let response_id = "018f6b1e-0000-7000-8000-000000000002";
        let application_id = "018f6b1e-0000-7000-8000-000000000003";

        let metrics = registry();
        metrics.record_http_response(StatusCode::OK, Duration::from_millis(2));
        metrics.record_http_latency(
            Some("/api/v1/admin/applications/{id}"),
            &Method::PUT,
            StatusCode::OK,
            Duration::from_millis(2),
        );
        metrics.record_execution_latency(
            ProviderType::OpenAi,
            ExecutionStatus::Succeeded,
            None,
            Duration::from_millis(400),
        );
        metrics.record_ttft(ProviderType::OpenAi, Duration::from_millis(120));
        metrics.record_provider_outcome(
            ProviderType::OpenAi,
            "gpt-4o-mini",
            ExecutionStatus::Failed,
            Some(ExecutionFailureClass::ProviderTimeout),
        );
        metrics.record_db_pool_utilization(10, 7);
        metrics.record_retention_deleted(TABLE_RESPONSES, 4);
        let rendered = metrics.render_prometheus("moira-test", true, true);

        // The original guard, kept verbatim.
        assert!(!rendered.contains("path="));
        // Extended to the new families: no request-scoped identifier may reach a label.
        for id in [execution_id, response_id, application_id] {
            assert!(
                !rendered.contains(id),
                "identifier {id} reached the metrics body:\n{rendered}"
            );
        }
        for forbidden in ["execution_id=", "response_id=", "application_id=", "uri="] {
            assert!(
                !rendered.contains(forbidden),
                "label key {forbidden} must never exist:\n{rendered}"
            );
        }
        for key in label_keys(&rendered) {
            assert!(
                ALLOWED_LABEL_KEYS.contains(&key.as_str()),
                "unexpected label key {key:?}; add it to ALLOWED_LABEL_KEYS only after \
                 confirming its value set is closed:\n{rendered}"
            );
        }
    }

    #[test]
    fn provider_outcome_labels_come_from_the_closed_outcome_set() {
        // Success and cancellation come from ExecutionStatus.
        assert_eq!(
            execution_outcome_label(ExecutionStatus::Succeeded, None),
            "succeeded"
        );
        assert_eq!(
            execution_outcome_label(ExecutionStatus::Cancelled, None),
            "cancelled"
        );
        assert_eq!(
            execution_outcome_label(ExecutionStatus::Failed, None),
            "failed"
        );
        // Everything else is an ExecutionFailureClass rendered with its own wire spelling.
        assert_eq!(
            execution_outcome_label(
                ExecutionStatus::Failed,
                Some(ExecutionFailureClass::CircuitOpen)
            ),
            "circuit_open"
        );
        assert_eq!(
            execution_outcome_label(
                ExecutionStatus::Failed,
                Some(ExecutionFailureClass::DeadlineExceeded)
            ),
            "deadline_exceeded"
        );

        // The label is the enum's serde spelling, so dashboards and the API agree.
        let class = ExecutionFailureClass::ProviderRateLimited;
        let serialized = serde_json::to_string(&class).unwrap();
        assert_eq!(serialized, "\"provider_rate_limited\"");
        assert_eq!(failure_class_label(class), "provider_rate_limited");

        // And no free-form provider text can reach the label: the recorder takes the enum,
        // never a string.
        let metrics = registry();
        metrics.record_provider_outcome(
            ProviderType::Anthropic,
            "claude-sonnet",
            ExecutionStatus::Failed,
            Some(ExecutionFailureClass::ProviderUpstreamError),
        );
        let rendered = metrics.render_prometheus("moira-test", false, false);
        assert!(rendered.contains("outcome=\"provider_upstream_error\""));
    }

    #[test]
    fn sanitize_label_value_rejects_or_escapes_newlines_and_quotes() {
        // The hand-rolled `sanitize_label_value` is gone; this pins the exporter's escaping,
        // which now carries that responsibility, so a breaking exporter upgrade fails loudly.
        let metrics = MetricsRegistry::new("moira\"test\nservice", None);
        metrics.record_http_latency(
            Some("/api/v1/\"weird\"\npath"),
            &Method::GET,
            StatusCode::OK,
            Duration::from_millis(1),
        );
        let rendered = metrics.render_prometheus("ignored", false, false);

        for line in rendered.lines().filter(|line| !line.starts_with('#')) {
            let quotes = line.matches('"').count() - line.matches("\\\"").count();
            assert_eq!(quotes % 2, 0, "unbalanced quoting in sample line: {line}");
        }
        assert!(rendered.contains("\\\""), "quotes must be escaped");
        assert!(rendered.contains("\\n"), "newlines must be escaped");
        // The escaped newline must not have become a real line break inside a label.
        assert!(
            !rendered
                .lines()
                .any(|line| !line.starts_with('#') && line.contains('{') && !line.contains('}')),
            "a raw newline split a sample line:\n{rendered}"
        );
    }

    #[test]
    fn db_pool_gauge_reports_size_and_idle_from_supplied_values() {
        let metrics = registry();
        metrics.record_db_pool_utilization(12, 5);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(rendered.contains(&format!(
            "{DB_POOL_CONNECTIONS}{{service=\"moira-test\",state=\"total\"}} 12"
        )));
        assert!(rendered.contains(&format!(
            "{DB_POOL_CONNECTIONS}{{service=\"moira-test\",state=\"idle\"}} 5"
        )));
    }

    #[test]
    fn metrics_registries_do_not_share_state_across_instances() {
        // The regression guard for "local recorder, never a global install": the integration
        // suite builds several AppStates in one process, and a process-global recorder would
        // merge their series (and panic on the second install).
        let first = MetricsRegistry::new("moira-first", None);
        let second = MetricsRegistry::new("moira-second", None);

        for _ in 0..3 {
            first.record_public_response_created();
        }

        let first_body = first.render_prometheus("moira-first", false, false);
        let second_body = second.render_prometheus("moira-second", false, false);

        assert_eq!(scalar(&first_body, PUBLIC_RESPONSES_CREATED_TOTAL), 3.0);
        assert_eq!(
            scalar(&second_body, PUBLIC_RESPONSES_CREATED_TOTAL),
            0.0,
            "a second registry must not see the first registry's observations"
        );
        assert!(first_body.contains("service=\"moira-first\""));
        assert!(!first_body.contains("service=\"moira-second\""));
        assert!(second_body.contains("service=\"moira-second\""));
        assert!(!second_body.contains("service=\"moira-first\""));
    }

    #[test]
    fn method_and_status_labels_stay_inside_their_closed_sets() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(
            method_label(&Method::from_bytes(b"WEIRDVERB").unwrap()),
            "other",
            "an extension method must not open the label set"
        );
        assert_eq!(status_class(StatusCode::CONTINUE), "1xx");
        assert_eq!(status_class(StatusCode::OK), "2xx");
        assert_eq!(status_class(StatusCode::FOUND), "3xx");
        assert_eq!(status_class(StatusCode::NOT_FOUND), "4xx");
        assert_eq!(status_class(StatusCode::BAD_GATEWAY), "5xx");
    }

    #[test]
    fn informational_responses_increment_no_status_class_counter() {
        // Parity with the pre-swap `is_success`/`is_redirection`/… chain, which also left a
        // 1xx uncounted.
        let metrics = registry();
        metrics.record_http_response(StatusCode::CONTINUE, Duration::from_micros(5));
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert_eq!(scalar(&rendered, HTTP_REQUESTS_TOTAL), 1.0);
        for class in ["2xx", "3xx", "4xx", "5xx"] {
            let line = format!(
                "{HTTP_STATUS_CLASS_TOTAL}{{service=\"moira-test\",status_class=\"{class}\"}} 0"
            );
            assert!(rendered.contains(&line), "expected {line} in:\n{rendered}");
        }
        assert!(!rendered.contains("status_class=\"1xx\""));
    }

    #[test]
    fn unrecognised_retention_tables_are_dropped_rather_than_mislabelled() {
        let metrics = registry();
        metrics.record_retention_deleted("some_other_table", 99);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(!rendered.contains("some_other_table"));
        assert!(rendered.contains(&format!(
            "{RETENTION_ROWS_DELETED_TOTAL}{{service=\"moira-test\",table=\"{TABLE_RESPONSES}\"}} 0"
        )));
    }

    // -----------------------------------------------------------------------------------
    // Plan 10 wave 2 families.
    // -----------------------------------------------------------------------------------

    /// The seed is not optional. The exporter renders only families that have been
    /// registered, so an unseeded family is a *removal* from the scrape body until its
    /// first observation — and an alert written against it fires on "no data" rather
    /// than on the condition it was written for.
    #[test]
    fn the_new_families_are_seeded_at_zero_before_any_observation() {
        let rendered = registry().render_prometheus("moira-test", false, false);
        for family in [
            WORKER_JOBS_CLAIMED_TOTAL,
            WORKER_JOBS_COMPLETED_TOTAL,
            WORKER_JOBS_FAILED_TOTAL,
            WORKER_JOBS_DEAD_LETTER_TOTAL,
            WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL,
            WORKER_LEADER_HELD,
            REDIS_OPERATION_FAILURES_TOTAL,
            RUNTIME_INVALIDATIONS_TOTAL,
            RAG_CHUNKS_WRITTEN_TOTAL,
            RAG_EMBEDDINGS_WRITTEN_TOTAL,
            RAG_INGESTION_RUNS_TOTAL,
            RETRIEVAL_RUNS_TOTAL,
            MEMORY_EXTRACTION_RUNS_TOTAL,
            MEMORY_EXTRACTION_WRITTEN_TOTAL,
            MEMORY_EXTRACTION_REJECTED_TOTAL,
            SUMMARIZATION_RUNS_TOTAL,
            SUMMARIZATION_INLINE_REASONING_TOTAL,
        ] {
            assert!(
                rendered.contains(family),
                "{family} is absent from a fresh scrape body:\n{rendered}"
            );
        }
        // Both outcome series for summarization, for the same reason as retrieval below: the
        // alert an operator writes is on the *failure* rate, and a family with only a
        // `succeeded` series evaluates to "no data" on a process that has never failed one.
        for outcome in RAG_INGESTION_OUTCOMES {
            assert!(
                rendered.contains(&format!(
                    "{SUMMARIZATION_RUNS_TOTAL}{{service=\"moira-test\",outcome=\"{outcome}\"}} 0"
                )),
                "{SUMMARIZATION_RUNS_TOTAL} is missing its zero-seeded {outcome} series:\n{rendered}"
            );
        }
        // The two label-free extraction families must be seeded at literal zero, not merely
        // present: an alert on "extraction stopped writing memories" has to have a series to
        // evaluate on a process where extraction has never run.
        for family in [
            MEMORY_EXTRACTION_WRITTEN_TOTAL,
            MEMORY_EXTRACTION_REJECTED_TOTAL,
            // F57. The alert is "this deployment is storing chain-of-thought as summaries", and a
            // deployment on OpenAI never emits it — so without the seed the series is absent and
            // the alert reads "no data" on precisely the healthy case it exists to distinguish.
            SUMMARIZATION_INLINE_REASONING_TOTAL,
        ] {
            assert!(
                rendered.contains(&format!("{family}{{service=\"moira-test\"}} 0")),
                "{family} is not zero-seeded:\n{rendered}"
            );
        }
        for outcome in RAG_INGESTION_OUTCOMES {
            assert!(
                rendered.contains(&format!(
                    "{MEMORY_EXTRACTION_RUNS_TOTAL}{{service=\"moira-test\",outcome=\"{outcome}\"}}"
                )),
                "{MEMORY_EXTRACTION_RUNS_TOTAL} is missing its {outcome} series:\n{rendered}"
            );
        }
        // Both outcome series, not just the one a happy path would produce: an alert on
        // `rate(moira_retrieval_runs_total{outcome="failed"}[5m])` must have a series to
        // evaluate on a process that has never failed a retrieval.
        for outcome in RAG_INGESTION_OUTCOMES {
            assert!(
                rendered.contains(&format!(
                    "{RETRIEVAL_RUNS_TOTAL}{{service=\"moira-test\",outcome=\"{outcome}\"}}"
                )),
                "{RETRIEVAL_RUNS_TOTAL} is missing its {outcome} series:\n{rendered}"
            );
        }
        // The histogram is deliberately NOT seeded, matching every other latency family here.
        assert!(
            !rendered.contains(RETRIEVAL_LATENCY_SECONDS),
            "histograms are not seeded; seeding one would publish a fabricated zero observation"
        );
        // One series per declared job name, so a queue that has never run still
        // reports zero rather than nothing.
        for job_name in WORKER_JOB_NAMES {
            assert!(
                rendered.contains(&format!(
                    "{WORKER_JOBS_CLAIMED_TOTAL}{{service=\"moira-test\",job_name=\"{job_name}\"}} 0"
                )),
                "no zero-seeded claim counter for {job_name}:\n{rendered}"
            );
        }
    }

    /// Both summarization outcomes reach their own series — plan 11 Sub-Phase E.
    ///
    /// The seeding test above proves the series *exist*; this one proves the recorder actually
    /// emits to them. Those are different facts, and this repository has already shipped five
    /// metric labels that were seeded and never emitted (`HANDOFF.md` §2.3). The cheapest edit
    /// that breaks the property while leaving the seeding test green is a `record_*` that never
    /// runs, or one that maps both outcomes onto the same label — this catches both, because it
    /// asserts the two series carry *different* counts.
    #[test]
    fn summarization_runs_are_counted_against_the_outcome_that_happened() {
        let metrics = registry();
        metrics.record_summarization_run(true);
        metrics.record_summarization_run(true);
        metrics.record_summarization_run(false);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        let series = |outcome: &str| {
            format!("{SUMMARIZATION_RUNS_TOTAL}{{service=\"moira-test\",outcome=\"{outcome}\"}}")
        };
        assert!(
            rendered.contains(&format!("{} 2", series("succeeded"))),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("{} 1", series("failed"))),
            "{rendered}"
        );
    }

    /// The inline-reasoning family is emitted, and it does **not** displace a run — finding F57.
    ///
    /// The seeding test proves the series exists; this proves the recorder reaches it. The second
    /// half is the one worth writing down: the two families count different things, and the
    /// tempting simplification is to treat "the reply carried reasoning" as a third *outcome*. It
    /// is not — the run succeeded and wrote a row. So this asserts the inline count and the
    /// `succeeded` count are **both** 2 off the same two observations, which no re-labelling
    /// arrangement can satisfy.
    #[test]
    fn inline_reasoning_is_counted_alongside_the_run_not_instead_of_it() {
        let metrics = registry();
        metrics.record_summarization_run(true);
        metrics.record_summarization_inline_reasoning();
        metrics.record_summarization_run(true);
        metrics.record_summarization_inline_reasoning();
        metrics.record_summarization_run(true);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(
            rendered.contains(&format!(
                "{SUMMARIZATION_INLINE_REASONING_TOTAL}{{service=\"moira-test\"}} 2"
            )),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "{SUMMARIZATION_RUNS_TOTAL}{{service=\"moira-test\",outcome=\"succeeded\"}} 3"
            )),
            "an inline-reasoning run is still a successful run:\n{rendered}"
        );
    }

    /// `job_name` is a metric label, so it must come from the closed set. An
    /// unrecognised name is dropped rather than folded into another job's total,
    /// for the same reason an unrecognised retention table is.
    #[test]
    fn unrecognised_job_names_are_dropped_rather_than_mislabelled() {
        let metrics = registry();
        metrics.record_worker_job_claimed("job-invented-at-runtime", 7);
        metrics.record_worker_job_completed("job-invented-at-runtime");
        metrics.record_worker_leadership("job-invented-at-runtime", true);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(!rendered.contains("job-invented-at-runtime"), "{rendered}");
        assert!(rendered.contains(&format!(
            "{WORKER_JOBS_CLAIMED_TOTAL}{{service=\"moira-test\",job_name=\"{RETENTION_CLEANUP_WORKER}\"}} 0"
        )));
    }

    #[test]
    fn worker_job_counters_record_against_their_job_name() {
        let metrics = registry();
        metrics.record_worker_job_claimed(RETENTION_CLEANUP_WORKER, 3);
        metrics.record_worker_job_completed(RETENTION_CLEANUP_WORKER);
        metrics.record_worker_job_failed(RETENTION_CLEANUP_WORKER);
        metrics.record_worker_job_dead_lettered(RETENTION_CLEANUP_WORKER);
        metrics.record_worker_queue_enqueue_rejected();
        let rendered = metrics.render_prometheus("moira-test", false, false);

        let series = |family: &str| {
            format!("{family}{{service=\"moira-test\",job_name=\"{RETENTION_CLEANUP_WORKER}\"}}")
        };
        assert!(rendered.contains(&format!("{} 3", series(WORKER_JOBS_CLAIMED_TOTAL))));
        assert!(rendered.contains(&format!("{} 1", series(WORKER_JOBS_COMPLETED_TOTAL))));
        assert!(rendered.contains(&format!("{} 1", series(WORKER_JOBS_FAILED_TOTAL))));
        assert!(rendered.contains(&format!("{} 1", series(WORKER_JOBS_DEAD_LETTER_TOTAL))));
        assert!(rendered.contains(&format!(
            "{WORKER_QUEUE_ENQUEUE_REJECTED_TOTAL}{{service=\"moira-test\"}} 1"
        )));
    }

    /// The gauge is per-process by construction — each process owns its own
    /// registry — so summing it across a cluster's scrape targets counts leaders.
    /// A replica label would put a pod UUID in a label value, which
    /// `high_cardinality_identifiers_never_appear_as_label_values` forbids.
    #[test]
    fn leadership_is_a_zero_or_one_gauge_with_no_replica_dimension() {
        let metrics = registry();
        metrics.record_worker_leadership(RETENTION_CLEANUP_WORKER, true);
        let rendered = metrics.render_prometheus("moira-test", false, false);
        let series = format!(
            "{WORKER_LEADER_HELD}{{service=\"moira-test\",job_name=\"{RETENTION_CLEANUP_WORKER}\"}}"
        );
        assert!(rendered.contains(&format!("{series} 1")), "{rendered}");

        metrics.record_worker_leadership(RETENTION_CLEANUP_WORKER, false);
        let rendered = metrics.render_prometheus("moira-test", false, false);
        assert!(rendered.contains(&format!("{series} 0")), "{rendered}");
        // `replica=`, not `replica`: the family's own HELP text explains *why*
        // there is no replica dimension, and matching on the bare word would
        // fail on the documentation rather than on a label.
        assert!(!rendered.contains("replica="), "{rendered}");
    }

    /// A Redis outage turns every coordinated control into a refusal, so without
    /// this counter it is indistinguishable from genuine saturation: a wall of
    /// 429s and no way to tell which.
    #[test]
    fn redis_failures_are_counted_by_operation_from_a_closed_set() {
        let metrics = registry();
        metrics.record_redis_operation_failure(RedisOperation::RateLimit);
        metrics.record_redis_operation_failure(RedisOperation::PermitAcquire);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(rendered.contains(&format!(
            "{REDIS_OPERATION_FAILURES_TOTAL}{{service=\"moira-test\",operation=\"rate_limit\"}} 1"
        )));
        assert!(rendered.contains(&format!(
            "{REDIS_OPERATION_FAILURES_TOTAL}{{service=\"moira-test\",operation=\"permit_acquire\"}} 1"
        )));
        // Every arm seeded, so an operation that has never failed reads as zero.
        assert!(rendered.contains(&format!(
            "{REDIS_OPERATION_FAILURES_TOTAL}{{service=\"moira-test\",operation=\"subscribe\"}} 0"
        )));
    }

    /// Both channels are seeded, which is what lets an operator confirm the
    /// authoritative Postgres path is still carrying traffic when Redis is off —
    /// the default.
    #[test]
    fn invalidations_are_counted_per_channel_and_both_channels_are_seeded() {
        let metrics = registry();
        metrics.record_runtime_invalidation(InvalidationChannel::Postgres);
        let rendered = metrics.render_prometheus("moira-test", false, false);

        assert!(rendered.contains(&format!(
            "{RUNTIME_INVALIDATIONS_TOTAL}{{service=\"moira-test\",channel=\"postgres\"}} 1"
        )));
        assert!(rendered.contains(&format!(
            "{RUNTIME_INVALIDATIONS_TOTAL}{{service=\"moira-test\",channel=\"redis\"}} 0"
        )));
    }

    /// The label keys the new families introduce must pass the cardinality gate.
    #[test]
    fn the_new_families_use_only_allow_listed_label_keys() {
        let metrics = registry();
        metrics.record_worker_job_claimed(RETENTION_CLEANUP_WORKER, 1);
        metrics.record_worker_leadership(RETENTION_CLEANUP_WORKER, true);
        metrics.record_redis_operation_failure(RedisOperation::Publish);
        metrics.record_runtime_invalidation(InvalidationChannel::Redis);
        // Plan 09 wave 2. Driven through the public recorders rather than asserted on
        // the seeded families alone, so a denial reason that arrives as a raw string
        // from a call site — the one way an unbounded value could get in — is covered.
        metrics.record_admin_invite_created();
        metrics.record_admin_invite_redeemed();
        metrics.record_admin_invite_denied("domain_not_allowed");
        metrics.record_admin_invite_denied("colleague@example.com");
        metrics.record_admin_identity_revoked();
        metrics.record_admin_ownership_transferred();
        let rendered = metrics.render_prometheus("moira-test", true, true);

        for key in label_keys(&rendered) {
            assert!(
                ALLOWED_LABEL_KEYS.contains(&key.as_str()),
                "unexpected label key {key:?}:\n{rendered}"
            );
        }

        // An email address is the exact value plan 09 §0.5 forbids as a label. It must
        // land in `other`, not create a new series — a counter that silently accepted it
        // would put PII in every scrape and blow up cardinality on the one endpoint an
        // attacker can make Moira write to by failing redemptions in a loop.
        assert!(
            !rendered.contains("colleague@example.com"),
            "an invitee email must never reach a label value:\n{rendered}"
        );
        assert!(
            rendered.contains(r#"outcome="other""#),
            "an unrecognised denial reason must be folded into `other`, not dropped:\n{rendered}"
        );
    }
}
