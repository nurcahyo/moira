# Grafana

The dashboard is committed at
[`deploy/observability/grafana-moira-overview.json`](../deploy/observability/grafana-moira-overview.json)
(UID `moira-overview`). Alerting rules sit next to it in
[`deploy/observability/prometheus-rules.yaml`](../deploy/observability/prometheus-rules.yaml).
Loading instructions, the alert inventory and the routing convention are in
[`deploy/observability/README.md`](../deploy/observability/README.md).

## What it covers

Rows, in order:

- **Service health** — scrape health, request rate, 5xx ratio, HTTP p95, provider
  failure ratio, and the Redis/workers feature gauges.
- **HTTP traffic and latency** — responses by status class, latency quantiles, top
  routes by rate and by p95.
- **Provider execution** — outcomes by class, failure ratio by provider type,
  execution latency, time to first token, attempts by model key.
- **Database, Redis and runtime config** — pool occupancy, Redis operation failures,
  runtime-config invalidations by channel, retention sweeps.
- **Background workers** — queue throughput, failures and dead letters by job name,
  enqueue rejections, leader-lock ownership.
- **RAG, retrieval, memory and summarization** — ingestion volume, retrieval and
  embedding latency, and the run outcomes of the three subsystems that fail silently.
- **Admin identity** — invitation outcomes and grant lifecycle events.

## The constraint the dashboard is built under

Every panel queries a metric family that `src/infra/metrics.rs` actually declares —
the full list is in [prometheus.md](./prometheus.md). The single exception is `up`,
which Prometheus synthesises per scrape target; no counter emitted by a process can
express that the process stopped answering.

That constraint rules some things out. There is no token-usage panel, no queue-depth
panel, no Redis-latency panel and no SQL-timing panel, because no such metric exists
today. Adding one is a change to `src/infra/metrics.rs`, not to the dashboard.

## Prerequisites

`/metrics` is off by default. Set `MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true`, and on
Kubernetes set `serviceMonitor.enabled=true` in `charts/moira` (also off by default)
so the pods are scraped at all.
