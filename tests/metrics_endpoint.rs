//! What `GET /metrics` actually serves, asserted over real HTTP.
//!
//! # Why this file exists separately from the unit tests in `src/infra/metrics.rs`
//!
//! Those tests call [`MetricsRegistry::render_prometheus`] directly. They prove the
//! *renderer* is correct — bucket semantics, `le="+Inf"`, label escaping, cardinality
//! discipline. They cannot prove any of it reaches a scraper, because they never touch the
//! route, the `prometheus_enabled` gate, the router's metrics middleware, or the
//! construction-time wiring in `AppState::new`. Plan 05 replaced a hand-rolled exposition
//! writer with `metrics` + `metrics-exporter-prometheus`; every one of those seams is new
//! or newly load-bearing, and a break in any of them is invisible from inside the module.
//!
//! So this file asserts on the **response body of the real endpoint**, served by
//! `moira::build_router` over a real TCP socket against a real PostgreSQL pool, and parses
//! the exposition text. It never reaches for a struct: `MetricsSnapshot` was deleted on
//! purpose in the swap, and the exported text is now the single source of truth. A helper
//! that read counters out of a Rust value would re-introduce exactly the second source of
//! truth the swap removed.
//!
//! # The three properties that are contractual
//!
//! 1. **Additive only.** Plan 05 promised `/metrics` consumers "Breaking API changes: none".
//!    [`PRE_EXISTING_FAMILIES`] is that promise written down, transcribed from the
//!    hand-rolled renderer as it stood at commit `41f1d99` (the last commit before the
//!    exporter swap), and
//!    `metrics_endpoint_still_exposes_every_pre_existing_metric_family` is what makes it
//!    defensible. A dashboard that breaks after a deploy is an outage nobody attributes to
//!    the metrics layer for hours.
//! 2. **Histograms, not summaries.** With no explicit bucket configuration
//!    `metrics-exporter-prometheus` renders a histogram as a *summary* — `quantile="…"`
//!    series instead of `_bucket{le="…"}`. Nothing panics and nothing looks wrong in the
//!    source; the scrape simply stops carrying the data every latency SLO is computed from.
//!    Only the endpoint output can catch that, so every exposure test below asserts the
//!    declared `# TYPE` and the absence of `quantile=`.
//! 3. **No unbounded label values.** A UUID reaching a label is not untidy output, it is a
//!    memory-exhaustion vector in the scrape path and an identifier disclosure on an
//!    endpoint that is deliberately unauthenticated.
//!    `metrics_endpoint_never_exposes_raw_paths_or_identifiers` drives identifiers this
//!    test controls through the router and greps the scrape for them, so it can genuinely
//!    fail rather than pass by never having produced a risky label in the first place.

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::{StatusCode, header};
use moira::{application::ExecutionService, domain::ExecutionStatus};
use reqwest::Client;
use serde_json::Value;
use support::mock_openai::{MockOpenAiServer, ProviderScript};
use support::{LifecycleFixture, MoiraHttpServer, RuntimePolicy};
use tokio::time::timeout;
use uuid::Uuid;

/// Every metric family `/metrics` emitted before plan 05, with the `# TYPE` the hand-rolled
/// renderer declared for it.
///
/// Transcribed from `git show 41f1d99:src/infra/metrics.rs` — specifically its ten
/// `write_metric_header` call sites — not from the current module, which is the thing under
/// test and would agree with its own regression. Adding a family here is meaningless;
/// *removing* one is the only edit that can make this list weaker, which is why it is a
/// const rather than being derived at runtime.
const PRE_EXISTING_FAMILIES: &[(&str, &str)] = &[
    ("moira_http_requests_total", "counter"),
    ("moira_http_response_status_class_total", "counter"),
    ("moira_http_latency_micros_total", "counter"),
    ("moira_public_responses_created_total", "counter"),
    ("moira_public_streams_started_total", "counter"),
    ("moira_worker_ticks_total", "counter"),
    ("moira_retention_runs_total", "counter"),
    ("moira_retention_rows_deleted_total", "counter"),
    ("moira_redis_enabled", "gauge"),
    ("moira_workers_enabled", "gauge"),
];

const HTTP_DURATION: &str = "moira_http_request_duration_seconds";
const EXECUTION_DURATION: &str = "moira_execution_duration_seconds";
const EXECUTION_TTFT: &str = "moira_execution_ttft_seconds";
const PROVIDER_OUTCOME: &str = "moira_provider_outcome_total";
const DB_POOL: &str = "moira_db_pool_connections";

/// The `Content-Type` the endpoint has always sent. `src/http/observability.rs` is
/// byte-unchanged by plan 05; this pins that from the outside so a future edit cannot
/// quietly drop the Prometheus text-format version token that scrapers negotiate on.
const EXPOSITION_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Ceiling on the pool-permit barrier in
/// `metrics_endpoint_exposes_db_pool_gauges_reflecting_the_live_pool`.
///
/// **This bounds a deadlock, not a race.** Every acquisition it guards either completes in
/// microseconds or never completes at all — the only way to miss this budget is for
/// something to hold a fixture-pool permit for the fixture's whole lifetime, in which case
/// waiting longer would not help. It exists so that failure reports a diagnosis instead of
/// hanging anonymously until the harness gives up.
const POOL_BARRIER: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

