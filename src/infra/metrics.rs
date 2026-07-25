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
    infra::workers::retention::{TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES},
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
            gauge!(DB_POOL_CONNECTIONS, "state" => "total").set(0.0);
            gauge!(DB_POOL_CONNECTIONS, "state" => "idle").set(0.0);
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

// `set_buckets_for_metric` has exactly one failure mode: an empty bucket slice. These
// compile-time assertions turn that runtime error into a build error, which is what makes
// the `expect` in `build_recorder` provably unreachable rather than merely unlikely.
const _: () = assert!(!HTTP_LATENCY_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!EXECUTION_LATENCY_BUCKETS_SECONDS.is_empty());
const _: () = assert!(!TTFT_BUCKETS_SECONDS.is_empty());

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
        ExecutionFailureClass::InternalError => "internal_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
