//! Telemetry composition root.
//!
//! Owns the process-wide `tracing` subscriber stack and, when
//! `telemetry.otel_enabled` is set, the OpenTelemetry SDK + OTLP export pipeline
//! that bridges those same `tracing` spans to a collector.
//!
//! # Why `init` returns a guard
//!
//! Spans are handed to a [`BatchSpanProcessor`], which buffers them on a
//! dedicated worker thread and exports on a timer. Nothing flushes that buffer
//! on a clean process exit unless [`SdkTracerProvider::shutdown`] is called: the
//! provider's own `Drop` only shuts down when the *last* handle drops, and the
//! `tracing` layer installed into the global subscriber holds a clone for the
//! lifetime of the process. So without an explicit shutdown the final batch is
//! silently discarded on every graceful shutdown — which presents as "OTel does
//! not work" and is miserable to diagnose. [`init`] therefore returns a
//! [`TelemetryGuard`] that the caller must hold and [`TelemetryGuard::shutdown`]
//! before exiting.
//!
//! # Security contract
//!
//! Exported spans leave the process and cross a trust boundary. This module
//! therefore:
//!
//! * adds **no** span attributes of its own beyond the OTel resource, whose only
//!   Moira-supplied value is `service.name` (`telemetry.service_name`);
//! * never places request or response bodies, prompt text, credential material,
//!   or raw secrets on a span — it only bridges spans other modules create;
//! * keeps the configured OTLP endpoint out of every error string it produces.
//!   An endpoint may legitimately carry an auth token in userinfo or a query
//!   parameter, and `opentelemetry_otlp`'s own `InvalidUri` error echoes the URI
//!   verbatim, so exporter build failures are scrubbed through
//!   [`redact_endpoint`] down to `scheme://host:port` before they reach a log.
//!
//! # Known gap: span coverage under the shipped `env_filter`
//!
//! `tracing-opentelemetry` bridges every span the subscriber records, so no
//! redundant instrumentation is added here. The two spans constructed in `src/`
//! are `redacted_request_span` (`http_request`, in `src/lib.rs`) and
//! `execution_attempt` (in `src/application/execution.rs`); both are
//! `debug_span!` on target `moira`. The shipped default `env_filter` is
//! `moira=info,tower_http=info`, which disables `DEBUG`, so a stock deployment
//! that merely flips `MOIRA_TELEMETRY__OTEL_ENABLED=true` exports an empty trace
//! stream. Enabling OTel today therefore also requires widening the filter, e.g.
//! `MOIRA_TELEMETRY__ENV_FILTER=moira=debug,tower_http=debug`. Promoting those
//! spans to `info_span!` (owned by their own modules) or adding a dedicated OTel
//! filter setting would remove the footgun; both are out of scope for this
//! module.

use std::{fmt::Display, time::Duration};

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    Resource,
    trace::{BatchSpanProcessor, SdkTracerProvider, SpanProcessor},
};
use tracing_subscriber::{EnvFilter, filter::filter_fn, fmt, prelude::*};
use url::Url;

use crate::{config::TelemetrySettings, error::AppError};

/// Instrumentation scope name reported on every exported span.
const INSTRUMENTATION_SCOPE: &str = "moira";

/// OTLP/HTTP path for the traces signal, per the OTLP specification's endpoint
/// URL rules.
const OTLP_TRACES_PATH: &str = "/v1/traces";

/// Upper bound on the flush performed at shutdown. An unreachable or wedged
/// collector must delay process exit by at most this long, never block it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Placeholder substituted for an OTLP endpoint that cannot be reduced to a
/// safe `scheme://host:port` form.
const REDACTED_ENDPOINT: &str = "<redacted otlp endpoint>";

/// Holds the OpenTelemetry tracer provider created by [`init`], if any.
///
/// Must be kept alive for the lifetime of the process and shut down before exit;
/// see the module docs for why dropping it is not sufficient.
#[must_use = "the telemetry guard must be held and shut down, or buffered spans are lost on exit"]
#[derive(Debug)]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