/// Starts a Moira server on the fixture's `AppState` with `prometheus_enabled` overridden.
///
/// Only `settings` is replaced on the clone; every `Arc`-backed component — crucially the
/// [`MetricsRegistry`] itself — is shared with `fixture.state`. That is what lets a test
/// drive an execution through `fixture.execution_service()` and then observe its
/// observations in this server's scrape.
///
/// The registry's `service` label is deliberately **not** overridable here: it is a
/// builder-time global label fixed when `AppState::new` constructed the recorder, so it
/// keeps the fixture's `telemetry.service_name` regardless of what this clone's settings
/// say. `render_prometheus`'s `service_name` argument survives only to keep the handler's
/// call site unedited. Tests therefore compare against
/// `fixture.state.settings.telemetry.service_name`.
async fn start_server(fixture: &LifecycleFixture, prometheus_enabled: bool) -> MoiraHttpServer {
    let mut state = fixture.state.clone();
    let mut settings = (*fixture.state.settings).clone();
    settings.telemetry.prometheus_enabled = prometheus_enabled;
    state.settings = Arc::new(settings);
    MoiraHttpServer::start(state).await
}

/// The `service` label value every family must carry, taken from the settings the registry
/// was actually constructed with.
fn service_label(fixture: &LifecycleFixture) -> String {
    fixture.state.settings.telemetry.service_name.clone()
}

struct HttpResult {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: String,
}

async fn get(client: &Client, url: &str, headers: &[(&str, &str)]) -> HttpResult {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.expect("send request");
    HttpResult {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.text().await.expect("read response body"),
    }
}

/// Scrapes `/metrics` and asserts the transport-level half of the contract before handing
/// the exposition text back.
///
/// No synchronisation is needed between driving traffic and scraping: `metrics_middleware`
/// records on the response path *before* the response leaves the router, so a completed
/// client request implies a completed recording (CONVENTIONS.md §3 — no `sleep()`).
async fn scrape(client: &Client, server: &MoiraHttpServer) -> String {
    let result = get(client, &format!("{}/metrics", server.base_url), &[]).await;
    assert_eq!(
        result.status,
        StatusCode::OK,
        "/metrics did not return 200: {}",
        result.body
    );
    assert_eq!(
        result
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some(EXPOSITION_CONTENT_TYPE),
        "the Prometheus text-format content type changed"
    );
    assert_eq!(
        result
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "a cached scrape is a stale scrape"
    );
    result.body
}

// ---------------------------------------------------------------------------------------
// Exposition parsing
//
// Deliberately hand-rolled and tiny rather than pulling in a Prometheus parser crate: the
// point of these tests is that the *bytes on the wire* are what a scraper expects, and a
// parser that is lenient in the same places the exporter is would hide precisely the
// regressions this file is here to catch.
// ---------------------------------------------------------------------------------------

/// The metric name of a sample line — everything before the label set or the value.
fn metric_name(line: &str) -> &str {
    let end = line.find(['{', ' ']).unwrap_or(line.len());
    &line[..end]
}

/// Every sample line for exactly this metric name. Exact-match on the name, so
/// `moira_http_request_duration_seconds` never collects its own `_bucket`/`_sum`/`_count`
/// series by accident.
fn sample_lines<'a>(body: &'a str, name: &str) -> Vec<&'a str> {
    body.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter(|line| metric_name(line) == name)
        .collect()
}

fn sample_value(line: &str) -> f64 {
    line.rsplit(' ')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("sample line carries no numeric value: {line}"))
}

fn label<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let open = line.find('{')?;
    let close = line.rfind('}')?;
    line[open + 1..close].split(',').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.trim_matches('"'))
    })
}

/// Sample lines for `name` whose label set matches every `(key, value)` pair given.
fn series<'a>(body: &'a str, name: &str, labels: &[(&str, &str)]) -> Vec<&'a str> {
    sample_lines(body, name)
        .into_iter()
        .filter(|line| {
            labels
                .iter()
                .all(|(key, value)| label(line, key) == Some(*value))
        })
        .collect()
}

/// The value of the one series matching `labels`, asserting that exactly one exists.
///
/// The uniqueness assertion is load-bearing: a duplicated series means two label sets
/// collapsed into one family, which is how a scrape silently starts double-counting.
fn series_value(body: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    let lines = series(body, name, labels);
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one {name} series for {labels:?}, found {lines:#?}"
    );
    sample_value(lines[0])
}

/// The type declared by the family's `# TYPE` line, or `None` when the family is absent
/// from the scrape entirely.
fn declared_type<'a>(body: &'a str, family: &str) -> Option<&'a str> {
    let prefix = format!("# TYPE {family} ");
    body.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
}

/// Asserts a family is exposed as a real Prometheus histogram: the declared type, all three
/// derived series, and a `+Inf` bucket equal to `_count`.
///
/// Returns the observation count so callers can pin it to the traffic they drove.
fn assert_histogram_shape(body: &str, family: &str, labels: &[(&str, &str)]) -> f64 {
    assert_eq!(
        declared_type(body, family),
        Some("histogram"),
        "{family} is not exposed as a histogram. If this says \"summary\", the exporter fell \
         back to quantiles because its bucket configuration was lost — every latency SLO \
         computed from _bucket series silently stops working:\n{body}"
    );

    let bucket = format!("{family}_bucket");
    let buckets = series(body, &bucket, labels);
    assert!(
        !buckets.is_empty(),
        "{bucket} has no series for {labels:?}:\n{body}"
    );

    let count = series_value(body, &format!("{family}_count"), labels);
    let sum = series_value(body, &format!("{family}_sum"), labels);
    assert!(
        sum >= 0.0,
        "{family}_sum must be a non-negative duration, got {sum}"
    );

    let mut inf_labels = labels.to_vec();
    inf_labels.push(("le", "+Inf"));
    assert_eq!(
        series_value(body, &bucket, &inf_labels),
        count,
        "the +Inf bucket must hold every observation the _count reports:\n{body}"
    );
    count
}

