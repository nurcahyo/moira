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
        key: "moira.error.conflict",
        default_message: "The request could not be completed because it conflicts with existing state.",
        description: "Used when a resource or operation cannot proceed because of a state conflict.",
    },
    I18nEntry {
        key: "moira.error.forbidden",
        default_message: "You do not have permission to perform this action.",
        description: "Used when the caller is authenticated but not authorized.",
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
        default_message: "The selected model does not support this capability.",
        description: "Used when a model cannot satisfy a requested feature.",
    },
    I18nEntry {
        key: "moira.error.model_not_found",
        default_message: "The model was not found.",
        description: "Used when a model is unavailable to the caller.",
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
        description: "Used when the model output does not match the expected schema.",
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
];
