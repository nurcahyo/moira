# Prometheus

Prometheus metrics are disabled by default:

```text
MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true
```

When enabled, scrape:

```text
GET /metrics
```

Current metrics are intentionally low-cardinality:

- HTTP request count
- HTTP response status-class count
- cumulative HTTP latency
- public response count
- public stream count
- worker supervisor ticks
- Redis enabled gauge
- worker enabled gauge

Future metrics must avoid labels such as raw path parameters, user IDs, prompts, response IDs, execution IDs, API-key prefixes, or tenant-specific free-form values.