/// The `le` boundaries a histogram family exposes for one series, parsed as floats.
///
/// Parsed rather than string-compared because the exporter's float formatting (`1` for
/// `1.0`, `2.5` for `2.5`) is its own detail, not part of Moira's contract; pinning the
/// spelling would turn a harmless upstream formatting change into a failing test.
fn bucket_bounds(body: &str, family: &str, labels: &[(&str, &str)]) -> Vec<f64> {
    let mut bounds: Vec<f64> = series(body, &format!("{family}_bucket"), labels)
        .into_iter()
        .map(|line| {
            let le = label(line, "le")
                .unwrap_or_else(|| panic!("a _bucket line without an le label: {line}"));
            if le == "+Inf" {
                f64::INFINITY
            } else {
                le.parse()
                    .unwrap_or_else(|_| panic!("unparseable le boundary {le:?} in: {line}"))
            }
        })
        .collect();
    bounds.sort_by(|a, b| a.partial_cmp(b).expect("bucket bounds are never NaN"));
    bounds
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

/// The additive-only promise, asserted at the endpoint.
///
/// Plan 05's PR states "Breaking API/OpenAPI changes: none" for `/metrics`. That claim is
/// only worth anything if something checks it, and the risk is specific to the exporter
/// swap: the hand-rolled renderer printed all ten families unconditionally, while
/// `metrics-exporter-prometheus` renders only metrics that have been *registered*. A family
/// nobody has touched since process start therefore disappears unless it was explicitly
/// seeded — which is a removal from a scraper's point of view, silent, and only visible
/// from a fresh process.
///
/// So this test scrapes **first**, before driving any traffic at all. Every family must
/// already be there, carrying the same `# TYPE` and the same `service` label it carried at
/// `41f1d99`.
#[tokio::test]
async fn metrics_endpoint_still_exposes_every_pre_existing_metric_family() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let redis_enabled = fixture.state.redis.is_some();
    let workers_enabled = fixture.state.workers.enabled();
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    // The very first request this process makes. Nothing has been recorded yet, which is
    // exactly the condition under which an unseeded family would be missing.
    let body = scrape(&client, &server).await;

    for (family, expected_type) in PRE_EXISTING_FAMILIES {
        assert_eq!(
            declared_type(&body, family),
            Some(*expected_type),
            "family {family} lost its `# TYPE {expected_type}` declaration; a scraper that \
             ingested it as a {expected_type} before the exporter swap now sees something \
             else:\n{body}"
        );
        let labelled = series(&body, family, &[("service", service.as_str())]);
        assert!(
            !labelled.is_empty(),
            "family {family} is not exposed with its service=\"{service}\" label:\n{body}"
        );
    }

    // `moira_http_latency_micros_total` is the one family the swap had a live reason to
    // drop: it is superseded by the duration histogram. It was deliberately kept, because
    // removing it would break any dashboard deriving an average from the cumulative sum.
    assert_eq!(
        series_value(
            &body,
            "moira_http_latency_micros_total",
            &[("service", service.as_str())]
        ),
        0.0,
        "the superseded cumulative latency counter must still be exposed, at zero, from \
         process start:\n{body}"
    );

    // The hand-rolled renderer emitted all four status classes and both retention tables
    // whether or not anything had incremented them. Their absence would break any query
    // that divides by them.
    for class in ["2xx", "3xx", "4xx", "5xx"] {
        assert_eq!(
            series_value(
                &body,
                "moira_http_response_status_class_total",
                &[("service", service.as_str()), ("status_class", class)]
            ),
            0.0,
            "status class {class} is missing from a fresh scrape:\n{body}"
        );
    }
    for table in ["idempotency_records", "responses"] {
        assert_eq!(
            series_value(
                &body,
                "moira_retention_rows_deleted_total",
                &[("service", service.as_str()), ("table", table)]
            ),
            0.0,
            "retention table {table} is missing from a fresh scrape:\n{body}"
        );
    }

    // Both boolean gauges are set at render time from the live process configuration, not
    // seeded — so this also proves the render-time hook still runs.
    assert_eq!(
        series_value(
            &body,
            "moira_redis_enabled",
            &[("service", service.as_str())]
        ),
        f64::from(u8::from(redis_enabled))
    );
    assert_eq!(
        series_value(
            &body,
            "moira_workers_enabled",
            &[("service", service.as_str())]
        ),
        f64::from(u8::from(workers_enabled))
    );

    server.shutdown().await;
}

