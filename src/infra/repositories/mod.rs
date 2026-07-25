mod admin;
mod conversation;
mod public;
mod runtime;
mod setup;

pub use admin::{
    AdminIdempotencyClaim, AdminIdempotencyClaimOutcome, AdminRepository, KeyMaterial,
    PgAdminCommandTransaction, PgAdminRepository, StoredCredentialSecret,
};
pub use conversation::{
    ConversationAccess, ConversationInsert, ConversationMessageInsert, MemoryInsert,
    PgConversationRepository,
};
// Deliberately `pub(crate)`, not `pub`: these three free functions mutate RAG state with no
// authorization check, no audit row and no idempotency envelope of their own — those are
// supplied by their only legitimate caller, `crate::application::conversation`, which wraps
// them in the admin-command runner. Exporting them from `moira::infra::repositories` would
// publish an unauthenticated, unaudited write path around that envelope.
pub(crate) use conversation::{
    create_rag_collection_with_connection, create_rag_document_with_connection,
    ingest_rag_document_with_connection,
};
pub use public::{
    IdempotencyClaim, PgPublicRepository, PublicAccess, ResponseStartedInsert,
    ResponseTerminalUpdate, default_application_execution_policy, idempotency_record,
};
pub use runtime::{
    ExecutionAttemptInsert, ExecutionAttemptUpdate, PgRuntimeRepository,
    RuntimeCredentialCandidate, UsageRecordInsert, execution_failure_class_to_db,
};
pub use setup::{PgSetupRepository, SetupReadinessSnapshot};
