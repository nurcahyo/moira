# Phase 1-6 TODO

This list tracks what is still left to harden or complete after the current Phase 1-6 implementation pass.

## Phase 1: Security Foundation

- TODO: Complete repository trait coverage for every PostgreSQL repository, including public/runtime repositories, so services can depend on traits consistently.
- TODO: Extend service-owned command transactions beyond the ten core idempotent admin create/rotate operations to every remaining sensitive multi-step update, revoke, and delete flow.
- TODO: Replace unkeyed admin command request hashes with versioned HMAC-SHA-256 using a dedicated idempotency pepper, so secret-bearing request hashes cannot be used as offline credential verifiers after a database-only compromise.
- TODO: Quarantine or remove unused legacy scaffolding that still models old provider/config concepts, such as legacy `owner_scope` DTOs and unregistered chat-route types, once compatibility is confirmed unnecessary.
- TODO: Add deeper credential-resolution integration tests for the full precedence order across user, application, tenant, and global scopes.
- TODO: Expand AAD regression tests to prove credential ciphertext fails to decrypt when any bound AAD field changes.
- TODO: Add pepper-rotation tests that verify old API-key hashes remain verifiable while new keys use the active pepper version.
- TODO: Add JWT cache bound/eviction tests and negative tests for disallowed algorithms, audience mismatch, and delegation without scope.
- TODO: Add security log assertions proving raw API keys, JWTs, ciphertext, nonces, and decrypted secrets are never emitted through tracing or audit metadata.

## Phase 2: Admin APIs And Runtime Config

- TODO: Split `AdminService` into focused domain services for applications, providers, models, credentials, JWT issuers, system keys, consumer keys, audit queries, idempotency, validation, and runtime invalidation.
- TODO: Replace simplified list pagination with real opaque cursor pagination using stable `created_at DESC, id DESC` ordering and `has_more`/`next_cursor` calculation.
- TODO: Require `If-Match` consistently on every versioned mutation and upsert, including application execution policy PUT, and return `409 resource_version_conflict` for stale versions.
- TODO: Extend atomic idempotency and sanitized deterministic-failure replay to the runtime-policy, RAG, conversation, memory, and other endpoints that advertise `Idempotency-Key`. (For the RAG/conversation/memory slice, see `plans/02b-idempotency-replay.md`. DONE for RAG: `rag.collection.create`, `rag.document.create`, and `rag.document.ingest` — the latter shared with `/reindex` — now replay atomically through `AdminCommandRunner`, per `plans/02b-idempotency-replay.md` and `docs/idempotency.md`. Still open: runtime-policy routes, and the conversation/memory create routes, which do not yet advertise `Idempotency-Key` at all.)
- TODO: Reject unknown query fields consistently on all admin list/filter endpoints.
- TODO: Finish centralized validation coverage for metadata depth/size, secret-like keys, custom headers, dangerous outbound headers, priorities, capabilities, expiration windows, and scope narrowing.
- TODO: Harden JWKS refresh with full SSRF checks, strict timeout, response size and content-type limits, valid JWKS parsing, singleflight refresh, old-cache retention on failure, and audit records.
- TODO: Add production HTTP middleware for body limits, content-type enforcement, request timeout, panic handling, secure response headers, redacted tracing, and no compression for once-only key secret responses.
- TODO: Align configurable `maximum_request_bytes` policy with the actual Axum body-limit layer, including per-route public/admin limits and tests for oversized JSON requests.
- TODO: Add integration tests for every admin route group covering CRUD/actions, filters, cursor pagination, audit writes, dependency conflicts, soft deletion, stale versions, and runtime cache invalidation.
- TODO: Add fault-injection coverage for admin replay serialization, ledger finalization, connection loss, commit failure, and post-commit runtime-cache invalidation count.
- TODO: Add cross-application consumer isolation tests that verify hidden/denied resources cannot be enumerated.

## Phase 3: Provider Runtime, Routing, And Rig Execution

- TODO: Implement the Rig `Agent`/`AgentRunner` path for approved tool-enabled agent profiles instead of only direct completion.
- TODO: Make custom providers executable only after a safe explicit provider contract exists; they are currently configurable but rejected at runtime.
- TODO: Use provider health snapshots and circuit state as first-class candidate filters and ranking inputs, not only static active configuration.
- TODO: Add durable or cross-instance strategy notes for concurrency limits and circuit breakers; the current controls are in-memory per process.
- TODO: Complete pricing and cost normalization so `usage_records.estimated_total_cost` is populated when model pricing is configured.
- TODO: Extend the total execution deadline across slow routing, credential resolution, runtime construction, and terminal persistence without abandoning active-attempt cleanup.
- TODO: Add structured-output execution tests that verify schema mapping, provider behavior, and `StructuredOutputInvalid` classification.
- TODO: Add runtime-cache tests for invalidation, TTL expiry, provider/runtime-policy changes, and stale-handle cleanup.
- TODO: Add benchmarks or load tests for candidate selection, runtime cache behavior, concurrency limits, and streaming backpressure.