/// The HTTP latency histogram, proven to reach a scraper.
///
/// `moira_http_request_duration_seconds` is new in plan 05 and is the family every HTTP
/// latency SLO will be computed from. Three things can go wrong between the recording call
/// and the scrape, none of which the unit tests can see: the middleware may not be wired
/// into the production router at all, the exporter may render the family as a summary
/// because its bucket configuration was lost, and the route label may not survive the trip
/// from `MatchedPath`.
///
/// The observation count is pinned to the exact number of requests driven, so a middleware
/// that fires twice per request — or not at all — fails here rather than showing up as a
/// mysteriously doubled latency rate in production.
#[tokio::test]
async fn metrics_endpoint_exposes_http_latency_histogram_buckets_sum_and_count() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    const PROBES: usize = 3;
    for _ in 0..PROBES {
        let result = get(&client, &format!("{}/health/live", server.base_url), &[]).await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    }

    let body = scrape(&client, &server).await;
    let health = [
        ("service", service.as_str()),
        ("route", "/health/live"),
        ("method", "GET"),
        ("status_class", "2xx"),
    ];

    let count = assert_histogram_shape(&body, HTTP_DURATION, &health);
    assert_eq!(
        count, PROBES as f64,
        "the histogram must hold exactly one observation per request served:\n{body}"
    );

    // The full configured bucket set, not a sample of it. A truncated bucket list still
    // renders a valid-looking histogram while making high-percentile queries meaningless.
    assert_eq!(
        bucket_bounds(&body, HTTP_DURATION, &health),
        vec![
            0.005,
            0.01,
            0.025,
            0.05,
            0.1,
            0.25,
            0.5,
            1.0,
            2.5,
            5.0,
            10.0,
            f64::INFINITY
        ],
        "the HTTP latency bucket boundaries changed:\n{body}"
    );

    // A local health probe cannot plausibly take ten seconds, so every observation must sit
    // in a finite bucket. This is what catches a unit mix-up — recording milliseconds into a
    // seconds-denominated family would pile every observation into +Inf.
    assert_eq!(
        series_value(
            &body,
            &format!("{HTTP_DURATION}_bucket"),
            &[
                ("service", service.as_str()),
                ("route", "/health/live"),
                ("method", "GET"),
                ("status_class", "2xx"),
                ("le", "10")
            ]
        ),
        PROBES as f64,
        "a local health probe landed outside the 10s bucket; the recorded unit is wrong:\n{body}"
    );

    // The additive half of the same story: the superseded cumulative counter kept counting
    // alongside the histogram rather than being replaced by it.
    assert!(
        series_value(
            &body,
            "moira_http_latency_micros_total",
            &[("service", service.as_str())]
        ) > 0.0,
        "the legacy cumulative latency counter stopped advancing:\n{body}"
    );

    // With no bucket configuration the exporter renders histograms as quantile summaries.
    // Moira exposes no summaries at all, so a single `quantile=` anywhere is proof of that
    // regression.
    assert!(
        !body.contains("quantile="),
        "a histogram family degraded into a quantile summary:\n{body}"
    );

    server.shutdown().await;
}

