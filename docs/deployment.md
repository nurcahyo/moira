# Deployment

Moira's hardened MVP runs one API replica backed by PostgreSQL/pgvector.
Rate limiting, circuit state, and execution concurrency are process-local, so
multiple API replicas are rejected until distributed coordination is implemented.

Minimum production dependencies:

- PostgreSQL with the pgvector extension
- Redis when distributed coordination is enabled
- HTTPS ingress or load balancer
- External secret source for the master encryption key and API-key pepper

Recommended rollout:

1. Build the production image from `Dockerfile`.
2. Apply database migrations with `moira migrate` from the Helm migration Job or
   another controlled release task.
3. Create the first root system key with `moira bootstrap-system-key`.
4. Deploy the API with `MOIRA_DEPLOYMENT__ENVIRONMENT=production`,
   `MOIRA_DATABASE__REQUIRE=true`, and
   `MOIRA_DATABASE__MIGRATE_ON_STARTUP=false`.
5. Enable `MOIRA_REDIS__ENABLED=true` only after Redis is reachable from all API pods.
6. Enable `MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true` and scrape `/metrics`.
7. Keep placeholder workers disabled; enable workers only after a real worker
   implementation and distributed coordination are available.

Production configuration is validated before telemetry initialization, database
access, migrations, worker startup, or socket binding. The production validator
also rejects insecure key fallbacks, unauthenticated administration, trusted
development headers, wildcard CORS, insecure provider URLs, and an API replica
count other than one.

No real secrets belong in manifests or Helm values. Use sealed secrets, an external secret operator, Vault, or cloud secret managers.

## The admin console

The console is a separate deployable with its own image, its own database, its
own release cadence and its own chart, `charts/moira-console`. It is deliberately
not a subchart of `charts/moira`.

It stays at `replicaCount: 1`. Sessions, rate limits, the Better Auth ES256 key
pair and the sealed OAuth client secrets are all shared through
`CONSOLE_DATABASE_URL`, but the auth-config snapshot in
`console/lib/auth-runtime.ts` is still per process, so two pods can serve
different provider configurations — including different client secrets after a
rotation — for an unbounded time. `autoscaling` and `podDisruptionBudget` are
disabled against the same constraint and are restored together with it. The
reversal condition is written out at `charts/moira-console/values.yaml:29-81`.

The console serves **one** enabled auth provider. Going to N is a staged rollout
with a mandatory verification between the server change and the console change:
see [console-multi-provider-rollout.md](console-multi-provider-rollout.md).
