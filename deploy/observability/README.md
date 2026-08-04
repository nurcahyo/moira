# Moira observability assets

Three files, all of them offline artefacts you load into your own monitoring stack:

| File | What it is |
|---|---|
| `grafana-moira-overview.json` | Grafana dashboard, UID `moira-overview` |
| `prometheus-rules.yaml` | Prometheus alerting rules, three groups |
| `prometheus.example.yaml` | A minimal `prometheus.yml` that scrapes Moira and loads the rules |

## The rule these assets follow

**Every panel and every alert queries a metric family that `src/infra/metrics.rs`
actually declares.** A dashboard that charts a metric the service never emits is
worse than no dashboard: it reads as "healthy" forever. The full family list is in
[`docs/prometheus.md`](../../docs/prometheus.md).

There is exactly one exception, and it is deliberate: `up`, which Prometheus
synthesises per scrape target. No counter emitted *by* a process can express "the
process stopped answering", so `MoiraServiceDown` and the "Targets up" stat panel
use `up`. Both say so in their own description.

### What is deliberately not here

These have no metric behind them today, so nothing charts or alerts on them:

- **Token usage.** Moira records usage per execution in the database and exposes it
  on `/api/v1/usage`; there is no `moira_*_tokens_total` family.
- **Worker queue depth.** There is no depth gauge. The saturation signal is
  `moira_worker_queue_enqueue_rejected_total` — the queue refusing an enqueue
  because the pending cap was reached — and that is what the panel and the
  `MoiraWorkerQueueRejectingEnqueues` alert use.
- **Redis latency.** Only `moira_redis_operation_failures_total` exists, a counter
  by operation. No histogram.
- **SQL query timing.** Only the pool-occupancy gauge
  `moira_db_pool_connections{state="total"|"idle"}` exists.

Adding any of those is a change to `src/infra/metrics.rs`, not to this directory.

## Prerequisites

Moira exposes `/metrics` only when Prometheus support is switched on. It is **off by
default**, and when off the endpoint answers `404` — which Prometheus records as a
failed scrape, so `MoiraServiceDown` fires:

```text
MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true
```

The endpoint requires no authentication.

Every family carries a `service` label whose value is
`MOIRA_TELEMETRY__SERVICE_NAME` (default `moira`). The dashboard exposes it as a
template variable.

## Loading the dashboard

**Grafana UI:** Dashboards → New → Import → Upload JSON file →
`grafana-moira-overview.json` → pick your Prometheus datasource for the
`DS_PROMETHEUS` input.

**Provisioning file** (`/etc/grafana/provisioning/dashboards/moira.yaml`):

```yaml
apiVersion: 1
providers:
  - name: moira
    type: file
    options:
      path: /var/lib/grafana/dashboards/moira
```

…then drop `grafana-moira-overview.json` into `/var/lib/grafana/dashboards/moira/`.

**Grafana HTTP API:**

```bash
# GRAFANA_TOKEN comes from your own environment. Never commit one.
jq '{dashboard: ., overwrite: true}' grafana-moira-overview.json \
  | curl -sS -X POST "$GRAFANA_URL/api/dashboards/db" \
      -H "Authorization: Bearer $GRAFANA_TOKEN" \
      -H 'Content-Type: application/json' \
      --data-binary @-
```

The dashboard has two template variables besides the datasource — `job` (the scrape
job) and `service` (the `service` label). Both default to "All".

## Loading the alert rules

**Plain Prometheus:** reference the file from `rule_files:` — see
`prometheus.example.yaml` — and reload:

```bash
promtool check rules prometheus-rules.yaml
curl -sS -X POST http://prometheus:9090/-/reload
```

**Prometheus Operator:** the `groups:` list is the body of a `PrometheusRule`:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PrometheusRule
metadata:
  name: moira
  labels:
    release: kube-prometheus-stack   # must match your Prometheus ruleSelector
spec:
  groups: [] # paste the `groups:` list from prometheus-rules.yaml here
```

The chart in `charts/moira` already ships a `ServiceMonitor` template; enable it with
`serviceMonitor.enabled=true` (it is `false` by default) so the pods get scraped.

**Routing:** rules are labelled `severity: critical` (page now) or
`severity: warning` (ticket), plus `service: moira`. Route on those in your existing
Alertmanager config; no Alertmanager configuration is shipped here, because a
routing tree is deployment-specific and the receiver blocks are exactly where
webhook secrets end up.

### The job-name assumption

The rules match `job=~"moira.*"`. Only `MoiraServiceDown` depends on this (the rest
aggregate `by (job)` without matching on it). If your scrape job is named something
else, edit that one expression.

## Alert inventory

| Alert | Severity | Metric it reads |
|---|---|---|
| `MoiraServiceDown` | critical | `up` (Prometheus-synthesised) |
| `MoiraHighServerErrorRate` | critical | `moira_http_response_status_class_total` |
| `MoiraProviderFailureRateHigh` | critical | `moira_provider_outcome_total` |
| `MoiraProviderCircuitOpen` | critical | `moira_provider_outcome_total` |
| `MoiraProviderAuthFailing` | critical | `moira_provider_outcome_total` |
| `MoiraDbPoolNearExhaustion` | critical | `moira_db_pool_connections` |
| `MoiraWorkerQueueRejectingEnqueues` | warning | `moira_worker_queue_enqueue_rejected_total` |
| `MoiraHttpLatencyP95High` | warning | `moira_http_request_duration_seconds` |
| `MoiraWorkerJobsDeadLettered` | warning | `moira_worker_jobs_dead_letter_total` |
| `MoiraRedisOperationsFailing` | warning | `moira_redis_operation_failures_total` |
| `MoiraRetrievalFailing` | warning | `moira_retrieval_runs_total` |
| `MoiraMemoryExtractionFailing` | warning | `moira_memory_extraction_runs_total` |
| `MoiraSummarizationFailing` | warning | `moira_summarization_runs_total` |
| `MoiraSummariesStoringInlineReasoning` | warning | `moira_summarization_inline_reasoning_total` |

The last four cover subsystems that never surface a failure to the caller. Retrieval,
memory extraction and summarization all fail silently by design, so their counters are
the only signal outside the database.

## Two things that will bite you

**Ratio alerts and no traffic.** Every ratio uses `clamp_min(denominator, 1e-9)`
rather than a bare division, so an idle service yields ~0 instead of `NaN`. It does
not invent traffic: with no requests the numerator is 0 too.

**Histogram bucket ceilings.** `moira_http_request_duration_seconds` tops out at 10s
and `moira_execution_duration_seconds` at 120s. A quantile pinned at the top bucket
means "at least that", not "exactly that".

## Tracing

OpenTelemetry export is separate and off by default; see
[`docs/otel.md`](../../docs/otel.md). Note that Moira's own spans are `DEBUG`, so
`MOIRA_TELEMETRY__OTEL_ENABLED=true` alone produces an empty trace stream — the
`env_filter` has to be widened too. Nothing in this directory reads traces.
