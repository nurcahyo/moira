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
        default_message: "The structured output is invalid.",
        description: "Used when a structured-output request cannot be honoured: either the response schema the caller supplied is rejected, or the model's output does not conform to it. The previous wording covered only the second case, so it did not describe the request-validation emitter at all.",
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
];
