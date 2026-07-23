# Redis

Redis is optional by default and configured through:

```text
MOIRA_REDIS__ENABLED=true
MOIRA_REDIS__URL=redis://redis:6379/0
MOIRA_REDIS__NAMESPACE=moira
MOIRA_REDIS__INVALIDATION_CHANNEL=moira:runtime-config
```

Current implementation:

- validates Redis configuration at startup
- checks Redis readiness through `/health/ready`
- exposes `moira_redis_enabled` on `/metrics`
- provides a namespaced Redis client for Phase 6 coordination work

Redis must not store long-term conversations, memory records, RAG documents, embeddings, audit logs, or durable response state. PostgreSQL remains the durable source of truth.

Pending Phase 6 TODOs include distributed rate limits, distributed concurrency permits, idempotency execution locks, Pub/Sub invalidation listeners, and leader election.