/// Outcome of [`TelemetryGuard::shutdown`].
///
/// A failed flush is reported rather than returned as an error: losing trailing
/// spans must never turn a successful run into a failed process exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryShutdown {
    /// OpenTelemetry export was disabled; there was nothing to flush.
    NotEnabled,
    /// The tracer provider was shut down and pending spans were flushed.
    Flushed,
    /// The provider was shut down but reported a problem while flushing.
    Failed(String),
}

impl TelemetryGuard {
    /// A guard for a process with OpenTelemetry export disabled.
    fn disabled() -> Self {
        Self { provider: None }
    }

    /// Whether an OpenTelemetry pipeline was constructed.
    pub fn otel_enabled(&self) -> bool {
        self.provider.is_some()
    }

    /// Flushes and shuts down the OpenTelemetry pipeline.
    ///
    /// Blocks for at most [`SHUTDOWN_TIMEOUT`]. The batch processor drains on a
    /// dedicated OS thread with no Tokio reactor attached, so callers inside an
    /// async runtime should invoke this from a blocking context.
    pub fn shutdown(self) -> TelemetryShutdown {
        match self.provider {
            None => TelemetryShutdown::NotEnabled,
            Some(provider) => match provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT) {
                Ok(()) => TelemetryShutdown::Flushed,
                Err(err) => TelemetryShutdown::Failed(err.to_string()),
            },
        }
    }
}

/// Installs the process-wide `tracing` subscriber and, when enabled, the OTLP
/// export pipeline.
///
/// Returns a [`TelemetryGuard`] that must be shut down before process exit.
///
/// # Errors
///
/// Returns [`AppError::Config`] (public code `configuration_error`,
/// `moira.error.configuration_error`) when the filter directive is invalid, when
/// `otel_enabled` is set without a usable `otel_endpoint`, when the endpoint is
/// not a usable http(s) URL, when the OTLP exporter cannot be built, or when a
/// global subscriber is already installed.
pub fn init(settings: &TelemetrySettings) -> Result<TelemetryGuard, AppError> {
    let filter = EnvFilter::try_new(&settings.env_filter)
        .or_else(|_| EnvFilter::try_from_default_env())
        .map_err(|err| AppError::Config(format!("invalid telemetry filter: {err}")))?;

    // Resolved before the subscriber is installed so a misconfiguration fails
    // fast instead of leaving the process running with a silently dead pipeline.
    let provider = match resolve_otlp_endpoint(settings)? {
        Some(endpoint) => Some(build_otlp_tracer_provider(
            &settings.service_name,
            &endpoint,
        )?),
        None => None,
    };

    // `Option<L>` is itself a `Layer`, and its `max_level_hint` for `None` is
    // `LevelFilter::OFF`, so the disabled arms cost nothing and cannot widen the
    // subscriber's static max level. This keeps the `otel_enabled = false` path
    // behaviourally identical to the pre-OTel implementation.
    let otel_layer = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer(INSTRUMENTATION_SCOPE))
            // The SDK's `internal-logs` feature reports exporter progress and
            // failures through `tracing`. Feeding those back into the exporter
            // would let one failed export generate the events that trigger the
            // next one.
            .with_filter(filter_fn(|metadata| {
                !metadata.target().starts_with("opentelemetry")
            }))
    });

    let (json_layer, text_layer) = if settings.json {
        (Some(fmt::layer().json()), None)
    } else {
        (None, Some(fmt::layer()))
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(filter_fn(suppresses_provider_payload_logs))
        .with(json_layer)
        .with(text_layer)
        .with(otel_layer)
        .try_init()
        .map_err(|err| AppError::Config(format!("initialize tracing: {err}")))?;

    Ok(match provider {
        Some(provider) => TelemetryGuard {
            provider: Some(provider),
        },
        None => TelemetryGuard::disabled(),
    })
}

/// Targets whose verbose events carry provider request or response payloads.
///
/// `rig-core` 0.40 emits the **entire** completion request body — every message, verbatim — on
/// the `rig::completions` target. That was already an exposure for caller-supplied prompts;
/// plan 11 made it worse, because the assembled context now also contains retrieved RAG chunk
/// and memory text belonging to documents the caller never typed.
const PAYLOAD_BEARING_LOG_TARGET_PREFIXES: &[&str] = &["rig"];

