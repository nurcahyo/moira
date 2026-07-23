# Execution Pipeline

Phase 4 keeps one execution engine: `MoiraExecutionService` and the Rig-backed runtime path from Phase 3. The public service wraps it with a thin pipeline:

1. RequestNormalizationInterceptor
2. IdentityBindingInterceptor
3. ExecutionAuthorizationInterceptor
4. InputValidationInterceptor
5. ApplicationPolicyInterceptor
6. RateLimitInterceptor
7. IdempotencyInterceptor
8. ContextBudgetInterceptor
9. ExecutionDispatchInterceptor
10. UsageFinalizationInterceptor
11. AuditInterceptor

Handlers parse headers and JSON, then call `PublicExecutionService`. SQL stays in `PgPublicRepository`; provider execution stays in orchestration.