/// The two provider-side latency histograms, after a real streamed execution.
///
/// These families only appear once something has been observed — they are deliberately not
/// seeded, because seeding would require inventing a fake `provider_type` series. That makes
/// them the families most likely to be silently absent: nothing in the process emits them
/// until a provider is actually called, so only an end-to-end execution can prove the
/// recording calls in `src/application/execution.rs` reach the exporter.
///
/// The mock provider streams, which is the only path that records TTFT at all.
#[tokio::test]
async fn metrics_endpoint_exposes_execution_latency_and_ttft_histograms() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let provider = MockOpenAiServer::start([ProviderScript::Stream {
        deltas: vec!["metrics".to_string(), "-probe".to_string()],
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    let mut handle = fixture
        .execution_service()
        .execute_stream(fixture.command(true))
        .await
        .expect("start streamed execution");
    // Draining to completion is the synchronisation: TTFT is recorded on the first
    // output-bearing chunk and the attempt latency when the attempt finishes, so awaiting
    // the outcome guarantees both observations exist before the scrape.
    while handle.events.recv().await.is_some() {}
    let outcome = (&mut handle.outcome)
        .await
        .expect("stream outcome sender dropped")
        .expect("streamed execution failed");
    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    drop(handle);

    let body = scrape(&client, &server).await;
    let attempt = [
        ("service", service.as_str()),
        ("provider_type", "openai_compatible"),
        ("outcome", "succeeded"),
    ];
    let ttft = [
        ("service", service.as_str()),
        ("provider_type", "openai_compatible"),
    ];

    assert_eq!(
        assert_histogram_shape(&body, EXECUTION_DURATION, &attempt),
        1.0,
        "one attempt must record exactly one execution-latency observation:\n{body}"
    );
    assert_eq!(
        assert_histogram_shape(&body, EXECUTION_TTFT, &ttft),
        1.0,
        "one streamed attempt must record exactly one TTFT observation:\n{body}"
    );

    // Both families carry a far longer tail than the HTTP buckets, because a provider call
    // that takes two minutes is a normal observation rather than an outlier. Pinning the
    // largest finite boundary catches a copy-pasted bucket set, which would push every real
    // provider latency into +Inf and make the p99 unreadable.
    let execution_bounds = bucket_bounds(&body, EXECUTION_DURATION, &attempt);
    assert_eq!(execution_bounds.last(), Some(&f64::INFINITY));
    assert_eq!(
        execution_bounds[execution_bounds.len() - 2],
        120.0,
        "execution latency lost its long tail:\n{body}"
    );
    let ttft_bounds = bucket_bounds(&body, EXECUTION_TTFT, &ttft);
    assert_eq!(
        ttft_bounds[ttft_bounds.len() - 2],
        20.0,
        "TTFT lost its long tail:\n{body}"
    );

    // TTFT is measured from the same `Instant` as the attempt latency, so it can never
    // exceed it. A violation here means the two are being timed from different origins,
    // which would make every "time to first token" dashboard quietly wrong.
    let ttft_sum = series_value(&body, &format!("{EXECUTION_TTFT}_sum"), &ttft);
    let attempt_sum = series_value(&body, &format!("{EXECUTION_DURATION}_sum"), &attempt);
    assert!(
        ttft_sum <= attempt_sum,
        "TTFT ({ttft_sum}s) exceeded the attempt latency it is measured within \
         ({attempt_sum}s):\n{body}"
    );

    assert!(
        !body.contains("quantile="),
        "a provider latency family degraded into a quantile summary:\n{body}"
    );

    server.shutdown().await;
    provider.shutdown().await;
}

/// The provider-outcome counter, and the label set it is allowed to carry.
///
/// This family is the one with a genuine cardinality question: it carries `model_key`, which
/// is admin-configured runtime configuration rather than caller input. That is a defensible
/// label — the operator's model catalogue is bounded — but the adjacent identifiers are not.
/// The provider, model and credential UUIDs are all in scope at the recording call site and
/// all one careless edit away from becoming labels, at which point every new credential
/// permanently adds a series to the scrape.
///
/// So this test asserts both halves: the counter counts, and the three UUIDs the fixture
/// owns appear nowhere in the body.
#[tokio::test]
async fn metrics_endpoint_exposes_provider_outcome_counters_with_bounded_labels() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let provider = MockOpenAiServer::start([
        ProviderScript::Completion {
            text: "first".to_string(),
        },
        ProviderScript::Completion {
            text: "second".to_string(),
        },
    ])
    .await;
    let provider_fixture = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    for attempt in 0..2 {
        let outcome = fixture
            .execution_service()
            .execute(fixture.command(false))
            .await
            .expect("execution result");
        assert_eq!(
            outcome.status,
            ExecutionStatus::Succeeded,
            "attempt {attempt} did not succeed: {:?}",
            outcome.failure
        );
    }

    let body = scrape(&client, &server).await;

    assert_eq!(
        declared_type(&body, PROVIDER_OUTCOME),
        Some("counter"),
        "{PROVIDER_OUTCOME} must be a counter:\n{body}"
    );
    assert_eq!(
        series_value(
            &body,
            PROVIDER_OUTCOME,
            &[
                ("service", service.as_str()),
                ("provider_type", "openai_compatible"),
                ("model_key", "test-model"),
                ("outcome", "succeeded"),
            ]
        ),
        2.0,
        "two successful attempts must produce a count of two on one series:\n{body}"
    );

    // Every label key the family is permitted to carry. A new key appearing here is not
    // necessarily wrong, but it is always a cardinality decision, and this is the assertion
    // that forces it to be made deliberately rather than discovered in a scrape.
    for line in sample_lines(&body, PROVIDER_OUTCOME) {
        let open = line
            .find('{')
            .unwrap_or_else(|| panic!("unlabelled: {line}"));
        let close = line
            .rfind('}')
            .unwrap_or_else(|| panic!("unlabelled: {line}"));
        for pair in line[open + 1..close].split(',') {
            let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
            assert!(
                ["service", "provider_type", "model_key", "outcome"].contains(&key),
                "{PROVIDER_OUTCOME} grew an unexpected label {key:?}; confirm its value set \
                 is closed before allowing it:\n{line}"
            );
        }
    }

    // The identifiers that were live at the recording call site and must not have leaked
    // into it. These are real values from this run, so the grep can actually fail.
    for (what, id) in [
        ("provider id", provider_fixture.provider_id),
        ("provider model id", provider_fixture.model_id),
        ("credential id", provider_fixture.credential_id),
        ("application id", fixture.application_id),
        ("route id", fixture.route_id),
    ] {
        assert!(
            !body.contains(&id.to_string()),
            "the {what} {id} reached the metrics body as a label value:\n{body}"
        );
    }

    server.shutdown().await;
    provider.shutdown().await;
}

/// The database connection-pool gauges, proven to track the pool rather than a constant.
///
/// A gauge that is exposed but frozen is worse than a missing one: it reads as a healthy
/// pool during the exact incident — connection exhaustion — it exists to diagnose. Because
/// the pool is sampled once per scrape inside `render_prometheus`, the only way to prove the
/// sampling is live is to change the pool's state between scrapes and watch the numbers
/// move.
///
/// # Why this test drives the pool to saturation instead of borrowing one connection
///
/// The previous version took a baseline scrape, acquired one connection, and asserted the
/// idle gauge had dropped by *exactly* one. It failed once under CPU contention with
/// `left: 1.0, right: 0.0` and passed nine reruns immediately afterwards (finding **F28**,
/// the metrics-gauge flake — not the unrelated F28 about inline memory extraction delaying
/// the terminal SSE event). Its doc comment asserted two things that are not true:
///
/// * *"Nothing else touches this pool."* **sqlx does.** Returning a `PoolConnection` is
///   asynchronous: `Drop` spawns a task, and that task issues a `ping()` round-trip to
///   PostgreSQL *before* it puts the connection back and increments `num_idle`
///   (`sqlx-core-0.8.6/src/pool/connection.rs`, `Floating::return_to_pool`). So between a
///   `drop` and the counter moving there is a whole database round-trip, and a scrape
///   landing inside that window reads a pool that is still settling. That is precisely what
///   the observed failure was: the baseline scrape sampled `idle = 1` while a second
///   connection's return was still in flight, the return landed, and the "busy" scrape then
///   read `idle = 1` again instead of `0`.
/// * *"`LifecycleFixture` serialises the suite."* It does not, and has not since fixtures
///   were given private databases — `tests/support/mod.rs` says so in as many words
///   (`CONCURRENT_FIXTURES`: *"This is **not** an isolation device"*). Four fixtures run at
///   once; what makes them safe is the private database, not serialisation.
///
/// Measured: replaying the old shape back to back, with nothing between the `drop` and the
/// next baseline scrape, reproduced the failure **8 times in 10**. The delay that made it
/// pass in practice was incidental — fixture setup and the first scrape happened to give
/// the in-flight return time to land.
///
/// # What makes the numbers below exact rather than bounded
///
/// **`acquire` is a barrier.** A connection whose return is still in flight has not yet
/// released its semaphore permit, so taking *every* permit cannot return until every return
/// has completed. And [`PoolConnection::return_to_pool`] runs that same return eagerly and
/// awaits it, instead of spawning it. Between them the pool reaches a state with no
/// in-flight work at all, and its numbers are then exactly known — no sleep, no retry, no
/// tolerance. The three exact idle observations below — `capacity`, `capacity - 1`, `0` —
/// held 15/15 and 20/20 in direct measurement.
///
/// The scrape at full saturation is not just bookkeeping: it is the incident this gauge
/// exists for, and it also proves `/metrics` answers without a pooled connection — which is
/// what lets this test hold every permit in the first place.
///
/// A fourth scrape, taken *before* any of that, carries the one thing saturation destroys:
/// while the pool is saturated `pool.size()` and `max_connections` are the same number, so
/// nothing after the barrier can tell a live `total` gauge from one hard-wired to the
/// configured ceiling. That mutation left this test green until the early scrape was added.
#[tokio::test]
async fn metrics_endpoint_exposes_db_pool_gauges_reflecting_the_live_pool() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    let capacity = fixture.pool.options().get_max_connections();
    // Three distinct idle readings are what makes this test able to fail: `capacity`,
    // `capacity - 1` and `0`. At a capacity of one the first two collapse into each other
    // and the middle observation stops carrying information.
    assert!(
        capacity >= 2,
        "the fixture pool must allow at least two connections for the idle gauge to be \
         observed at three distinct values, got {capacity}"
    );

    let total = [("service", service.as_str()), ("state", "total")];
    let idle = [("service", service.as_str()), ("state", "idle")];

    // One scrape before the pool is touched, for one reason: every observation after this
    // saturates the pool, so in all of them `pool.size()` and the configured ceiling are the
    // same number. A `total` gauge sourced from `max_connections` instead of the live size
    // would agree with every one of them and be wrong in production, where the two differ
    // for as long as the pool is warming. Here they still differ — the fixture has used the
    // pool but not exhausted it.
    //
    // Only `total` is asserted here. `idle` is deliberately left alone: this is the one
    // sample in the test taken from a pool that may still be settling, which is the whole
    // subject of F28, and `pool.size()` is the number that is *not* affected by it — a
    // connection whose return is in flight is already counted in `size` and not yet counted
    // in `num_idle`.
    let initial = scrape(&client, &server).await;
    assert_eq!(
        declared_type(&initial, DB_POOL),
        Some("gauge"),
        "{DB_POOL} must be a gauge:\n{initial}"
    );
    let initial_total = series_value(&initial, DB_POOL, &total);
    assert!(
        initial_total >= 1.0,
        "the fixture has already run queries through this pool, so at least one connection \
         must exist:\n{initial}"
    );
    assert!(
        initial_total < f64::from(capacity),
        "the fixture opened all {capacity} connections before this test started, so no \
         scrape below can tell a `total` gauge sourced from `pool.size()` apart from one \
         sourced from the configured ceiling, and the test would pass either way:\n{initial}"
    );

    // Take every permit, then hand every connection back eagerly. The acquisitions wait out
    // any return still in flight from fixture setup; the eager returns replace the spawned
    // ones, so nothing is pending when the loop ends. `capacity` connections are open and
    // all of them are idle — exactly, not approximately.
    let mut held = Vec::with_capacity(capacity as usize);
    for slot in 0..capacity {
        held.push(
            timeout(POOL_BARRIER, fixture.pool.acquire())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "fixture connection {slot} could not be acquired within \
                         {POOL_BARRIER:?}: something holds a permit on the fixture pool for \
                         the fixture's lifetime (a PgListener?), so the pool cannot be \
                         brought to a known state and none of the assertions below would \
                         mean what they say"
                    )
                })
                .unwrap_or_else(|error| panic!("acquire fixture connection {slot}: {error}")),
        );
    }
    for mut connection in held {
        connection.return_to_pool().await;
    }

    let settled = scrape(&client, &server).await;
    assert_eq!(
        series_value(&settled, DB_POOL, &total),
        f64::from(capacity),
        "every permit was taken and every connection returned, so the pool holds exactly \
         {capacity} connections:\n{settled}"
    );
    assert_eq!(
        series_value(&settled, DB_POOL, &idle),
        f64::from(capacity),
        "nothing is checked out and no return is in flight, so all {capacity} connections \
         are idle. A gauge reporting anything else is not reading `num_idle`:\n{settled}"
    );

    // One connection checked out of a settled pool: `idle` must fall by exactly one and
    // `total` must not move, because `acquire` reuses an idle connection rather than
    // opening another.
    let one = fixture
        .pool
        .acquire()
        .await
        .expect("hold one pooled connection");
    let busy = scrape(&client, &server).await;
    assert_eq!(
        series_value(&busy, DB_POOL, &total),
        f64::from(capacity),
        "acquiring an already-idle connection must not change the pool size:\n{busy}"
    );
    assert_eq!(
        series_value(&busy, DB_POOL, &idle),
        f64::from(capacity) - 1.0,
        "the idle gauge did not follow the live pool; it is sampled from a stale or constant \
         source:\n{busy}"
    );

    // Full exhaustion — the incident the gauge exists to diagnose. `idle` must reach zero,
    // and it must reach zero *for the right reason*: a gauge frozen at its construction
    // value of zero would satisfy this line alone, which is why the settled scrape above
    // pins it at `capacity` first.
    let mut all = vec![one];
    for slot in 1..capacity {
        all.push(
            timeout(POOL_BARRIER, fixture.pool.acquire())
                .await
                .unwrap_or_else(|_| panic!("exhausting connection {slot} timed out"))
                .unwrap_or_else(|error| panic!("acquire exhausting connection {slot}: {error}")),
        );
    }
    let exhausted = scrape(&client, &server).await;
    assert_eq!(
        series_value(&exhausted, DB_POOL, &total),
        f64::from(capacity),
        "exhausting the pool must not change how many connections exist:\n{exhausted}"
    );
    assert_eq!(
        series_value(&exhausted, DB_POOL, &idle),
        0.0,
        "with every connection checked out the idle gauge must read zero; a gauge that still \
         reports spare capacity during exhaustion is worse than no gauge at all:\n{exhausted}"
    );

    for mut connection in all {
        connection.return_to_pool().await;
    }
    server.shutdown().await;
}