/// Drops verbose events from upstream targets that log prompt payloads.
///
/// **This is a hard suppression, not a default.** It sits below the `EnvFilter`, so it holds
/// however the operator sets `env_filter` or `RUST_LOG` — an operator who wants
/// `moira=trace` to debug routing must not have to accept every prompt and every retrieved
/// document chunk in the log stream as the price.
///
/// `INFO` and above still pass, so upstream warnings and errors are never hidden: the events
/// being dropped are exactly the ones whose *content* is the payload.
///
/// **Reversal condition:** remove this the moment `rig-core` gains a way to disable or redact
/// its request-body logging at the source, which is where it belongs. Until then a filter here
/// is the only place Moira controls.
fn suppresses_provider_payload_logs(metadata: &tracing::Metadata<'_>) -> bool {
    let verbose = *metadata.level() > tracing::Level::INFO;
    let payload_bearing = PAYLOAD_BEARING_LOG_TARGET_PREFIXES.iter().any(|prefix| {
        metadata.target() == *prefix || metadata.target().starts_with(&format!("{prefix}::"))
    });
    !(verbose && payload_bearing)
}

/// Resolves the OTLP traces endpoint, or `None` when export is disabled.
///
/// Fails fast rather than no-opping: a telemetry pipeline that is switched on
/// but silently exports nowhere is worse than one that refuses to start.
fn resolve_otlp_endpoint(settings: &TelemetrySettings) -> Result<Option<String>, AppError> {
    if !settings.otel_enabled {
        return Ok(None);
    }

    let configured = settings
        .otel_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::Config(
                "telemetry.otel_enabled is true but telemetry.otel_endpoint is not set; set \
                 MOIRA_TELEMETRY__OTEL_ENDPOINT to the OTLP/HTTP collector base URL (for example \
                 http://otel-collector:4318) or set MOIRA_TELEMETRY__OTEL_ENABLED=false"
                    .to_string(),
            )
        })?;

    normalize_otlp_traces_endpoint(configured).map(Some)
}

/// Normalises a configured collector URL to the traces-signal URL the exporter
/// will POST to.
///
/// `opentelemetry-otlp` uses a programmatically supplied endpoint **verbatim**
/// and appends `/v1/traces` only to values taken from `OTEL_EXPORTER_OTLP_ENDPOINT`.
/// Moira supplies the endpoint programmatically, so the natural operator value
/// `http://collector:4318` would POST to `/` and be rejected by every collector —
/// another silent-failure mode. A root-path endpoint is therefore treated as a
/// base URL and gets [`OTLP_TRACES_PATH`] appended; an endpoint that already
/// carries a path is assumed to be a fully specified signal endpoint and is left
/// alone.
fn normalize_otlp_traces_endpoint(endpoint: &str) -> Result<String, AppError> {
    // `url::ParseError`'s `Display` describes the failure without echoing the
    // input, so it is safe to surface directly.
    let mut url = Url::parse(endpoint).map_err(|err| {
        AppError::Config(format!("telemetry.otel_endpoint is not a valid URL: {err}"))
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Config(format!(
            "telemetry.otel_endpoint must use the http or https scheme, got {}",
            url.scheme()
        )));
    }

    if url.path().is_empty() || url.path() == "/" {
        url.set_path(OTLP_TRACES_PATH);
    }

    Ok(url.to_string())
}

/// Builds the OTLP/HTTP exporter and wraps it in a batching tracer provider.
fn build_otlp_tracer_provider(
    service_name: &str,
    endpoint: &str,
) -> Result<SdkTracerProvider, AppError> {
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|err| {
            AppError::Config(format!(
                "build OTLP span exporter for {} failed: {}",
                redact_endpoint(endpoint),
                scrub_endpoint(endpoint, err)
            ))
        })?;

    Ok(tracer_provider_with_processor(
        service_name,
        BatchSpanProcessor::builder(exporter).build(),
    ))
}

