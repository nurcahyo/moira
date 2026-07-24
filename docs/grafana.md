# Grafana

Grafana should visualize the Prometheus metrics and SLOs documented for Phase 6.

Suggested dashboards:

- API availability and status classes
- HTTP latency and error budget burn
- public response and streaming volume
- provider health and fallback behavior
- database pool utilization and slow queries
- Redis latency and lock contention
- worker queue depth, retries, and dead letters
- RAG and vector search latency

Dashboard JSON is not committed yet. Add it under `deploy/grafana` after metric names stabilize.