/// The security test: identifiers a caller controls must never reach a label value.
///
/// The `/metrics` endpoint is deliberately unauthenticated, so anything that reaches a label
/// is disclosed to anyone who can reach the port — and because Prometheus labels are keys,
/// an unbounded label set is also a memory-exhaustion vector in the scrape path.
///
/// Being able to *fail* is the whole design of this test. Each probe below seeds a value
/// this test owns into a different part of the request that a naive implementation would
/// reach for: a path parameter on a templated route, an entire unmatched path, a query
/// string, and a correlation header. Each is a real regression somebody could plausibly
/// write — labelling by `req.uri().path()` instead of `MatchedPath`, or falling back to the
/// raw URI when no route matched. The non-vacuity assertions at the end prove the traffic
/// really was observed, so a scrape that simply forgot to record HTTP requests cannot pass
/// this test by having nothing to leak.
#[tokio::test]
async fn metrics_endpoint_never_exposes_raw_paths_or_identifiers() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    let path_id = Uuid::now_v7();
    let unmatched_marker = format!("moiracanary{}", Uuid::now_v7().simple());
    let query_canary = format!("qcanary{}", Uuid::now_v7().simple());
    let request_id_canary = format!("ridcanary{}", Uuid::now_v7().simple());

    // 1. A real UUID in a path parameter of a templated route. The label must be the route
    //    *template*; the resolved path would make the label set unbounded.
    let templated = get(
        &client,
        &format!("{}/api/v1/admin/applications/{path_id}", server.base_url),
        &[],
    )
    .await;
    assert_eq!(
        templated.status,
        StatusCode::NOT_FOUND,
        "expected a miss on a random application id: {}",
        templated.body
    );

    // 2. A path that matches no route at all. This is the fallback case, where "just use the
    //    URI" is the tempting shortcut and would hand a caller direct control of the label.
    let unmatched = get(
        &client,
        &format!("{}/{unmatched_marker}/probe", server.base_url),
        &[],
    )
    .await;
    assert_eq!(unmatched.status, StatusCode::NOT_FOUND);

    // 3. A query string and a correlation header on an otherwise ordinary request.
    let probed = get(
        &client,
        &format!("{}/health/live?probe={query_canary}", server.base_url),
        &[("x-request-id", request_id_canary.as_str())],
    )
    .await;
    assert_eq!(probed.status, StatusCode::OK, "{}", probed.body);

    let body = scrape(&client, &server).await;

    for (what, canary) in [
        ("path parameter", path_id.to_string()),
        ("unmatched path segment", unmatched_marker.clone()),
        ("query string value", query_canary.clone()),
        ("x-request-id header", request_id_canary.clone()),
    ] {
        assert!(
            !body.contains(&canary),
            "the {what} {canary:?} reached the /metrics body; caller-controlled input is now \
             a label value:\n{body}"
        );
    }

    // The label *keys* that would signal the same mistake even if the canary itself had been
    // truncated or escaped beyond recognition.
    for forbidden in ["path=", "uri=", "query=", "request_id=", "execution_id="] {
        assert!(
            !body.contains(forbidden),
            "label key {forbidden} must never exist on any Moira metric:\n{body}"
        );
    }

    // Non-vacuity: the three requests above really were observed, as bounded labels. Without
    // this, a `/metrics` body that recorded nothing would pass every assertion above.
    assert!(
        !series(
            &body,
            &format!("{HTTP_DURATION}_count"),
            &[
                ("service", service.as_str()),
                ("route", "/api/v1/admin/applications/{id}")
            ]
        )
        .is_empty(),
        "the templated request was not observed under its route template:\n{body}"
    );
    assert!(
        !series(
            &body,
            &format!("{HTTP_DURATION}_count"),
            &[("service", service.as_str()), ("route", "unmatched")]
        )
        .is_empty(),
        "the unmatched request was not observed under the constant fallback label:\n{body}"
    );
    assert_eq!(
        series_value(
            &body,
            &format!("{HTTP_DURATION}_count"),
            &[
                ("service", service.as_str()),
                ("route", "/health/live"),
                ("method", "GET"),
                ("status_class", "2xx")
            ]
        ),
        1.0,
        "the query-string request was not observed:\n{body}"
    );

    server.shutdown().await;
}

