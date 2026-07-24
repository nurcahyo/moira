# Chaos Testing

Required chaos scenarios:

- provider unavailable
- Redis unavailable
- PostgreSQL unavailable
- network partition
- high latency
- provider timeout
- stream interruption
- worker crash

Expected behavior:

- fail closed on auth and secret access
- return bounded, classified errors
- avoid duplicate idempotent executions
- preserve durable audit records where PostgreSQL is available
- recover after dependencies return
- avoid logging prompts, secrets, JWTs, or embeddings

Automated chaos tests are tracked in `docs/todo.md`.