## Phase 4: Public Responses API And SSE

- TODO: Add a database-backed public SSE test for a connected client that stops reading without disconnecting, proving bounded send timeout releases permits and leaves no attempt `started` or response `in_progress`.
- TODO: Replace scheduling-delay-based post-delta provider failure tests with explicit producer/consumer acknowledgement gates.
- TODO: Record non-streaming client disconnect/cancellation audit events reliably.
- TODO: Implement response persistence modes beyond metadata-only, including encrypted content storage, retrieval, retention, and cleanup semantics.
- TODO: Add retention cleanup for expired `responses` and idempotency records.
- TODO: Implement full public body-size, decompression, content-type, timeout, secure-header, tracing, and panic middleware.
- TODO: Strengthen public image URL SSRF validation with private/link-local/multicast/metadata denial, DNS resolution checks, rebinding revalidation where practical, and egress allow-list support.
- TODO: Replace public list placeholders with real cursor pagination for executions, usage, models, and routes.
- TODO: Make public model/route discovery include credential availability and health/routing eligibility where safe to expose.
- TODO: Add tokenizer-aware context budgeting and provider/model-specific input/output limit checks.
- TODO: Add approved tool registry support before allowing public tool declarations; keep arbitrary client tools rejected until then.
- TODO: Expand the `/v1/responses` compatibility adapter for supported OpenAI Responses fields such as structured text format, richer input arrays, streaming event naming, and compatible error bodies.
- TODO: Keep `/v1/chat/completions` unregistered unless a future phase explicitly approves it.
- TODO: Improve public idempotency replay so failed executions replay with their original sanitized status instead of a generic mapped error.
- TODO: Add end-to-end public API tests for create, stream, replay conflict, model override denial, consumer plus JWT identity binding, response retrieval, usage filtering, discovery, and OpenAPI exposure.

## Phase 5: Conversations, Context Planning, Memory, Retrieval, And RAG

- TODO: Replace the context-planning boundary with a real planner that loads bounded history, latest summary, memory candidates, RAG chunks, and writes `context_plans`.
- TODO: Inject planned context into the existing execution service without exposing storage internals, protected instructions, or untrusted retrieved text as system instructions.
- TODO: Implement live conversation summarization using the existing execution kernel, immutable summary versions, manual and policy-triggered summarize paths, singleflight protection, and audit.
- TODO: Implement automatic memory extraction using structured execution output, confidence/type/sensitivity validation, source references, dedupe, contradiction handling, and explicit consent enforcement.
- TODO: Implement memory embeddings through Rig embedding APIs with batch limits, model/dimension versioning, cancellation, supersession, and no content logging.
- TODO: Implement semantic memory retrieval with vector queries, policy filters, score thresholds, usage counters, and strict application/tenant/user isolation.
- TODO: Implement a retrieval service that combines memory and RAG candidates separately, records `retrieval_runs`, and exposes diagnostic metadata only through diagnostic scopes.
- TODO: Implement document chunking with paragraph, Markdown, and token-window strategies; preserve UTF-8 boundaries; enforce chunk count/size limits; and persist deterministic chunk hashes.
- TODO: Finish direct-text ingestion so it creates chunks and chunk embeddings, not only document metadata and versions.
- TODO: Add safe remote URL ingestion with HTTPS-by-default SSRF checks, DNS rebinding protection, redirect limits, MIME/size/time limits, no forwarded credentials, and sanitized stored metadata.
- TODO: Wire Rig embedding integration for both memory and RAG paths, and document the exact Rig embedding API/version assumptions.
- TODO: Implement RAG vector, keyword, and hybrid retrieval with collection/document/version filters, diversity controls, required-retrieval behavior, and provenance.
- TODO: Populate response `citations` from retrieved context source provenance when retrieval is used, while keeping exact spans absent unless supported by the source.
- TODO: Document MVP scope clearly so conversation, explicit memory, and RAG endpoints are advertised as persistence/configuration primitives until retrieval, chunking, embeddings, context injection, and citations are wired end to end. (Descoped for MVP: plans/02a-mvp-boundary-honesty.md makes the current no-op behavior honest in the API contract; full implementation remains tracked here for plans/11-rag-memory-intelligence.md.)
- TODO: Add response-time conversation history loading and tokenizer-aware context budgeting; return `context_length_exceeded` when required content cannot fit.
- TODO: Add conversation export packaging and deletion propagation for derived memories, vectors, context plans, retrieval runs, and response conversation links.
- TODO: Enforce optimistic concurrency, `If-Match`, and idempotency consistently for Phase 5 public and admin create/update/ingest operations.
- TODO: Add full route-level integration, security, and concurrency tests from the Phase 5 plan, including cross-tenant vector isolation and no prompt/content leakage in audit or logs.
- TODO: Add query-plan and benchmark docs for pgvector HNSW indexes using realistic row counts, configured dimensions, and representative filters.

