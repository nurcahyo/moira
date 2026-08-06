//! Error translation keys.
//!
//! Add new `moira.error.*` entries here so agents can quickly find the failure
//! vocabulary without scanning the entire catalog.

use super::I18nEntry;

pub const RESPONSE_ERROR_CATALOG: &[I18nEntry] = &[
    I18nEntry {
        key: "moira.error.bad_request",
        default_message: "The request could not be processed.",
        description: "Generic client-side request validation or shape errors.",
    },
    I18nEntry {
        key: "moira.error.cluster_lease_denied",
        default_message: "This replica does not hold a valid cluster admission lease.",
        description: "Used by GET /health/ready when the replica's row in cluster_replica_leases has been lost or reclaimed mid-run, so the replica is outside the configured cluster.max_replicas ceiling and must stop receiving traffic. Denial at process startup is a fatal log and a non-zero exit, not a response, so it carries no key.",
    },
    I18nEntry {
        key: "moira.error.configuration_error",
        default_message: "The service configuration is invalid.",
        description: "Used when Moira cannot start or serve a request because a configuration value is missing or invalid (for example, telemetry export enabled without an endpoint).",
    },
    I18nEntry {
        key: "moira.error.conflict",
        default_message: "The request could not be completed because it conflicts with existing state.",
        description: "Used when a resource or operation cannot proceed because of a state conflict.",
    },
    I18nEntry {
        key: "moira.error.database_error",
        default_message: "A database error occurred.",
        description: "Used when a database operation fails unexpectedly while serving a request.",
    },
    I18nEntry {
        key: "moira.error.database_unavailable",
        default_message: "The database is temporarily unavailable.",
        description: "Used when Moira cannot reach or use its database, for example when a required database connection is not configured.",
    },
    I18nEntry {
        key: "moira.error.forbidden",
        default_message: "You do not have permission to perform this action.",
        description: "Used when the caller is authenticated but not authorized.",
    },
    I18nEntry {
        key: "moira.error.http_client_error",
        default_message: "An outbound HTTP request failed.",
        description: "Used when Moira's own HTTP client fails to complete a request to a dependency, distinct from the more specific upstream provider conditions.",
    },
    I18nEntry {
        key: "moira.error.idempotency_conflict",
        default_message: "The idempotent request does not match the previous request for this key.",
        description: "Used when the same idempotency key is reused with incompatible request data.",
    },
    I18nEntry {
        key: "moira.error.idempotency_in_progress",
        default_message: "An identical request with this Idempotency-Key is already being processed. Retry shortly.",
        description: "Used when a concurrent request holding the same idempotency key has claimed the ledger record but has not finished, or when the advisory lock could not be acquired within the deadline.",
    },
    I18nEntry {
        key: "moira.error.internal_error",
        default_message: "An unexpected error occurred.",
        description: "Used for unclassified server-side failures.",
    },
    I18nEntry {
        key: "moira.error.invalid_cursor",
        default_message: "The pagination cursor is invalid.",
        description: "Used when a list cursor is malformed, tampered with, wrongly formatted, or reused across a different list endpoint.",
    },
    I18nEntry {
        key: "moira.error.invalid_request",
        default_message: "The request is invalid.",
        description: "Used when the request cannot be parsed or violates a basic contract rule.",
    },
    I18nEntry {
        key: "moira.error.not_found",
        default_message: "The requested resource was not found.",
        description: "Used when a requested resource does not exist or is not visible to the caller.",
    },
    I18nEntry {
        key: "moira.error.payload_too_large",
        default_message: "The request body exceeds the maximum allowed size.",
        description: "Used when a request body exceeds the configured per-route body limit.",
    },
    I18nEntry {
        key: "moira.error.rate_limited",
        default_message: "Too many requests were sent in a short period of time.",
        description: "Used when a caller must back off and retry later.",
    },
    I18nEntry {
        key: "moira.error.redis_error",
        default_message: "A cache service error occurred.",
        description: "Used when a Redis operation fails unexpectedly while serving a request.",
    },
    I18nEntry {
        key: "moira.error.request_timeout",
        default_message: "The request timed out before it could be completed.",
        description: "Used when the server-side request timeout elapses before the handler produces a response.",
    },
    I18nEntry {
        key: "moira.error.stream_interrupted",
        default_message: "The stream ended before the response could finish.",
        description: "Used when an SSE or streaming response terminates unexpectedly.",
    },
    I18nEntry {
        key: "moira.error.unauthorized",
        default_message: "Authentication is required.",
        description: "Used when the caller is missing valid credentials.",
    },
    I18nEntry {
        key: "moira.error.upstream_bad_response",
        default_message: "The upstream service returned an invalid response.",
        description: "Used when a dependency responds with an unexpected payload or status.",
    },
    I18nEntry {
        key: "moira.error.upstream_error",
        default_message: "The upstream provider request failed.",
        description: "Used when a call to an upstream provider fails in a way not covered by the more specific upstream_bad_response, upstream_timeout, or upstream_unavailable conditions.",
    },
    I18nEntry {
        key: "moira.error.upstream_timeout",
        default_message: "The upstream service timed out.",
        description: "Used when a dependency fails to respond in time.",
    },
    I18nEntry {
        key: "moira.error.upstream_unavailable",
        default_message: "The upstream service is temporarily unavailable.",
        description: "Used when a dependency cannot be reached or is refusing requests.",
    },
    I18nEntry {
        key: "moira.error.validation_failed",
        default_message: "The request validation failed.",
        description: "Used when one or more fields fail schema or domain validation.",
    },
    I18nEntry {
        key: "moira.error.worker_queue_capacity_exceeded",
        default_message: "The background job queue is at capacity.",
        description: "Used when WorkerQueue::enqueue refuses a job because the pending backlog has reached workers.queue_max_pending_jobs. Returned as 429 rather than 503 because it is backpressure: the request is well-formed and retrying later is the correct client behaviour. Plan 10 ships the queue with no synchronous producer, so this has no HTTP surface yet; it gains one the moment a request-path caller enqueues, which is why the entry lands with the code rather than after it.",
    },
    I18nEntry {
        key: "moira.error.context_length_exceeded",
        default_message: "The request context exceeds the available budget.",
        description: "Used when the context planner cannot fit required content — the caller's current turn — within application_conversation_policies.maximum_history_tokens even after excluding every optional section (RAG chunks, memories, the summary, and finally history). Deliberately distinct from moira.error.context_required_content_too_large, which is about one oversized required item rather than the assembled budget; conflating them would lose the difference between 'this message is too big' and 'the budget is too small'. The envelope's details field carries the machine-readable reason and the numeric budget.",
    },
    I18nEntry {
        key: "moira.error.context_required_content_too_large",
        default_message: "Required content is too large to process.",
        description: "Used when mandatory context exceeds the allowed size.",
    },
    I18nEntry {
        key: "moira.error.conversation_archived",
        default_message: "The conversation is archived.",
        description: "Used when a conversation cannot be modified because it is archived.",
    },
    I18nEntry {
        key: "moira.error.conversation_forbidden",
        default_message: "You are not allowed to access this conversation.",
        description: "Used when the caller cannot access the requested conversation.",
    },
    I18nEntry {
        key: "moira.error.conversation_not_found",
        default_message: "The conversation was not found.",
        description: "Used when the requested conversation does not exist or is not visible.",
    },
    I18nEntry {
        key: "moira.error.conversation_policy_disabled",
        default_message: "Conversation policies are disabled.",
        description: "Used when a conversation policy action is not permitted.",
    },
    I18nEntry {
        key: "moira.error.duplicate_application",
        default_message: "The application already exists.",
        description: "Used when an application key or slug would collide.",
    },
    I18nEntry {
        key: "moira.error.duplicate_resource",
        default_message: "The resource already exists.",
        description: "Used when a unique resource already exists.",
    },
    I18nEntry {
        key: "moira.error.embedding_provider_unsupported",
        default_message: "The configured embedding provider does not support embeddings.",
        description: "Used when an application's embedding policy names a provider whose type exposes no embedding model in the configured Rig version. Anthropic and DeepSeek expose none in rig-core 0.40; OpenAI-compatible, Azure OpenAI and Gemini do.",
    },
    I18nEntry {
        key: "moira.error.embedding_request_failed",
        default_message: "The embedding request failed.",
        description: "Used when a call to the embedding provider fails or exceeds the configured timeout. The provider's own response body is deliberately not propagated, because an embedding request body is document content.",
    },
    I18nEntry {
        key: "moira.error.embedding_response_invalid",
        default_message: "The embedding provider returned an invalid response.",
        description: "Used when the embedding provider returns a different number of vectors than inputs, or vectors of a width the schema cannot store. Rejected rather than padded: a truncated embedding is a corrupt index entry that would degrade retrieval invisibly.",
    },
    I18nEntry {
        key: "moira.error.execution_failed",
        default_message: "The execution failed.",
        description: "Used when an execution attempt ends in failure.",
    },
    I18nEntry {
        key: "moira.error.execution_in_progress",
        default_message: "An execution is already in progress.",
        description: "Used when a conflicting execution is already running.",
    },
    I18nEntry {
        key: "moira.error.idempotency_not_supported_for_stream",
        default_message: "Idempotency keys are not supported for response streams.",
        description: "Used when streaming rejects idempotent replay headers.",
    },
    I18nEntry {
        key: "moira.error.if_match_required",
        default_message: "The If-Match header is required for this request.",
        description: "Used when a versioned mutation is called without the If-Match precondition header.",
    },
    I18nEntry {
        key: "moira.error.image_too_large",
        default_message: "The image input is too large.",
        description: "Used when an uploaded image exceeds the configured limit.",
    },
    I18nEntry {
        key: "moira.error.image_url_not_allowed",
        default_message: "The image URL is not allowed.",
        description: "Used when a remote image URL fails policy checks.",
    },
    I18nEntry {
        key: "moira.error.input_too_large",
        default_message: "The input is too large.",
        description: "Used when the request body or prompt exceeds the configured limit.",
    },
    I18nEntry {
        key: "moira.error.invalid_execution_request",
        default_message: "The execution request is invalid.",
        description: "Used when the request cannot be converted into a valid execution.",
    },
    I18nEntry {
        key: "moira.error.invalid_metadata",
        default_message: "The metadata is invalid.",
        description: "Used when metadata fails validation.",
    },
    I18nEntry {
        key: "moira.error.jwks_url_rejected",
        default_message: "The JWKS URL was rejected by the server's security policy.",
        description: "Used when a configured JWKS URL fails scheme, address-range, size, content-type, or timeout validation.",
    },
    I18nEntry {
        key: "moira.error.max_output_tokens_exceeded",
        default_message: "The requested output tokens exceed the allowed limit.",
        description: "Used when the request exceeds the configured output budget.",
    },
    I18nEntry {
        key: "moira.error.memory_consent_required",
        default_message: "Memory consent is required.",
        description: "Used when a memory operation requires consent that is missing.",
    },
    I18nEntry {
        key: "moira.error.memory_disabled",
        default_message: "Memory is disabled.",
        description: "Used when a memory operation is not allowed by policy.",
    },
    I18nEntry {
        key: "moira.error.memory_not_found",
        default_message: "The memory item was not found.",
        description: "Used when a memory record does not exist or is not visible.",
    },
    I18nEntry {
        key: "moira.error.memory_sensitivity_forbidden",
        default_message: "The requested memory sensitivity is not allowed.",
        description: "Used when a memory item violates the allowed sensitivity level.",
    },
    I18nEntry {
        key: "moira.error.message_role_invalid",
        default_message: "The message role is invalid.",
        description: "Used when a message role is not accepted by the current endpoint.",
    },
    I18nEntry {
        key: "moira.error.metrics_disabled",
        default_message: "Prometheus metrics are disabled.",
        description: "Used when metrics are requested but disabled.",
    },
    I18nEntry {
        key: "moira.error.model_capability_mismatch",
        default_message: "The requested capability is not available for this request.",
        description: "Used when a requested feature cannot be served, either because the selected model does not support it or because the application's execution policy disables it. The previous wording blamed the model alone, which is wrong for the policy emitter (for example vision inputs disabled for an application).",
    },
    I18nEntry {
        key: "moira.error.model_not_found",
        default_message: "The model was not found.",
        description: "Used when the named model does not exist, is not visible to the caller, or could not be resolved while routing. Distinct from model_forbidden, which is returned when the model does resolve but the caller may not use it.",
    },
    I18nEntry {
        key: "moira.error.provider_url_not_allowed",
        default_message: "The provider URL is not allowed.",
        description: "Used when a provider URL fails SSRF or scheme checks.",
    },
    I18nEntry {
        key: "moira.error.rag_collection_not_found",
        default_message: "The RAG collection was not found.",
        description: "Used when a RAG collection does not exist or is not visible.",
    },
    I18nEntry {
        key: "moira.error.rag_document_not_found",
        default_message: "The RAG document was not found.",
        description: "Used when a RAG document does not exist or is not visible.",
    },
    I18nEntry {
        key: "moira.error.rag_document_parse_failed",
        default_message: "The RAG document could not be parsed.",
        description: "Used when ingestion or parsing fails.",
    },
    I18nEntry {
        key: "moira.error.rag_document_too_large",
        default_message: "The RAG document is too large to ingest.",
        description: "Used when chunking a document version would exceed rag.max_chunks_per_document. The request is refused rather than the document truncated, because a truncated document produces a retrieval index that is quietly incomplete.",
    },
    I18nEntry {
        key: "moira.error.rag_document_type_unsupported",
        default_message: "The RAG document type is not supported.",
        description: "Used when a document type is outside the supported set.",
    },
    I18nEntry {
        key: "moira.error.resource_version_conflict",
        default_message: "The resource version does not match.",
        description: "Used when If-Match does not match the stored version.",
    },
    I18nEntry {
        key: "moira.error.response_terminal",
        default_message: "The response is already terminal.",
        description: "Used when a response can no longer be modified.",
    },
    I18nEntry {
        key: "moira.error.responses_disabled",
        default_message: "Responses are disabled for this application.",
        description: "Used when the application policy disables responses.",
    },
    I18nEntry {
        key: "moira.error.retrieval_unavailable",
        default_message: "Retrieval is required for this request but is currently unavailable.",
        description: "Used when the context planner's retrieval or embedding backend cannot serve a query AND the application's application_embedding_policies.failure_behavior is 'fail_request'. It must never fire under the default 'continue_without_semantic_retrieval', where a retrieval failure degrades silently to a 200 with empty citations — a broken vector index must not take down the execution path. Both branches are pinned by named tests.",
    },
    I18nEntry {
        key: "moira.error.summarization_disabled",
        default_message: "Conversation summarization is disabled for this application.",
        description: "Used when POST /api/v1/conversations/{id}/summarize is called on an application whose application_conversation_policies.summarization_enabled is false. Deliberately NOT bypassable by the request's force flag: force overrides the two trigger thresholds, not the operator's switch, because the endpoint is caller-plane and the caller is not the operator.",
    },
    I18nEntry {
        key: "moira.error.summarization_not_needed",
        default_message: "There is nothing new to summarize in this conversation.",
        description: "Used when a summarize request is refused because the conversation's backlog does not warrant a new summary version. details.reason distinguishes the cases: no_new_messages (nothing has been said since the active summary's coverage boundary, which conversation_summary_boundary_unique makes unrepresentable and force therefore cannot override), below_message_threshold, below_token_threshold (both overridable with force), and no_persisted_content (the messages in the backlog carry no plaintext to summarize).",
    },
    I18nEntry {
        key: "moira.error.summarization_failed",
        default_message: "The conversation could not be summarized.",
        description: "Used when a summarization run reached the model and did not produce a storable summary — the completion call failed, or the reply was empty or exceeded the summary size ceiling. details.reason carries the failure class. Only the manual endpoint surfaces this; an automatic summarization failure is recorded on the metric and the audit row and never turns a successful response into an error.",
    },
    I18nEntry {
        key: "moira.error.routing_policy_provider_model_mismatch",
        default_message: "The routing policy references a provider model that does not belong to the selected provider.",
        description: "Used when a routing policy create/patch names a provider_model_id that is not owned by the given provider_id.",
    },
    I18nEntry {
        key: "moira.error.scope_invalid",
        default_message: "The requested scope is invalid.",
        description: "Used when a scope string fails validation.",
    },
    I18nEntry {
        key: "moira.error.streaming_not_supported",
        default_message: "Streaming is not supported for this application.",
        description: "Used when streaming is disabled by policy.",
    },
    I18nEntry {
        key: "moira.error.structured_output_invalid",
        default_message: "The structured output request could not be honoured.",
        description: "Used when a structured-output request cannot be honoured — because of the schema the caller supplied, or because of what the model sent back. Three emitters, all mapped to 422 by failure_http_status. Two are about the request: validate_response_format rejects a schema over public_api.maximum_schema_bytes, and build_completion_request rejects one that is not a readable schemars::Schema. One is about the reply: structured_output_from_text raises it when a schema-carrying request comes back as bytes that are not JSON, on the completion path and the streaming path alike (issue #80, decided 2026-08-06 — the fail-hard flip F29 deferred and F42 recorded as non-existent; both of those earlier wordings were true when written and are false from #80 onward). The flip exists because structured_output: null on a 200 was the same document a legitimately empty answer produces, so a caller could not tell a provider that did not comply from an empty result; now a failure is a 422 and only an answer is a 200. Moira parses JSON here rather than validating against the schema, so a reply that is valid JSON but violates the schema still succeeds, and null, {} and [] parse and are answers rather than failures. The message is a constant and never carries the provider's bytes. memory_extraction::FAILURE_STRUCTURED_OUTPUT_INVALID is the same string for a reply that parsed as JSON but is not the extraction envelope, but it is written to memory_extraction_runs.failure_class and never returned to a caller, so it never renders this message. StructuredOutputInvalid is in none of is_retryable, is_fallback_eligible or is_circuit_failure — one disposition for all three emitters, each reason stated at its function — and structured_output_invalid_has_only_the_three_emitters_its_catalog_entry_describes counts the emitters per file so a fourth cannot be added silently.",
    },
    I18nEntry {
        key: "moira.error.structured_output_unsupported",
        default_message: "Structured output is not supported.",
        description: "Used when structured output is disabled or unavailable.",
    },
    I18nEntry {
        key: "moira.error.system_key_scope_escalation",
        default_message: "The requested scope exceeds the effective system key scope.",
        description: "Used when a system key tries to mint broader scopes.",
    },
    I18nEntry {
        key: "moira.error.credential_override_forbidden",
        default_message: "Choosing the credential for a request is not allowed.",
        description: "Used when the caller names a specific credential but the application's execution policy forbids credential overrides, or the caller lacks the scope for them. Retrying will not help; omit the credential and let routing select one.",
    },
    I18nEntry {
        key: "moira.error.model_override_forbidden",
        default_message: "Choosing the model for a request is not allowed.",
        description: "Used when the caller names a specific model but the application's execution policy forbids model overrides, or the caller lacks the scope for them. Retrying will not help; omit the model and let routing select one.",
    },
    I18nEntry {
        key: "moira.error.provider_override_forbidden",
        default_message: "Choosing the provider for a request is not allowed.",
        description: "Used when the caller names a specific provider but the application's execution policy forbids provider overrides, or the caller lacks the scope for them. Retrying will not help; omit the provider and let routing select one.",
    },
    I18nEntry {
        key: "moira.error.route_override_forbidden",
        default_message: "Choosing the route for a request is not allowed.",
        description: "Used when the caller names a specific route but the application's execution policy forbids route overrides, or the caller lacks the scope for them. Retrying will not help; omit the route and let the application's default apply.",
    },
    I18nEntry {
        key: "moira.error.timeout_override_forbidden",
        default_message: "Timeout overrides are not allowed.",
        description: "Used when the caller attempts to override an enforced timeout.",
    },
    I18nEntry {
        key: "moira.error.tool_not_allowed",
        default_message: "Tools are not allowed for this caller.",
        description: "Used when tool usage is rejected by policy.",
    },
    I18nEntry {
        key: "moira.error.unsupported_input_type",
        default_message: "The input type is not supported.",
        description: "Used when a request includes an unsupported input content type.",
    },
    I18nEntry {
        key: "moira.error.unsupported_message_role",
        default_message: "The message role is not supported.",
        description: "Used when a request includes an unsupported message role.",
    },
    I18nEntry {
        key: "moira.error.unsupported_request_option",
        default_message: "The request option is not supported.",
        description: "Used when a request includes an unsupported option.",
    },
    I18nEntry {
        key: "moira.error.unsupported_tool",
        default_message: "The tool is not supported.",
        description: "Used when a tool declaration is not accepted.",
    },
    I18nEntry {
        key: "moira.error.application_unavailable",
        default_message: "The application is not available to serve this request.",
        description: "Used when the request targets an application the caller is not bound to, or one that is not currently active. Retrying will not help until the binding or the application's status is corrected.",
    },
    I18nEntry {
        key: "moira.error.route_not_found",
        default_message: "No route matched this request.",
        description: "Used when the requested route does not exist or is not visible to the caller. Retrying will not help until a matching route is configured.",
    },
    I18nEntry {
        key: "moira.error.agent_profile_not_found",
        default_message: "The route requires an agent profile that no longer exists.",
        description: "Used when the selected route's agent_profile_id names no live agent_profiles row, because the profile was soft-deleted or never existed. Moira refuses the execution rather than serving it without the profile's preamble (issue #79); the English message names the route and the profile id so the deployment can be corrected without reading server logs. Distinct from agent_profile_disabled, where the row is still there: the remedy here is to create a profile and repoint the route.",
    },
    I18nEntry {
        key: "moira.error.agent_profile_disabled",
        default_message: "The route requires an agent profile that is currently disabled.",
        description: "Used when the selected route's agent_profile_id names an agent profile whose status is disabled. Moira refuses the execution rather than serving it without the profile's preamble (issue #79). Distinct from agent_profile_not_found, which means no live row has that id: the remedy here is to re-enable the profile or point the route at an active one, and the HTTP status is 409 rather than 404 because the profile exists and is visible on the admin plane.",
    },
    I18nEntry {
        key: "moira.error.route_forbidden",
        default_message: "You are not allowed to use this route.",
        description: "Used when the route resolves but the caller's credentials or scopes do not permit its use. Distinct from route_not_found, which means no route resolved at all.",
    },
    I18nEntry {
        key: "moira.error.model_forbidden",
        default_message: "You are not allowed to use this model.",
        description: "Used when the model resolves but the caller's credentials or scopes do not permit its use. Distinct from model_not_found, which means no model resolved at all.",
    },
    I18nEntry {
        key: "moira.error.no_eligible_model",
        default_message: "No model is available that can serve this request.",
        description: "Used when no configured model satisfies both the routing policy and the capabilities the request needs. A configuration or request-shape problem, not a transient one.",
    },
    I18nEntry {
        key: "moira.error.credential_not_found",
        default_message: "No usable credential is available for this request.",
        description: "Used when no credential could be selected for the chosen model. Retrying will not help until a credential is configured for it.",
    },
    I18nEntry {
        key: "moira.error.credential_forbidden",
        default_message: "You are not allowed to use the requested credential.",
        description: "Used when the caller names a specific credential without the scope to override credential selection, or names one that is outside its reach.",
    },
    I18nEntry {
        key: "moira.error.credential_expired",
        default_message: "The credential needed for this request has expired.",
        description: "Used when the selected credential is past its validity period. Retrying will not help until it is renewed or replaced.",
    },
    I18nEntry {
        key: "moira.error.credential_disabled",
        default_message: "The credential needed for this request is disabled.",
        description: "Used when the selected credential exists but has been deactivated. Retrying will not help until it is re-enabled or replaced.",
    },
    I18nEntry {
        key: "moira.error.credential_decryption_failed",
        default_message: "The credential needed for this request could not be read.",
        description: "Used when a stored credential cannot be recovered for use. Nothing in the caller's request causes this and retrying will not help; it needs operator attention.",
    },
    I18nEntry {
        key: "moira.error.provider_configuration_invalid",
        default_message: "The configuration required to serve this request is invalid.",
        description: "Used when the stored settings for the selected model or its credential cannot be assembled into a usable request. Retrying will not help until the configuration is corrected.",
    },
    I18nEntry {
        key: "moira.error.provider_unavailable",
        default_message: "The provider is temporarily unavailable.",
        description: "Used when the selected provider cannot currently accept the request. Transient — retry after a short delay, or allow routing to fall back to another provider.",
    },
    I18nEntry {
        key: "moira.error.provider_rate_limited",
        default_message: "The provider is refusing requests because a rate limit was reached.",
        description: "Used when a provider rejects the attempt for exceeding its own request or token allowance. Transient — retry after a short delay; if it persists, the allowance is too small for the traffic.",
    },
    I18nEntry {
        key: "moira.error.provider_timeout",
        default_message: "The provider did not respond in time.",
        description: "Used when a single provider attempt exceeds its timeout. Transient — retry, optionally with a smaller request. Distinct from deadline_exceeded, which covers the whole execution.",
    },
    I18nEntry {
        key: "moira.error.provider_connection_failed",
        default_message: "The provider could not be reached.",
        description: "Used when the connection to a provider could not be established, or was lost before a response arrived. Transient — retry after a short delay.",
    },
    I18nEntry {
        key: "moira.error.provider_authentication_failed",
        default_message: "The provider rejected the credential used for this request.",
        description: "Used when a provider answers an attempt with an authentication or authorization refusal. Retrying will not help until the credential is corrected or replaced.",
    },
    I18nEntry {
        key: "moira.error.provider_invalid_response",
        default_message: "The provider returned a response that could not be understood.",
        description: "Used when a provider replies but the reply cannot be read as a valid completion. Not caused by the caller's input; report it if it persists.",
    },
    I18nEntry {
        key: "moira.error.provider_upstream_error",
        default_message: "The provider reported an error while handling this request.",
        description: "Used when a provider attempt fails provider-side in a way not covered by the more specific timeout, rate-limit, connection, or authentication conditions.",
    },
    I18nEntry {
        key: "moira.error.circuit_open",
        default_message: "Requests to this provider are paused after repeated failures.",
        description: "Used when the circuit breaker for the selected provider and model is open, so attempts are refused without being sent. Transient — it closes again on its own; retry after a short delay.",
    },
    I18nEntry {
        key: "moira.error.capacity_exhausted",
        default_message: "The service is at capacity and cannot accept this request right now.",
        description: "Used when a concurrency or rate allowance is already fully consumed. Transient — retry after a short delay, and reduce concurrency if it recurs.",
    },
    I18nEntry {
        key: "moira.error.request_cancelled",
        default_message: "The request was cancelled before it completed.",
        description: "Used when the caller disconnected, or the response stream was closed, before execution finished. Submit the request again if the result is still wanted.",
    },
    I18nEntry {
        key: "moira.error.deadline_exceeded",
        default_message: "The request ran out of time before it could be completed.",
        description: "Used when the execution's overall deadline elapses, as opposed to a single attempt timing out. Retry, or allow the request a longer budget.",
    },
    I18nEntry {
        key: "moira.error.stream_backpressure_exceeded",
        default_message: "The response stream was closed because it was not being read quickly enough.",
        description: "Used when a streaming consumer stops accepting events for long enough to miss the delivery deadline. Read the stream as it arrives, or use the non-streaming endpoint.",
    },
    // ---------------------------------------------------------------------
    // Plan 07 — identity foundation (admin identity claiming + runtime auth
    // provider settings).
    //
    // Every one of these is raised through `AppError::coded`/`conflict`, never
    // through `AppError::BadRequest`/`Forbidden`/`NotFound`, because those
    // derive the generic `bad_request`/`forbidden`/`not_found` codes and would
    // silently drop the specific key plan 08's console binds to.
    //
    // Deliberately absent: the setup-token credential codes
    // (`setup_token_invalid`/`_expired`/`_consumed`/`_target_mismatch`) and
    // `auth_provider_secret_*`. The first group belongs to the deferred
    // one-time-token path (decision D1); the second describes conditions that
    // can no longer occur, because Moira accepts, stores and binds no OAuth
    // client secret (decision D7).
    // ---------------------------------------------------------------------
    I18nEntry {
        key: "moira.error.unregistered_trusted_issuer",
        default_message: "The target issuer is not a registered, active trusted JWT issuer.",
        description: "Used when an admin-identity claim, or an auth-provider configuration, names a JWT issuer that has no active row in trusted_jwt_issuers. Moira never accepts a free-text issuer at claim time; register and enable the issuer first.",
    },
    I18nEntry {
        key: "moira.error.admin_claim_email_required",
        default_message: "An email address is required to claim an admin identity.",
        description: "Used when a claim omits an email address, presents an empty one, or presents a value from which no domain can be extracted. Email is required on every credential path; there is no exemption.",
    },
    I18nEntry {
        key: "moira.error.admin_claim_email_not_verified",
        default_message: "The email address for this identity is not verified.",
        description: "Used when a claim names an email address that the identity provider has not marked verified. The requirement is hard, not configurable, and applies on every credential path.",
    },
    I18nEntry {
        key: "moira.error.admin_claim_domain_not_allowed",
        default_message: "This email domain is not allowed to claim an admin identity.",
        description: "Used when the deny-by-default email-domain policy refuses a claim, either because no enabled auth-provider configuration governs the target issuer or because the email's domain is not in its allowed_email_domains. An unconfigured or empty allow-list denies every claim on every credential path; there is no first-claim exemption. The operator must create and enable an auth-provider configuration with the intended domains before any claim can succeed.",
    },
    I18nEntry {
        key: "moira.error.admin_identity_already_claimed",
        default_message: "This identity has already been granted admin access.",
        description: "Used when a claim targets an (issuer, subject) pair that already holds a grant. Raised by the unique index on admin_identities, so it holds even if the command runner's advisory-lock window is raced.",
    },
    I18nEntry {
        key: "moira.error.setup_claim_credential_required",
        default_message: "A system key is required to claim an admin identity.",
        description: "Used when POST /api/v1/admin/setup/claim arrives with no X-Moira-System-Key. A trusted-JWT bearer token is deliberately not accepted here, however well it verifies: if a verified JWT could claim, whoever reached a fresh deployment first would own it. Present the system key the operator holds.",
    },
    I18nEntry {
        key: "moira.error.setup_token_not_supported",
        default_message: "The one-time setup token path is not available on this deployment.",
        description: "Used when a claim populates the reserved setup_token field. The one-time-token credential path is deferred, and the field is refused rather than ignored: accepting and discarding it would let a caller believe Moira had honoured a credential it never read. Present the system key instead.",
    },
    I18nEntry {
        key: "moira.error.auth_provider_not_found",
        default_message: "The auth provider configuration was not found.",
        description: "Used when an auth-provider settings row does not exist or has been soft-deleted.",
    },
    I18nEntry {
        key: "moira.error.duplicate_auth_provider",
        default_message: "An auth provider is already configured for this method and issuer.",
        description: "Used when a create or patch would collide with the unique index over live auth_provider_settings rows on (method, issuer).",
    },
    I18nEntry {
        key: "moira.error.duplicate_trusted_jwt_issuer",
        default_message: "A trusted JWT issuer is already registered for this issuer.",
        description: "Used when registering a trusted JWT issuer would collide with the unique index over live trusted_jwt_issuers rows on issuer. Finding F13: this condition used to fall through to a 500 database_error - alone among Moira's uniqueness conflicts - so a client recovering from a half-finished registration could not adopt the existing row by catching a 409, and an operator was paged for what was only a duplicate. The remedy is to read the existing issuer rather than to create a second one; issuer is the identity of the row and is deliberately not patchable.",
    },
    I18nEntry {
        key: "moira.error.auth_provider_method_config_incomplete",
        default_message: "The auth provider configuration is incomplete for this method.",
        description: "Used when a create or enable request leaves the method's required non-secret configuration incomplete - for example generic_oidc with neither issuer nor discovery_url, or jwks with no jwks_url. Moira never checks for an OAuth client secret: under decision D7 the client secret is owned by the console and Moira does not store it.",
    },
    I18nEntry {
        key: "moira.error.auth_provider_url_not_allowed",
        default_message: "The configured URL is not allowed.",
        description: "Used when an auth-provider URL is not an absolute https URL with a host. This is a syntactic gate: no fetch happens here, and any future fetch must go through the SSRF-hardened client rather than a second implementation.",
    },
    I18nEntry {
        key: "moira.error.console_issuer_must_not_assert_scopes",
        default_message: "A console issuer must not map a scopes claim.",
        description: "Used when an auth-provider configuration links a trusted JWT issuer whose scopes_claim is set. Such an issuer's tokens self-assert scopes, which would defeat the admin_identities grant table as the sole source of human authorization (CONVENTIONS 7.5).",
    },
    // ---------------------------------------------------------------------
    // Plan 09 wave 4A — finding F23 and the trusted-issuer lifecycle.
    // ---------------------------------------------------------------------
    I18nEntry {
        key: "moira.error.duplicate_enabled_provider_for_issuer",
        default_message: "More than one enabled auth provider is bound to this trusted JWT issuer.",
        description: "Used when a create, patch or enable would leave two enabled auth_provider_settings rows bound to one trusted_jwt_issuers row, and by the admission-policy lookup when it finds that state already present. Finding F23: with two such rows the old query broke the tie on created_at, so the OLDEST row's allowed_email_domains governed every admin claim and every invite redemption regardless of which provider authenticated the human - silently, and in both directions. The remedy is to disable the row that should not govern; there is deliberately no automatic choice, because which provider governs admission is an operator decision. The same code is emitted at configure time by the service, by the partial unique index auth_provider_settings_one_enabled_per_trusted_issuer when two enables race, and at admission time when the invariant has been bypassed.",
    },
    I18nEntry {
        key: "moira.error.auth_provider_issuer_shadows_trusted_issuer",
        default_message: "This issuer is already registered as a trusted JWT issuer that the provider is not bound to.",
        description: "Used when an auth-provider row's own issuer column would equal the issuer string of an active trusted_jwt_issuers row it is not bound to. Finding F23 shape (b): such a row claims an issuer identity it does not have, and before the two-stage admission lookup it outranked the correctly-bound provider at any age while needing no binding at all - so no index on trusted_jwt_issuer_id could reach it. Binding the row to that issuer is permitted and is CONVENTIONS 7.3 mode 3 configured explicitly; leaving it unbound is not.",
    },
    I18nEntry {
        key: "moira.error.trusted_issuer_has_active_grants",
        default_message: "Active admin identity grants were made through this trusted JWT issuer.",
        description: "Used when deleting or disabling a trusted JWT issuer that still has active admin_identities rows. Both paths are soft, so admin_identities' foreign key never fires, while load_issuer filters on status = 'active' and deleted_at is null - retiring the issuer therefore stopped every grant made through it from authorising anybody, silently and deployment-wide. Revoke the grants first; the grants are not cascaded here because revocation has its own endpoint, its own audit trail and its own last-primary guard.",
    },
    // ---------------------------------------------------------------------
    // Plan 09 wave 2 — admin invitations and grant administration.
    //
    // Every code below has a pinned emitter and a pinned status, asserted by
    // `tests/admin_invite_lifecycle.rs`. Two of them - admin_identity_not_found
    // and admin_identity_already_revoked - are named by plan 09 with no
    // specified emitter at all; plan 09 section 0.5's rule is "pin both to a
    // path and status with a test, or drop them", so they are pinned to
    // PATCH/DELETE /api/v1/admin/admin-identities/{id} at 404 and 409.
    //
    // Deliberately absent: any recovery code. The is_recovery /
    // replaces_admin_identity_id swap is not built in this wave (decision
    // D-W2-1), and a code with no emitter is worse than the gap.
    // ---------------------------------------------------------------------
    I18nEntry {
        key: "moira.error.invite_not_found",
        default_message: "No invitation matches this token.",
        description: "Used when a preview or redemption presents a token that matches no live admin_invites row - either because no row has that prefix, or because the row's Argon2id hash does not verify against the presented token. The two are deliberately indistinguishable on the wire: telling a caller their prefix was right would turn the endpoint into a guessing oracle.",
    },
    I18nEntry {
        key: "moira.error.invite_expired",
        default_message: "This invitation has expired.",
        description: "Used when an invitation is past its expires_at. Expiry is derived from the timestamp on every read rather than stored as a status, because nothing sweeps for it. The invitation cannot be extended; an admin issues a new one.",
    },
    I18nEntry {
        key: "moira.error.invite_already_consumed",
        default_message: "This invitation has already been redeemed.",
        description: "Used when an invitation has already produced a grant. Invitations are single-use: the redeeming transaction re-checks the row under select-for-update, so two simultaneous redemptions of the same valid invitation produce exactly one grant and the loser receives this code.",
    },
    I18nEntry {
        key: "moira.error.invite_revoked",
        default_message: "This invitation has been revoked.",
        description: "Used when an invitation was revoked by an admin before it was redeemed. Distinct from invite_expired because the remedy differs: an expiry is a deadline that passed, a revocation is a deliberate withdrawal.",
    },
    I18nEntry {
        key: "moira.error.invite_email_mismatch",
        default_message: "This invitation was issued for a different email address.",
        description: "Used when an email-constrained invitation is redeemed by a verified address that is not the one it names. This is the INVITATION's own constraint, never the provider allow-list: the remedy is a reissued invitation, not a widened allow-list, which is why it must not be conflated with admin_claim_domain_not_allowed.",
    },
    I18nEntry {
        key: "moira.error.invite_domain_mismatch",
        default_message: "This invitation was issued for a different email domain.",
        description: "Used when a domain-constrained invitation is redeemed by a verified address outside that domain. Matching is exact - a parent domain does not admit its subdomains. This is the INVITATION's own constraint and must not be conflated with admin_claim_domain_not_allowed, whose remedy is different.",
    },
    I18nEntry {
        key: "moira.error.admin_invite_expiry_too_long",
        default_message: "The requested invitation lifetime exceeds the maximum allowed.",
        description: "Used when a create-invitation request asks for a lifetime above the server-side hard cap of 72 hours. The request is refused rather than clamped: an operator who believes they issued a long-lived invitation and silently received a short one would discover the difference at the worst possible moment.",
    },
    I18nEntry {
        key: "moira.error.admin_identity_not_found",
        default_message: "The admin identity was not found.",
        description: "Used by PATCH and DELETE /api/v1/admin/admin-identities/{id} when no live admin_identities row has that id. Follows the per-resource not-found convention already set by auth_provider_not_found, credential_not_found and route_not_found.",
    },
    I18nEntry {
        key: "moira.error.admin_identity_already_revoked",
        default_message: "This admin identity has already been revoked.",
        description: "Used by DELETE /api/v1/admin/admin-identities/{id} when the target grant is already revoked, and by PATCH when it would change ownership on a revoked grant. A repeat revoke under a fresh Idempotency-Key is the path that reaches it: answering 409 rather than 204 tells the caller their view of the world is stale instead of pretending an action occurred.",
    },
    I18nEntry {
        key: "moira.error.admin_identity_last_primary",
        default_message: "This is the last admin identity that can manage other admins.",
        description: "Used when clearing is_primary, or revoking a primary grant, would leave zero active primary admins - locking every remaining admin out of admin management and leaving system-key break-glass as the only re-entry path. Transfer ownership to another identity first. This guard is expressible only because ownership is row state: as a scope it would have been implied by moira:admin and held by everyone, leaving nothing to count.",
    },
    I18nEntry {
        key: "moira.error.admin_identity_not_primary",
        default_message: "Only a primary admin identity may manage other admin identities.",
        description: "Used when a caller who is not a primary admin attempts an ownership transfer or a grant revocation. Ownership is admin_identities.is_primary, not a scope: AuthorizationService::has_scope grants a moira:admin-holding trusted-JWT actor every scope by implication, so a scope could not express 'not every admin'. System-key callers pass this check, because break-glass remains the documented last resort.",
    },
];
