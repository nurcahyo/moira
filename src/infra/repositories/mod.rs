mod admin;
mod auth_settings;
mod cluster;
mod conversation;
mod identity;
mod public;
mod runtime;
mod setup;

pub use admin::{
    AdminIdempotencyClaim, AdminIdempotencyClaimOutcome, AdminRepository, KeyMaterial,
    PgAdminCommandTransaction, PgAdminRepository, StoredCredentialSecret,
};
// Plan 07 modules 5-6. Both ship as a trait plus one Postgres implementation from their
// first commit, so no later plan has to retrofit the seam onto a surface that already has
// callers — the retrofit P2-3 had to perform for `AdminRepository` and `SetupRepository`.
pub use auth_settings::{
    AuthProviderSettingsRepository, GoverningAuthPolicy, PgAuthProviderSettingsRepository,
};
// Plan 10 wave 1. Same shape as the plan-07 modules above: a trait plus one Postgres
// implementation from the first commit, so the startup gate in `src/app/cluster_lease.rs`
// is testable without a database.
pub use cluster::{
    ClusterLeaseGrant, ClusterLeaseOutcome, ClusterLeaseRepository, PgClusterLeaseRepository,
    is_undefined_table, pod_name, resolve_pod_name,
};
pub use conversation::{
    ConversationAccess, ConversationInsert, ConversationMessageInsert, ConversationRepository,
    MemoryInsert, PgConversationRepository,
};
pub use identity::{
    AdminIdentityGrant, AdminIdentityGrantInsert, AdminIdentityRepository,
    PgAdminIdentityRepository,
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
    IdempotencyClaim, PgPublicRepository, PublicAccess, PublicRepository, ResponseStartedInsert,
    ResponseTerminalUpdate, default_application_execution_policy, idempotency_record,
};
pub use runtime::{
    ExecutionAttemptInsert, ExecutionAttemptUpdate, PgRuntimeRepository,
    RuntimeCredentialCandidate, RuntimeRepository, UsageRecordInsert,
    execution_failure_class_to_db,
};
pub use setup::{PgSetupRepository, SetupReadinessSnapshot, SetupRepository};
// Test-only in-memory fakes (plan 06, Module 8 / P2-3). Exported `pub(crate)` under `cfg(test)`
// so application-layer unit tests can drive a service without Postgres; they are compiled out of
// every shipped binary. Each fake backs one coherent slice of its trait and returns an explicit
// "not stubbed" error elsewhere — never a plausible empty result — so a unit test cannot pass
// while exercising nothing. None of them holds credential material.
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use conversation::InMemoryConversationRepository;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use public::InMemoryPublicRepository;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use runtime::InMemoryRuntimeRepository;
#[cfg(test)]
pub(crate) use setup::InMemorySetupRepository;