/// Assembles a tracer provider around an arbitrary span processor.
///
/// Split out from [`build_otlp_tracer_provider`] so tests can substitute a
/// processor that needs no network.
fn tracer_provider_with_processor<P>(service_name: &str, processor: P) -> SdkTracerProvider
where
    P: SpanProcessor + 'static,
{
    SdkTracerProvider::builder()
        .with_span_processor(processor)
        // `service.name` is the only Moira-supplied resource attribute. Nothing
        // request-scoped, caller-supplied, or secret belongs here.
        .with_resource(
            Resource::builder()
                .with_service_name(service_name.to_string())
                .build(),
        )
        .build()
}

/// Reduces an OTLP endpoint to `scheme://host[:port]`, dropping any userinfo,
/// path, query, and fragment that could carry an auth token.
fn redact_endpoint(endpoint: &str) -> String {
    let Ok(url) = Url::parse(endpoint) else {
        return REDACTED_ENDPOINT.to_string();
    };
    let Some(host) = url.host_str() else {
        return REDACTED_ENDPOINT.to_string();
    };
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Renders a third-party error, replacing any verbatim copy of the endpoint with
/// its redacted form.
///
/// `ExporterBuildError::InvalidUri` embeds the URI it was given, so the raw
/// endpoint can otherwise reach a log line through an error message alone.
fn scrub_endpoint(endpoint: &str, err: impl Display) -> String {
    err.to_string()
        .replace(endpoint, &redact_endpoint(endpoint))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use opentelemetry_sdk::{
        error::OTelSdkResult,
        trace::{SpanData, SpanExporter as SpanExporterTrait},
    };

    use super::*;

    fn disabled_settings() -> TelemetrySettings {
        TelemetrySettings::default()
    }

    /// `filter_fn` receives a `Metadata`, which cannot be constructed outside `tracing`'s own
    /// macros, so the decision is expressed as a pure function of `(target, level)` and tested
    /// through that. The wiring above passes `metadata.target()` and `metadata.level()`
    /// straight through, so this is the whole of the logic.
    fn suppresses(target: &str, level: tracing::Level) -> bool {
        let verbose = level > tracing::Level::INFO;
        let payload_bearing = PAYLOAD_BEARING_LOG_TARGET_PREFIXES
            .iter()
            .any(|prefix| target == *prefix || target.starts_with(&format!("{prefix}::")));
        verbose && payload_bearing
    }

    /// `rig-core` logs the entire completion request body, which after plan 11 contains
    /// retrieved RAG and memory text. That must not be reachable by turning up a log level.
    #[test]
    fn verbose_rig_events_are_suppressed_however_the_operator_sets_the_filter() {
        for level in [tracing::Level::TRACE, tracing::Level::DEBUG] {
            assert!(suppresses("rig", level));
            assert!(suppresses("rig::completions", level));
            assert!(suppresses("rig::streaming", level));
        }
    }

    #[test]
    fn rig_warnings_and_errors_still_reach_the_operator() {
        for level in [
            tracing::Level::INFO,
            tracing::Level::WARN,
            tracing::Level::ERROR,
        ] {
            assert!(
                !suppresses("rig::completions", level),
                "suppressing a {level} event would hide a real upstream failure"
            );
        }
    }

    /// The prefix match is on a target *segment*, not a substring: a Moira module whose name
    /// merely begins with the same letters must keep its verbose logging.
    #[test]
    fn the_suppression_does_not_catch_unrelated_targets_by_prefix() {
        for target in [
            "rigging",
            "moira::orchestration",
            "rigid::thing",
            "sqlx::query",
        ] {
            assert!(
                !suppresses(target, tracing::Level::TRACE),
                "{target} must not be caught by the rig suppression"
            );
        }
    }

    fn enabled_settings(endpoint: Option<&str>) -> TelemetrySettings {
        TelemetrySettings {
            otel_enabled: true,
            otel_endpoint: endpoint.map(str::to_string),
            ..TelemetrySettings::default()
        }
    }

    /// Collects exported spans in memory. `opentelemetry_sdk`'s own
    /// `InMemorySpanExporter` sits behind the `testing` feature, which this crate
    /// does not enable, so the batch processor is fed this instead.
    #[derive(Debug, Clone, Default)]
    struct RecordingExporter {
        spans: Arc<Mutex<Vec<SpanData>>>,
    }

    impl RecordingExporter {
        fn names(&self) -> Vec<String> {
            self.spans
                .lock()
                .expect("recording exporter mutex")
                .iter()
                .map(|span| span.name.to_string())
                .collect()
        }
    }

    impl SpanExporterTrait for RecordingExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.spans
                .lock()
                .expect("recording exporter mutex")
                .extend(batch);
            Ok(())
        }
    }

    #[test]
    fn otel_layer_is_not_constructed_when_otel_is_disabled() {
        let settings = disabled_settings();
        assert!(
            !settings.otel_enabled,
            "default config must not enable OTel"
        );

        // No endpoint is resolved, so no exporter, no batch processor, no
        // provider, and therefore no OTel layer is added to the registry.
        assert_eq!(
            resolve_otlp_endpoint(&settings).expect("disabled telemetry must resolve cleanly"),
            None
        );

        let guard = TelemetryGuard::disabled();
        assert!(!guard.otel_enabled());
        assert_eq!(guard.shutdown(), TelemetryShutdown::NotEnabled);
    }

    #[test]
    fn otel_disabled_ignores_a_configured_endpoint() {
        // Leaving a stale endpoint in config must not switch export on.
        let settings = TelemetrySettings {
            otel_endpoint: Some("http://otel-collector:4318".to_string()),
            ..TelemetrySettings::default()
        };

        assert_eq!(resolve_otlp_endpoint(&settings).expect("resolve"), None);
    }

    #[test]
    fn otel_enabled_without_an_endpoint_is_a_configuration_error() {
        for settings in [
            enabled_settings(None),
            enabled_settings(Some("")),
            enabled_settings(Some("   ")),
        ] {
            let err = resolve_otlp_endpoint(&settings)
                .expect_err("otel_enabled without an endpoint must fail fast");

            assert!(
                matches!(err, AppError::Config(_)),
                "expected AppError::Config, got {err:?}"
            );

            // Ties this module to the `configuration_error` catalog entry: the
            // fail-fast path is a new emitter of that code.
            let response = err.error_response(None);
            assert_eq!(response.error.code, "configuration_error");
            assert_eq!(
                response.error.message_key,
                "moira.error.configuration_error"
            );
            assert!(!response.error.message.is_empty());
        }
    }

    #[test]
    fn fail_fast_message_names_the_env_vars_needed_to_resolve_it() {
        let err = resolve_otlp_endpoint(&enabled_settings(None)).expect_err("must fail");
        let AppError::Config(message) = err else {
            panic!("expected AppError::Config");
        };

        assert!(
            message.contains("MOIRA_TELEMETRY__OTEL_ENDPOINT"),
            "{message}"
        );
        assert!(
            message.contains("MOIRA_TELEMETRY__OTEL_ENABLED"),
            "{message}"
        );
    }

    #[test]
    fn base_endpoint_gets_the_traces_signal_path_appended() {
        // The exporter uses a programmatic endpoint verbatim, so without this a
        // stock `host:4318` value would POST to `/` and be rejected.
        for base in [
            "http://otel-collector:4318",
            "http://otel-collector:4318/",
            "https://otel.example.com",
        ] {
            let resolved = normalize_otlp_traces_endpoint(base).expect("normalize");
            assert!(
                resolved.ends_with(OTLP_TRACES_PATH),
                "{base} resolved to {resolved}"
            );
        }
    }

    #[test]
    fn explicit_signal_endpoint_path_is_preserved() {
        let resolved = normalize_otlp_traces_endpoint("http://otel-collector:4318/v1/traces")
            .expect("normalize");
        assert_eq!(resolved, "http://otel-collector:4318/v1/traces");

        // A gateway path is treated as deliberate and left alone rather than
        // having a second `/v1/traces` bolted on.
        let gateway =
            normalize_otlp_traces_endpoint("https://gateway.example.com/otlp").expect("normalize");
        assert_eq!(gateway, "https://gateway.example.com/otlp");
    }

    #[test]
    fn non_http_endpoints_are_rejected_as_configuration_errors() {
        for endpoint in ["grpc://collector:4317", "file:///tmp/spans", "not a url"] {
            let err =
                normalize_otlp_traces_endpoint(endpoint).expect_err("{endpoint} must be rejected");
            assert!(matches!(err, AppError::Config(_)), "{endpoint}: {err:?}");
        }
    }

    #[test]
    fn endpoint_errors_never_echo_credentials_from_the_endpoint() {
        let endpoint =
            "https://ingest-token:s3cr3t-token@otel.example.com:4318/v1/traces?key=abcd1234";

        let redacted = redact_endpoint(endpoint);
        assert_eq!(redacted, "https://otel.example.com:4318");
        for leaked in ["s3cr3t-token", "ingest-token", "abcd1234", "/v1/traces"] {
            assert!(!redacted.contains(leaked), "redaction leaked {leaked}");
        }

        // `ExporterBuildError::InvalidUri` embeds the URI verbatim; scrubbing is
        // what keeps it out of the log line.
        let scrubbed = scrub_endpoint(endpoint, format!("invalid URI {endpoint}. Reason bad port"));
        assert!(!scrubbed.contains("s3cr3t-token"), "{scrubbed}");
        assert!(!scrubbed.contains("abcd1234"), "{scrubbed}");
        assert!(
            scrubbed.contains("https://otel.example.com:4318"),
            "{scrubbed}"
        );
    }

    #[test]
    fn fail_fast_message_never_echoes_a_configured_value() {
        let settings = TelemetrySettings {
            otel_enabled: true,
            otel_endpoint: Some("   ".to_string()),
            service_name: "moira-canary-service".to_string(),
            ..TelemetrySettings::default()
        };

        let AppError::Config(message) =
            resolve_otlp_endpoint(&settings).expect_err("must fail fast")
        else {
            panic!("expected AppError::Config");
        };
        assert!(!message.contains("moira-canary-service"), "{message}");
    }

    #[test]
    fn guard_shutdown_flushes_spans_buffered_by_the_batch_processor() {
        let exporter = RecordingExporter::default();
        let provider = tracer_provider_with_processor(
            "moira-test",
            BatchSpanProcessor::builder(exporter.clone()).build(),
        );
        // Second handle onto the same shared inner state, used below to observe
        // that the guard really shut the provider down.
        let observer = provider.clone();

        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer(INSTRUMENTATION_SCOPE)),
        );

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("telemetry_guard_flush_probe");
            let _entered = span.enter();
        });

        // The batch processor buffers on a timer, so nothing has been exported
        // yet — this is exactly the state a clean shutdown must not discard.
        assert!(
            exporter.names().is_empty(),
            "batch processor exported before shutdown; the flush assertion below would be vacuous"
        );

        let guard = TelemetryGuard {
            provider: Some(provider),
        };
        assert!(guard.otel_enabled());
        assert_eq!(guard.shutdown(), TelemetryShutdown::Flushed);

        assert_eq!(
            exporter.names(),
            vec!["telemetry_guard_flush_probe".to_string()],
            "guard shutdown did not flush the buffered span"
        );

        // Proof the guard's shutdown reached the provider rather than merely
        // dropping a clone: the shared inner state is now marked shut down.
        assert!(
            observer.shutdown_with_timeout(SHUTDOWN_TIMEOUT).is_err(),
            "provider was not shut down by the guard"
        );
    }

    #[test]
    fn guard_shutdown_reports_failure_without_panicking() {
        let provider = tracer_provider_with_processor(
            "moira-test",
            BatchSpanProcessor::builder(RecordingExporter::default()).build(),
        );
        provider
            .shutdown_with_timeout(SHUTDOWN_TIMEOUT)
            .expect("first shutdown");

        let guard = TelemetryGuard {
            provider: Some(provider),
        };

        // A second shutdown reports rather than returns an error: losing trailing
        // spans must not turn a successful run into a failed process exit.
        assert!(matches!(guard.shutdown(), TelemetryShutdown::Failed(_)));
    }
}
