mod chunking;
mod controls;
mod embedding;
mod ingestion;
mod provider_url;
mod retrieval;
mod runtime_cache;
mod runtime_factory;

pub use chunking::{ChunkCandidate, ChunkStrategy, ChunkingError, ChunkingLimits, chunk};
pub use controls::{
    CircuitBreakerRegistry, CircuitResetScope, CircuitState, ClusterCoordinator,
    ClusterRateLimiter, ConcurrencyController, ExecutionPermits, InMemoryRateLimiter,
    ProviderRuntimeCache, RateLimiterBackend, RuntimeCacheKey, is_fallback_eligible, is_retryable,
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
pub use provider_url::normalize_openai_base_url;
pub use retrieval::{
    CANDIDATE_OVERFETCH, MAX_CANDIDATE_ROWS, MemoryCandidate, RagChunkCandidate, RetrievalLimits,
    RetrievalWeights, ScoreComponents, Scored, blend, lexical_overlap_score, rank_chunks,
    rank_memories, recency_score, semantic_score,
};
pub use runtime_cache::{AuthProviderSettingsCache, RuntimeConfigCache};
pub use runtime_factory::{
    RigRuntimeFactory, RuntimeCompletionOutput, RuntimeEventSeed, RuntimeFactory,
    RuntimeItemStream, RuntimeModelHandle, RuntimeStreamItem, RuntimeStreamOutput,
    classify_completion_error, rig_chat_history, usage_from_rig,
};