## Phase 6: Production Hardening And Enterprise Operations

- TODO: Replace in-memory public rate limiting with Redis-backed distributed token buckets and request actor fingerprints.
- TODO: Replace in-memory execution concurrency with Redis-backed distributed global, provider, application, and user concurrency permits.
- TODO: Add cluster admission or a database lease that prevents out-of-band scaling beyond one API replica until distributed controls replace the MVP Helm gate.
- TODO: Move HTTP idempotency execution locking to distributed Redis locks while keeping PostgreSQL as the durable replay ledger.
- TODO: Add Redis Pub/Sub runtime invalidation listeners for all API instances and publish invalidation events from every runtime-config mutation.
- TODO: Add leader election for singleton workers such as cleanup, cache warming, provider health probing, and OAuth refresh.
- TODO: Build durable worker queues with exponential retry, cancellation, dead-letter handling, metrics, tracing, and bounded payload storage.
- TODO: Implement actual worker jobs for memory extraction retry, summarization retry, embedding retry, document ingestion retry, OAuth refresh, retention cleanup, vector cleanup, and cache warming.
- TODO: Persist provider health rolling windows for latency, error rate, timeout rate, rate-limit rate, throughput, token/sec, and circuit state.
- TODO: Add `GET /api/v1/admin/providers/{provider}/health` backed by persisted health windows and guarded by runtime diagnostic scopes.
- TODO: Feed provider health, cost, saturation, and recent failures into deterministic adaptive routing.
- TODO: Add full OpenTelemetry SDK/exporter wiring for HTTP, SQL, Redis, Rig execution, routing, retrieval, embedding, streaming, and workers.
- TODO: Replace aggregate-only in-process metrics with full Prometheus histograms/summaries for latency, TTFT, TPS, provider health, DB pool utilization, Redis latency, worker queues, and vector search latency.
- TODO: Add Grafana dashboard JSON and Alertmanager alert rules for the documented SLOs.
- TODO: Add production structured-log redaction tests and trace/log correlation tests for request, execution, application, provider, and route identifiers.
- TODO: Add Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, and Kubernetes Secret loaders behind the existing secret configuration boundary.
- TODO: Add automated PostgreSQL/pgvector backup, restore, and migration rollback drills with documented RPO/RTO evidence.
- TODO: Add reproducible load-test scripts for 1k, 5k, 10k, and 50k concurrent users covering streaming and non-streaming flows.
- TODO: Add chaos-test automation for Redis, PostgreSQL, provider, network, high-latency, stream-interruption, and worker-crash scenarios.
- TODO: Add SBOM generation to the Docker build/publish pipeline and store artifacts in CI.
- TODO: Add SAST, DAST, secret scanning, container scanning, OWASP ASVS evidence, and penetration-test reporting gates.
- TODO: Validate Helm and Kubernetes artifacts in CI against target cluster versions, including ServiceMonitor CRDs where installed.
- TODO: Add production smoke tests for Kubernetes rollout, readiness, `/metrics`, Redis connectivity, worker supervisor startup, and graceful shutdown.

## Cross-Phase Verification

- TODO: Add OpenAPI generation validation in CI.
- TODO: Add secret-leak snapshot tests for HTTP responses, OpenAPI schemas, audit metadata, and logs.
- TODO: Add prompt/content-leak snapshot tests for conversation messages, memories, RAG documents, vector records, retrieval diagnostics, HTTP responses, audit metadata, and logs.
- TODO: Add concurrency tests for simultaneous credential rotations, key rotations, idempotent creates, public response creation, conversation message appends, memory updates, and RAG ingestion.
- TODO: Add documented manual smoke tests for bootstrap system key, admin setup, route/model configuration, credential setup, internal execution, public response creation, streaming, conversation attach, explicit memory, and direct-text RAG ingestion.
- TODO: Isolate database-backed integration test binaries or add deterministic teardown so repeated local runs cannot accumulate routing, issuer, credential, or application fixtures.
- TODO: Support migration-contract validation in restricted CI databases that cannot create and forcibly drop temporary databases.

## Not TODO For Phases 1-6

- Workflow engines
- Billing and payment
- Compatibility APIs beyond the approved `/v1/responses` subset
- Unrestricted web crawling or arbitrary remote document ingestion without the Phase 5 SSRF and size-limit controls
- External vector databases beyond the PostgreSQL/pgvector foundation
- Cross-application or cross-tenant learning
- Fine-tuning pipelines
- A second execution engine outside the existing Rig-backed execution service
