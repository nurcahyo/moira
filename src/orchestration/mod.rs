mod controls;
mod executor;
mod resolver;
mod runtime_factory;

pub use controls::{
    CircuitBreakerRegistry, CircuitState, ConcurrencyController, ExecutionPermits,
    InMemoryRateLimiter, ProviderRuntimeCache, RuntimeCacheKey, is_fallback_eligible, is_retryable,
};
pub use executor::{execute_chat, stream_chat};
pub use resolver::{
    CredentialCandidate, ResolvedProvider, RuntimeConfigCache, credential_aad, credential_priority,
    get_provider, normalize_openai_base_url, resolve_provider,
};
pub use runtime_factory::{
    RigRuntimeFactory, RuntimeCompletionOutput, RuntimeEventSeed, RuntimeFactory,
    RuntimeItemStream, RuntimeModelHandle, RuntimeStreamItem, RuntimeStreamOutput,
    classify_completion_error, rig_chat_history, usage_from_rig,
};
