mod admin;
mod admin_command;
mod auth_settings;
mod context;
mod context_planner;
mod conversation;
mod execution;
mod identity;
mod memory_extraction;
mod public;
mod runtime_admin;
mod setup;
mod summarization;

pub use admin::AdminService;
pub use admin_command::{
    AdminCommandIdempotency, AdminCommandMutation, AdminCommandOutcome, AdminCommandRunner,
    AdminCommandSpec,
};
pub use auth_settings::AuthProviderSettingsService;
pub use context::RequestContext;
pub use context_planner::{
    AssembledContext, CONTEXT_LENGTH_EXCEEDED, ContextPlanner, ContextSections,
    RETRIEVED_CONTEXT_LABEL, SUMMARY_CONTEXT_LABEL, assemble_context, budget_tokens,
};
pub use conversation::{
    ConversationExecutionLink, ConversationService, PlannedContext, SummarizationOutcome,
};
pub use execution::{ExecutionService, MoiraExecutionService, execute_diagnostic};
pub use identity::{AdminIdentityService, ClaimCredential};
// Plan 11 Sub-Phase F. Everything here is pure decision-making — the consent branch, the policy
// validation, the prompt boundary — kept out of `conversation.rs` so it can be tested without a
// database or a provider, exactly as `context_planner` is.
pub use memory_extraction::{
    EXTRACTION_INSTRUCTION, EXTRACTION_SOURCE_LABEL, EXTRACTION_TRANSCRIPT_MESSAGES,
    ExtractionPolicy, FAILURE_EXTRACTION_CALL_FAILED, FAILURE_STRUCTURED_OUTPUT_INVALID,
    MAXIMUM_CANDIDATES_PER_RUN, NEAR_DUPLICATE_MAX_DISTANCE, RejectionReason, classify_candidate,
    effective_extraction_status, extraction_messages, extraction_output_schema, is_near_duplicate,
    parse_candidates, render_transcript, status_for_consent_mode,
};
// `pub(crate)`, unlike the block above: the needle list is shared with
// `conversation::validate_content` so the caller-supplied and model-supplied memory paths screen
// against one list rather than two copies that can drift. It is not part of any public surface.
pub(crate) use memory_extraction::SECRET_NEEDLES;
pub use public::{ExecutionPipeline, PublicExecutionService};
pub use runtime_admin::RuntimeAdminService;
pub use setup::SetupService;
// Plan 11 Sub-Phase E. Same split as `context_planner` and `memory_extraction`: the trigger
// decision, the prompt boundary and the reply contract are pure, so they are tested with no
// database and no provider.
pub use summarization::{
    FAILURE_SUMMARIZATION_CALL_FAILED, FAILURE_SUMMARY_EMPTY, FAILURE_SUMMARY_TOO_LARGE,
    MAXIMUM_SUMMARY_BYTES, PRIOR_SUMMARY_LABEL, SUMMARIZATION_INSTRUCTION,
    SUMMARY_TRANSCRIPT_LABEL, SUMMARY_TRANSCRIPT_MESSAGES, SummarizationBacklog,
    SummarizationPolicy, SummarizationSkip, ValidatedSummary, decide_summarization, parse_summary,
    summarization_messages,
};
