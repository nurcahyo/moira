# Deployment

Moira production deployments run the API as a stateless process backed by PostgreSQL/pgvector and optional Redis coordination.

Minimum production dependencies:

- PostgreSQL with the pgvector extension
- Redis when distributed coordination is enabled
- HTTPS ingress or load balancer
- External secret source for the master encryption key and API-key pepper

Recommended rollout:

1. Build the production image from `Dockerfile`.
2. Apply database migrations from a one-shot job or controlled release task.
3. Create the first root system key with `moira bootstrap-system-key`.
4. Deploy the API with `MOIRA_DATABASE__REQUIRE=true`.
5. Enable `MOIRA_REDIS__ENABLED=true` only after Redis is reachable from all API pods.
6. Enable `MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true` and scrape `/metrics`.
7. Enable workers after Redis and database readiness are stable.

No real secrets belong in manifests or Helm values. Use sealed secrets, an external secret operator, Vault, or cloud secret managers.