/// The disabled-path contract, which the exporter swap was not allowed to change.
///
/// `prometheus_enabled` is `false` by default, so this is the configuration most Moira
/// deployments start in — and the response it produces is a *keyed* error, not a bare 404,
/// because an operator staring at a 404 needs to be told the difference between "this build
/// has no metrics endpoint" and "you have not turned it on". The keyed contract is what a
/// console can localise; a plain 404 is not.
///
/// The gate is also a disclosure boundary: a disabled endpoint must return no exposition
/// text whatsoever, which is asserted here rather than assumed from the status code.
#[tokio::test]
async fn metrics_endpoint_returns_a_keyed_error_when_prometheus_is_disabled() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let server = start_server(&fixture, false).await;
    let client = Client::new();

    let request_id = format!("metrics-disabled-{}", Uuid::now_v7());
    // Deliberately unauthenticated: the disabled path must answer with its own coded error
    // rather than an authentication challenge, or an operator debugging a scrape is sent
    // looking for a credential problem that does not exist.
    let result = get(
        &client,
        &format!("{}/metrics", server.base_url),
        &[("x-request-id", request_id.as_str())],
    )
    .await;

    assert_eq!(
        result.status,
        StatusCode::NOT_FOUND,
        "body: {}",
        result.body
    );

    let value: Value = serde_json::from_str(&result.body).unwrap_or_else(|error| {
        panic!("expected a JSON error envelope ({error}): {}", result.body)
    });
    let error = &value["error"];
    assert_eq!(error["code"], "metrics_disabled");
    assert_eq!(error["message_key"], "moira.error.metrics_disabled");
    // Compared against the catalog rather than only against the literal above: a key that is
    // spelled consistently but missing from the catalog renders as nothing in a console, and
    // a literal-only assertion would happily agree with the typo.
    assert!(
        moira::i18n::is_known_key(
            error["message_key"]
                .as_str()
                .expect("message_key is a string")
        ),
        "the disabled-metrics key is not in the response catalog: {}",
        result.body
    );
    assert!(
        !error["message"]
            .as_str()
            .expect("message is a string")
            .trim()
            .is_empty()
    );
    assert_eq!(error["request_id"], request_id);
    assert!(error["message_args"].is_object());

    // No exposition may leak through the gate — not a family name, not a HELP line.
    assert!(
        !result.body.contains("# TYPE") && !result.body.contains("moira_http_requests_total"),
        "a disabled /metrics still returned exposition text: {}",
        result.body
    );

    server.shutdown().await;
}

