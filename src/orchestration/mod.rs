mod chunking;
mod controls;
mod embedding;
mod ingestion;
mod openapi_import;
mod provider_url;
mod retrieval;
mod runtime_cache;
mod runtime_factory;
mod skill_tool;

pub use chunking::{ChunkCandidate, ChunkStrategy, ChunkingError, ChunkingLimits, chunk};
pub use controls::{
    CapacityExhaustion, CapacityScope, CircuitBreakerRegistry, CircuitResetScope, CircuitState,
    ClusterCoordinator, ClusterRateLimiter, ConcurrencyController, ExecutionPermits,
    InMemoryRateLimiter, ProviderRuntimeCache, RateLimiterBackend, RuntimeCacheKey,
    is_fallback_eligible, is_retryable,
};
pub use embedding::{
    EMBEDDING_PROVIDER_UNSUPPORTED, EMBEDDING_REQUEST_FAILED, EMBEDDING_RESPONSE_INVALID,
    EmbeddingBatchPlan, EmbeddingFactory, EmbeddingModelHandle, RigEmbeddingFactory,
    SUPPORTED_EMBEDDING_DIMENSION, classify_embedding_error, embed_texts, encode_vector_literal,
    provider_type_supports_embeddings, unsupported_provider,
};
pub use ingestion::{
    FAILURE_EMBEDDING_DIMENSION_UNSUPPORTED, FAILURE_EMBEDDING_FAILED,
    FAILURE_EMBEDDING_NOT_CONFIGURED, PreparedChunk, RAG_DOCUMENT_TOO_LARGE, RagIngestionPlan,
    prepare_chunks,
};
pub use openapi_import::{
    MAX_DOCUMENT_BYTES, MAX_IMPORT_OPERATIONS, MAX_OPERATION_SCHEMA_BYTES, MAX_TOTAL_SCHEMA_BYTES,
    OpenApiImportError, ParsedImport, ParsedOperation, parse_openapi_document,
};
pub use provider_url::normalize_openai_base_url;
pub use retrieval::{
    CANDIDATE_OVERFETCH, MAX_CANDIDATE_ROWS, MemoryCandidate, RagChunkCandidate, RetrievalLimits,
    RetrievalWeights, ScoreComponents, Scored, blend, lexical_overlap_score, rank_chunks,
    rank_memories, recency_score, semantic_score,
};
pub use runtime_cache::{AuthProviderSettingsCache, RuntimeConfigCache};
pub use runtime_factory::{
    CHATGPT_SUBSCRIPTION_OPT_IN_REQUIRED, RigRuntimeFactory, RuntimeCompletionOutput,
    RuntimeFactory, RuntimeItemStream, RuntimeModelHandle, RuntimeStreamItem,
    classify_completion_error, require_chatgpt_subscription_opt_in, rig_chat_history,
    usage_from_rig,
};
pub use skill_tool::{
    HttpSkillTool, SkillCallerScope, SkillCredential, SkillOutboundPolicy, SkillToolBuildError,
    SkillToolSpec, ToolCallRecord, ToolLoopContext, ToolLoopFailure, ToolLoopOutcome,
    build_skill_tool_set, run_tool_loop,
};
