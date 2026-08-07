# Prometheus

Prometheus metrics are disabled by default:

```text
MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true
```

When enabled, scrape:

```text
GET /metrics
```

The endpoint requires no authentication. When Prometheus support is off it answers
`404 metrics_disabled`, which a scraper reports as a failed scrape rather than as an
empty body.

Every family carries a `service` label — a builder-time global label whose value is
`MOIRA_TELEMETRY__SERVICE_NAME` (default `moira`).

Dashboard and alert rules built on these families live in
[`deploy/observability/`](../deploy/observability/README.md).

## Families

Names below are exactly what `src/infra/metrics.rs` declares. Counters marked *seeded*
are registered at zero on process start, so an alert written against them fires on the
condition rather than on absent data; histograms are not seeded and appear on first
observation.

### HTTP

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_http_requests_total` | counter | — | seeded |
| `moira_http_response_status_class_total` | counter | `status_class` | `1xx`…`5xx`, `other`; seeded for 2xx–5xx |
| `moira_http_latency_micros_total` | counter | — | cumulative µs, superseded by the histogram below; kept so existing averages keep working |
| `moira_http_request_duration_seconds` | histogram | `route`, `method`, `status_class` | buckets 5ms → 10s |
| `moira_public_responses_created_total` | counter | — | seeded |
| `moira_public_streams_started_total` | counter | — | seeded |

`route` is the matched Axum route *template*, never the resolved path; a request that
matched no route is labelled `unmatched`. `method` is folded into a closed set.

### Provider execution

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_execution_duration_seconds` | histogram | `provider_type`, `outcome` | buckets 50ms → 120s |
| `moira_execution_ttft_seconds` | histogram | `provider_type` | streamed attempts only; buckets 25ms → 20s |
| `moira_provider_outcome_total` | counter | `provider_type`, `model_key`, `outcome` | not seeded — series appear per configured model |

`provider_type` is the `ProviderType` enum: `openai`, `openai_compatible`, `anthropic`,
`gemini`, `deepseek`, `azure_openai`, `local`, `custom`. `outcome` is `succeeded`,
`failed` or `cancelled` from `ExecutionStatus`, otherwise the snake-case
`ExecutionFailureClass` name (`provider_timeout`, `circuit_open`,
`credential_expired`, …). Provider error *text* is never a label. `model_key` is
admin-configured runtime configuration, so its cardinality is bounded by the operator's
model catalogue.

### Database, Redis, runtime config

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_db_pool_connections` | gauge | `state` = `total` \| `idle` | sampled once per scrape |
| `moira_redis_enabled` | gauge | — | 1 when Redis coordination is on |
| `moira_redis_operation_failures_total` | counter | `operation` | `rate_limit`, `permit_acquire`, `permit_release`, `publish`, `subscribe`; seeded |
| `moira_runtime_invalidations_total` | counter | `channel` | `postgres` (authoritative) \| `redis` (additive); seeded |

### Background workers

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_workers_enabled` | gauge | — | 1 when workers run in this process |
| `moira_worker_ticks_total` | counter | — | supervisor ticks; seeded |
| `moira_worker_jobs_claimed_total` | counter | `job_name` | seeded per declared job |
| `moira_worker_jobs_completed_total` | counter | `job_name` | seeded |
| `moira_worker_jobs_failed_total` | counter | `job_name` | attempt failed and was rescheduled; seeded |
| `moira_worker_jobs_dead_letter_total` | counter | `job_name` | `max_attempts` exhausted; seeded |
| `moira_worker_queue_enqueue_rejected_total` | counter | — | pending-depth cap reached; seeded |
| `moira_worker_leader_held` | gauge | `job_name` | leader-gated jobs only; per-process, no replica label |
| `moira_retention_runs_total` | counter | — | seeded |
| `moira_retention_rows_deleted_total` | counter | `table` | seeded per swept table |

`job_name` comes from the closed `WORKER_JOB_NAMES` list in `src/infra/workers.rs`.

