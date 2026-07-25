use std::{
    fmt::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::http::StatusCode;

#[derive(Debug, Clone, Default)]
pub struct MetricsRegistry {
    inner: Arc<MetricsInner>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    http_requests_total: AtomicU64,
    http_responses_2xx_total: AtomicU64,
    http_responses_3xx_total: AtomicU64,
    http_responses_4xx_total: AtomicU64,
    http_responses_5xx_total: AtomicU64,
    http_latency_micros_total: AtomicU64,
    public_responses_created_total: AtomicU64,
    public_streams_started_total: AtomicU64,
    worker_ticks_total: AtomicU64,
    retention_runs_total: AtomicU64,
    retention_idempotency_records_deleted_total: AtomicU64,
    retention_responses_deleted_total: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub http_requests_total: u64,
    pub http_responses_2xx_total: u64,
    pub http_responses_3xx_total: u64,
    pub http_responses_4xx_total: u64,
    pub http_responses_5xx_total: u64,
    pub http_latency_micros_total: u64,
    pub public_responses_created_total: u64,
    pub public_streams_started_total: u64,
    pub worker_ticks_total: u64,
    pub retention_runs_total: u64,
    pub retention_idempotency_records_deleted_total: u64,
    pub retention_responses_deleted_total: u64,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_http_response(&self, status: StatusCode, latency: Duration) {
        self.inner
            .http_requests_total
            .fetch_add(1, Ordering::Relaxed);
        self.inner.http_latency_micros_total.fetch_add(
            latency.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );

        if status.is_success() {
            self.inner
                .http_responses_2xx_total
                .fetch_add(1, Ordering::Relaxed);
        } else if status.is_redirection() {
            self.inner
                .http_responses_3xx_total
                .fetch_add(1, Ordering::Relaxed);
        } else if status.is_client_error() {
            self.inner
                .http_responses_4xx_total
                .fetch_add(1, Ordering::Relaxed);
        } else if status.is_server_error() {
            self.inner
                .http_responses_5xx_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_public_response_created(&self) {
        self.inner
            .public_responses_created_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_public_stream_started(&self) {
        self.inner
            .public_streams_started_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_worker_tick(&self) {
        self.inner
            .worker_ticks_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one completed retention sweep, successful or not — the sweep-rate
    /// signal is what tells an operator the worker is alive at all.
    pub fn record_retention_run(&self) {
        self.inner
            .retention_runs_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Counts rows deleted by the retention worker, per table.
    ///
    /// Deliberately a plain counter keyed on a closed set of table names, not a
    /// general labelled metric: plan 05 replaces the whole registry with the
    /// `metrics` facade and can upgrade this into a labelled counter/histogram
    /// without changing this call site's shape. An unrecognised table is dropped
    /// rather than silently folded into another table's total.
    pub fn record_retention_deleted(&self, table: &str, count: u64) {
        let counter = match table {
            crate::infra::workers::retention::TABLE_IDEMPOTENCY_RECORDS => {
                &self.inner.retention_idempotency_records_deleted_total
            }
            crate::infra::workers::retention::TABLE_RESPONSES => {
                &self.inner.retention_responses_deleted_total
            }
            _ => return,
        };
        counter.fetch_add(count, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            http_requests_total: self.inner.http_requests_total.load(Ordering::Relaxed),
            http_responses_2xx_total: self.inner.http_responses_2xx_total.load(Ordering::Relaxed),
            http_responses_3xx_total: self.inner.http_responses_3xx_total.load(Ordering::Relaxed),
            http_responses_4xx_total: self.inner.http_responses_4xx_total.load(Ordering::Relaxed),
            http_responses_5xx_total: self.inner.http_responses_5xx_total.load(Ordering::Relaxed),
            http_latency_micros_total: self.inner.http_latency_micros_total.load(Ordering::Relaxed),
            public_responses_created_total: self
                .inner
                .public_responses_created_total
                .load(Ordering::Relaxed),
            public_streams_started_total: self
                .inner
                .public_streams_started_total
                .load(Ordering::Relaxed),
            worker_ticks_total: self.inner.worker_ticks_total.load(Ordering::Relaxed),
            retention_runs_total: self.inner.retention_runs_total.load(Ordering::Relaxed),
            retention_idempotency_records_deleted_total: self
                .inner
                .retention_idempotency_records_deleted_total
                .load(Ordering::Relaxed),
            retention_responses_deleted_total: self
                .inner
                .retention_responses_deleted_total
                .load(Ordering::Relaxed),
        }
    }

    pub fn render_prometheus(
        &self,
        service_name: &str,
        redis_enabled: bool,
        workers_enabled: bool,
    ) -> String {
        let snapshot = self.snapshot();
        let service_name = sanitize_label_value(service_name);
        let mut output = String::new();
        write_metric_header(
            &mut output,
            "moira_http_requests_total",
            "Total HTTP requests observed by Moira.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_http_requests_total",
            &service_name,
            snapshot.http_requests_total,
        );
        write_metric_header(
            &mut output,
            "moira_http_response_status_class_total",
            "Total HTTP responses by low-cardinality status class.",
            "counter",
        );
        write_status_metric(
            &mut output,
            &service_name,
            "2xx",
            snapshot.http_responses_2xx_total,
        );
        write_status_metric(
            &mut output,
            &service_name,
            "3xx",
            snapshot.http_responses_3xx_total,
        );
        write_status_metric(
            &mut output,
            &service_name,
            "4xx",
            snapshot.http_responses_4xx_total,
        );
        write_status_metric(
            &mut output,
            &service_name,
            "5xx",
            snapshot.http_responses_5xx_total,
        );
        write_metric_header(
            &mut output,
            "moira_http_latency_micros_total",
            "Cumulative HTTP response latency in microseconds.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_http_latency_micros_total",
            &service_name,
            snapshot.http_latency_micros_total,
        );
        write_metric_header(
            &mut output,
            "moira_public_responses_created_total",
            "Total public non-streaming responses created.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_public_responses_created_total",
            &service_name,
            snapshot.public_responses_created_total,
        );
        write_metric_header(
            &mut output,
            "moira_public_streams_started_total",
            "Total public response streams started.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_public_streams_started_total",
            &service_name,
            snapshot.public_streams_started_total,
        );
        write_metric_header(
            &mut output,
            "moira_worker_ticks_total",
            "Total background worker maintenance ticks.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_worker_ticks_total",
            &service_name,
            snapshot.worker_ticks_total,
        );
        write_metric_header(
            &mut output,
            "moira_retention_runs_total",
            "Total retention cleanup sweeps completed by this process.",
            "counter",
        );
        write_labeled_metric(
            &mut output,
            "moira_retention_runs_total",
            &service_name,
            snapshot.retention_runs_total,
        );
        write_metric_header(
            &mut output,
            "moira_retention_rows_deleted_total",
            "Total expired rows deleted by the retention cleanup worker, by table.",
            "counter",
        );
        write_retention_metric(
            &mut output,
            &service_name,
            crate::infra::workers::retention::TABLE_IDEMPOTENCY_RECORDS,
            snapshot.retention_idempotency_records_deleted_total,
        );
        write_retention_metric(
            &mut output,
            &service_name,
            crate::infra::workers::retention::TABLE_RESPONSES,
            snapshot.retention_responses_deleted_total,
        );
        write_metric_header(
            &mut output,
            "moira_redis_enabled",
            "Whether Redis coordination is enabled for this process.",
            "gauge",
        );
        write_labeled_metric(
            &mut output,
            "moira_redis_enabled",
            &service_name,
            u64::from(redis_enabled),
        );
        write_metric_header(
            &mut output,
            "moira_workers_enabled",
            "Whether background workers are enabled for this process.",
            "gauge",
        );
        write_labeled_metric(
            &mut output,
            "moira_workers_enabled",
            &service_name,
            u64::from(workers_enabled),
        );
        output
    }
}

fn write_metric_header(output: &mut String, name: &str, help: &str, metric_type: &str) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} {metric_type}");
}

fn write_labeled_metric(output: &mut String, name: &str, service_name: &str, value: u64) {
    let _ = writeln!(output, "{name}{{service=\"{service_name}\"}} {value}");
}

fn write_status_metric(output: &mut String, service_name: &str, status_class: &str, value: u64) {
    let _ = writeln!(
        output,
        "moira_http_response_status_class_total{{service=\"{service_name}\",status_class=\"{status_class}\"}} {value}"
    );
}

/// `table` is always one of the two `&'static str` constants in
/// `crate::infra::workers::retention`, so the label set stays at cardinality 2.
fn write_retention_metric(output: &mut String, service_name: &str, table: &str, value: u64) {
    let _ = writeln!(
        output,
        "moira_retention_rows_deleted_total{{service=\"{service_name}\",table=\"{table}\"}} {value}"
    );
}

fn sanitize_label_value(value: &str) -> String {
    value
        .chars()
        .filter_map(|ch| match ch {
            '\\' => Some("\\\\".to_string()),
            '"' => Some("\\\"".to_string()),
            '\n' | '\r' => None,
            _ => Some(ch.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_export_uses_low_cardinality_labels() {
        let metrics = MetricsRegistry::new();
        metrics.record_http_response(StatusCode::OK, Duration::from_micros(15));
        metrics.record_http_response(StatusCode::NOT_FOUND, Duration::from_micros(7));
        metrics.record_public_response_created();

        let rendered = metrics.render_prometheus("moira-test", true, false);
        assert!(rendered.contains("moira_http_requests_total{service=\"moira-test\"} 2"));
        assert!(rendered.contains("status_class=\"2xx\""));
        assert!(rendered.contains("status_class=\"4xx\""));
        assert!(!rendered.contains("path="));
    }
}