/// The endpoint's authentication posture, pinned in both directions.
///
/// `/metrics` is unauthenticated on purpose: a Prometheus scraper is a piece of
/// infrastructure that generally cannot present a Moira credential, and the endpoint is
/// expected to be reachable only on an internal network. That posture must not drift
/// *either* way. Adding authentication silently breaks every scrape in every deployment.
/// The opposite drift — the endpoint quietly becoming authenticated-looking by returning
/// 401s — is equally a break.
///
/// The control case is what makes this test mean something: the same bogus credential that
/// `/metrics` ignores must produce a `401` on a route that does authenticate. Without it
/// this test would pass just as happily against a server with authentication disabled
/// entirely, and would therefore be pinning the test fixture rather than the endpoint.
#[tokio::test]
async fn metrics_endpoint_is_reachable_without_authentication() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let server = start_server(&fixture, true).await;
    let client = Client::new();

    // No credential of any kind.
    let anonymous = get(&client, &format!("{}/metrics", server.base_url), &[]).await;
    assert_eq!(
        anonymous.status,
        StatusCode::OK,
        "/metrics must be scrapeable without a credential: {}",
        anonymous.body
    );
    assert!(
        anonymous
            .body
            .contains("# TYPE moira_http_requests_total counter"),
        "an anonymous scrape returned no exposition: {}",
        anonymous.body
    );
    assert!(
        anonymous.headers.get(header::WWW_AUTHENTICATE).is_none(),
        "/metrics must not issue an authentication challenge"
    );

    // A credential that is rejected everywhere else is simply ignored here, because the
    // handler runs no authentication extractor at all.
    let bogus_key = format!("moira_sys_metricscanary{}", Uuid::now_v7().simple());
    let with_bogus_key = get(
        &client,
        &format!("{}/metrics", server.base_url),
        &[("x-moira-system-key", bogus_key.as_str())],
    )
    .await;
    assert_eq!(
        with_bogus_key.status,
        StatusCode::OK,
        "an invalid credential must not turn a public scrape into a rejection: {}",
        with_bogus_key.body
    );

    // Control: the same key on a route that does authenticate.
    let protected = get(
        &client,
        &format!("{}/api/v1/admin/applications?limit=1", server.base_url),
        &[("x-moira-system-key", bogus_key.as_str())],
    )
    .await;
    assert_eq!(
        protected.status,
        StatusCode::UNAUTHORIZED,
        "the control route accepted a bogus system key, so the assertions above prove \
         nothing about /metrics: {}",
        protected.body
    );

    // The rejected key must not have been recorded as a label either — a 401 is exactly the
    // request whose details are most tempting to record.
    let body = scrape(&client, &server).await;
    assert!(
        !body.contains(&bogus_key),
        "a rejected credential reached the metrics body:\n{body}"
    );

    server.shutdown().await;
}

/// Guards the one assumption the whole file rests on: that a scrape observes traffic driven
/// through a *different* clone of the same `AppState`.
///
/// Every test above starts its server on `fixture.state.clone()` with only `settings`
/// replaced, and several then drive work through `fixture.execution_service()` — a
/// completely separate service object. If `MetricsRegistry` ever stopped being shared behind
/// an `Arc` (a plausible outcome of a future refactor toward per-request recorders), those
/// tests would not fail loudly; they would observe an empty registry and every count would
/// quietly become zero. This asserts the sharing directly, so that refactor breaks here with
/// an obvious message instead of hollowing out the suite.
#[tokio::test]
async fn cloned_app_states_scrape_one_shared_registry() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let service = service_label(&fixture);
    let recording_server = start_server(&fixture, true).await;
    let scraping_server = start_server(&fixture, true).await;
    let client = Client::new();

    assert_ne!(
        recording_server.base_url, scraping_server.base_url,
        "the two servers must be distinct processes' worth of routing"
    );

    let probes = 2;
    for _ in 0..probes {
        let result = get(
            &client,
            &format!("{}/health/live", recording_server.base_url),
            &[],
        )
        .await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    }

    let body = scrape(&client, &scraping_server).await;
    assert_eq!(
        series_value(
            &body,
            &format!("{HTTP_DURATION}_count"),
            &[
                ("service", service.as_str()),
                ("route", "/health/live"),
                ("method", "GET"),
                ("status_class", "2xx")
            ]
        ),
        f64::from(probes),
        "traffic served by one AppState clone was invisible to another's scrape, so every \
         count in this suite is measuring the wrong registry:\n{body}"
    );

    scraping_server.shutdown().await;
    recording_server.shutdown().await;
}