### RAG, retrieval, memory, summarization

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_rag_ingestion_runs_total` | counter | `outcome` = `succeeded` \| `failed` | seeded |
| `moira_rag_chunks_written_total` | counter | — | seeded |
| `moira_rag_embeddings_written_total` | counter | — | zero unless `rag_embeddings_enabled`; seeded |
| `moira_embedding_batch_latency_seconds` | histogram | — | one document's whole embedding run; buckets 50ms → 120s |
| `moira_retrieval_runs_total` | counter | `outcome` | seeded |
| `moira_retrieval_latency_seconds` | histogram | — | one retrieval pass; buckets 5ms → 30s |
| `moira_memory_extraction_runs_total` | counter | `outcome` | seeded |
| `moira_memory_extraction_written_total` | counter | — | inserts only, not confirmed duplicates; seeded |
| `moira_memory_extraction_rejected_total` | counter | — | refused by the application's memory policy; seeded |
| `moira_summarization_runs_total` | counter | `outcome` | seeded |
| `moira_summarization_inline_reasoning_total` | counter | — | successful runs whose stored summary is the model's chain of thought (finding F57); seeded |

Retrieval, memory extraction and summarization never return their failure to the
caller. These counters and their `*_runs` tables are the only place a failing one
surfaces.

### Admin identity

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_admin_invite_outcomes_total` | counter | `outcome` | `created`, `redeemed`, plus bounded denial reasons and `other`; seeded |
| `moira_admin_identity_grant_events_total` | counter | `event` | `granted`, `revoked`, `ownership_transferred`; seeded |

### Content envelopes

| Family | Type | Labels | Notes |
|---|---|---|---|
| `moira_content_envelope_seal_total` | counter | `profile` | envelopes sealed; seeded per AAD profile |
| `moira_content_envelope_open_total` | counter | `profile`, `data_key_id` | envelopes opened end to end; **not** seeded — see the cardinality note below |
| `moira_content_envelope_open_failed_total` | counter | `profile`, `reason` | opens refused at any stage; seeded across `profile` × `reason` |

`open_total` and `open_failed_total` are disjoint: the success counter is written
after the last fallible step, so no single call moves both. A refused open moves
only the failure counter.

`reason` is the same `&'static str` the neighbouring WARN line logs, so the log and
the metric cannot disagree. Every AEAD failure collapses into the single opaque
`aead_open_failed` — splitting it would turn the scrape endpoint into an oracle for
whether a guessed key or a doctored blob got closer. The remaining reasons are
framing facts already readable by anyone holding the ciphertext.

## Not emitted

No metric exists for these, so nothing can chart or alert on them:

- **token usage** — recorded per execution in the database and served on
  `/api/v1/usage`, not as a metric family;
- **worker queue depth** — the saturation signal is
  `moira_worker_queue_enqueue_rejected_total`;
- **Redis latency** — only the failure counter above;
- **SQL query timing** — only the pool-occupancy gauge.

## Cardinality rules

Labels are the security-relevant property: an unbounded label set is a
memory-exhaustion vector on the scrape path. Every label value above comes from a
closed set — a domain enum, an HTTP status class, a matched route template, or an
admin-configured identifier.

`data_key_id` on `moira_content_envelope_open_total` is the one exception, and it is
bounded by operator action rather than by a fixed enum. Its growth rate is one value
per key rotation (`data_key_rotation_days`, default 30), never one per request. The
bound is enforced structurally: the label is read off the *resolved* `ContentCipher`,
so a value can only exist if the loaded keyring carries that key. An envelope naming
an unknown key is refused before the counter, and the failure family carries no
`data_key_id` at all — without that ordering, anyone with database write access could
mint arbitrary UUIDs into the `*_encrypted` columns and grow the series set without
limit. Within one process the series accumulate, because a retired key's series
survives until restart; the true bound is the number of distinct non-retired content
keys the process has loaded since boot, which reaches two digits only after years.

Future metrics must never carry raw path parameters, user IDs, prompts, response IDs,
execution IDs, conversation IDs, application IDs, API-key prefixes, email addresses,
provider error text, or any other tenant-specific free-form value. `src/infra/metrics.rs`
guards this with an `ALLOWED_LABEL_KEYS` allow-list checked by its own unit tests: a call
site that introduces a new label key fails the suite until the key is added deliberately.
