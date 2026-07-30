mod admin;
mod auth_settings;
mod conversation;
mod health;
mod identity;
mod observability;
mod openapi;
mod public;

use std::{sync::Arc, time::Duration};

use axum::{Extension, Router, extract::DefaultBodyLimit, http::StatusCode};
use tower_http::timeout::{RequestBodyTimeoutLayer, TimeoutLayer};
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{app::AppState, config::Settings};

// `test-routes` adds two unauthenticated probe routes (`/internal/test/panic`,
// `/internal/test/slow`) whose entire purpose is to panic on demand and to park a
// connection. They must never exist in a shipped binary.
//
// The Cargo feature alone does not guarantee that: `--all-features` is a different
// feature set from the default one, and this repository's own CI already uses
// `--all-features` (clippy and test). `cargo build --release --all-features` would
// therefore have produced a binary containing both probes plus the panic payload
// string. Refusing to compile is the only guarantee that holds regardless of which
// feature flags a build uses, so the gate is `debug_assertions`, not the feature.
//
// CI is unaffected: `cargo clippy --all-targets --all-features` and
// `cargo test --all-features` both build with `debug_assertions` on. A *release* build
// that adds `--all-features` will now fail to compile — that is the point.
//
// Regular comments, not doc comments: rustc emits `unused doc comment` for a doc comment
// attached to a macro invocation.
#[cfg(all(feature = "test-routes", not(debug_assertions)))]
compile_error!(
    "the `test-routes` feature exposes unauthenticated panic and connection-parking \
     probe routes and cannot be compiled with `debug_assertions` off. Drop \
     `--all-features`/`--features test-routes` from optimized builds, or build the \
     probes under the dev profile."
);

/// Fixed body limit for the conversation / memory / RAG surface.
///
/// Plan 03 (P1-3) deliberately does **not** introduce a new configuration field for
/// this surface: the value simply matches the effective default these routes already
/// had (`PublicApiSettings.maximum_request_bytes`'s 1 MiB default), so no legitimate
/// payload that works today can start failing. Making it configurable is an explicit
/// deferred follow-up.
pub const CONVERSATION_BODY_LIMIT_BYTES: usize = 1024 * 1024;

/// Body limit for the `/api/v1/admin/*` surface.
///
/// Admin payloads are structurally larger than public ones — routing policies, agent
/// profile prompts, provider runtime policies and RAG document bodies are all
/// operator-authored JSON documents rather than a single prompt — so the admin cap is
/// set above the public one. 2 MiB matches Axum's own built-in default body limit, so
/// this is a ceiling that already applied to every unlayered Axum route, not a novel
/// number. The largest admin request body in the existing test corpus is well under
/// 1 KiB (the largest generated payload anywhere in `tests/` is a 128-character
/// repeat), so this cannot regress a known-legitimate payload.
///
/// The rationale above is only true if every `/api/v1/admin/*` path actually receives
/// this limit. `RagDocumentCreateRequest`/`RagDocumentIngestRequest` carry the document
/// text inline (`content: Option<String>`), and those routes used to sit in
/// [`conversation_routes`], which meant the RAG document bodies the rationale names
/// were in fact capped at [`CONVERSATION_BODY_LIMIT_BYTES`]. They now live in
/// [`admin_conversation_routes`], which is layered with this constant — see that
/// function for the full reasoning.
///
/// **Needs ops sign-off**: 2 MiB is a judgement call, not a measured requirement.
pub const ADMIN_BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Hard ceiling on `PublicApiSettings.maximum_request_bytes`.
///
/// `maximum_request_bytes` is an `i64` and `Settings::validate` places no upper bound on
/// it, so `MOIRA_PUBLIC_API__MAXIMUM_REQUEST_BYTES=9223372036854775807` used to pass
/// straight into `DefaultBodyLimit::max` and yield an effectively unlimited public body
/// limit — every accepted byte is buffered in memory by the `Json` extractor, so an
/// unbounded limit is a memory-exhaustion switch disguised as a tuning knob.
///
/// 16 MiB is 16x the 1 MiB default and 8x the admin cap: far above any legitimate
/// prompt payload, far below "unbounded". Exceeding it is clamped, not rejected, and is
/// reported through a `warn`-level log at router-construction time so an operator who
/// genuinely wants more sees why they did not get it.
///
/// **Needs ops sign-off**: 16 MiB is a judgement call, not a measured requirement. The
/// real fix is a bound inside `Settings::validate` (`src/config/settings.rs`), which is
/// outside this module's ownership; this clamp is the enforcement-side backstop.
pub const MAX_PUBLIC_BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Inactivity ceiling applied to **request body ingestion** on every route group.
///
/// `TimeoutLayer` bounds the response-head future, which for a body-consuming handler
/// includes body ingestion — but the SSE group cannot carry a tight head timeout, and
/// even the non-SSE ceiling (`maximum_execution_timeout_seconds + 30`, ≥ 630 s by
/// default) is far too generous to be the only bound on how long a client may take to
/// send its body. Without an ingestion bound, a client that writes request headers with
/// a `Content-Length` and then never sends the body pins a connection task and a file
/// descriptor for that entire window, before any authentication runs.
///
/// `RequestBodyTimeoutLayer` bounds the gap *between* body frames and resets on every
/// frame, so it never penalises a slow-but-progressing upload and never touches the
/// response body (which is what would break SSE). 30 s of complete silence mid-body is
/// already far outside normal client behaviour.
///
/// Residual, accepted: because the timer resets per frame, a client that drips one byte
/// every 29 s still holds a connection, bounded by `DefaultBodyLimit` in bytes but not
/// in time. Closing that fully needs a total-ingestion budget, which tower-http does not
/// provide; it is recorded as a deferred follow-up rather than hand-rolled here.
pub const REQUEST_BODY_IDLE_TIMEOUT_SECONDS: u64 = 30;

/// Headroom added on top of `RuntimeSettings.maximum_execution_timeout_seconds` when
/// sizing the HTTP-level timeout, so the HTTP deadline always sits *above* the
/// execution deadline and an execution timeout surfaces as its own specific error
/// rather than as a generic transport timeout.
///
/// **Needs product/ops sign-off**: 30 s is the plan's proposed buffer, not a derived
/// value.
pub const NON_STREAMING_TIMEOUT_BUFFER_SECONDS: u64 = 30;

/// Per-route-group request policy (plan 03 / P1-3).
///
/// Replaces the single global `DefaultBodyLimit::max(512 * 1024)` that used to be
/// applied in `build_router` and that was disconnected from
/// `PublicApiSettings.maximum_request_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterPolicy {
    /// Applied to `/api/v1/responses*` and `/v1/responses`; sourced from
    /// `PublicApiSettings.maximum_request_bytes`.
    pub public_body_limit_bytes: usize,
    /// Applied to the conversation / memory / RAG surface.
    pub conversation_body_limit_bytes: usize,
    /// Applied to `/api/v1/admin/*`.
    pub admin_body_limit_bytes: usize,
    /// Applied to every route group that can only ever return a buffered response.
    pub non_streaming_timeout: Duration,
    /// Applied to the group that *may* return `text/event-stream`.
    ///
    /// This bounds the response-**head** future only. `tower_http`'s `Timeout` drops its
    /// sleep as soon as the inner service resolves to a `Response`, so an SSE body that
    /// has already been handed back streams for as long as it likes (asserted by
    /// `a_timeout_layer_never_caps_an_already_returned_streaming_body` in `src/lib.rs`).
    /// Kept as a distinct field from [`Self::non_streaming_timeout`] precisely so a test
    /// can drive the non-streaming ceiling down to prove the exemption without also
    /// capping the stream's setup phase.
    pub streaming_head_timeout: Duration,
    /// Applied to every route group as a `RequestBodyTimeoutLayer`: the maximum gap
    /// between two request-body frames. See [`REQUEST_BODY_IDLE_TIMEOUT_SECONDS`].
    pub request_body_idle_timeout: Duration,
}

impl RouterPolicy {
    pub fn from_settings(settings: &Settings) -> Self {
        let non_streaming_timeout = Duration::from_secs(
            settings
                .runtime
                .maximum_execution_timeout_seconds
                .saturating_add(NON_STREAMING_TIMEOUT_BUFFER_SECONDS),
        );
        Self {
            public_body_limit_bytes: public_body_limit_bytes(
                settings.public_api.maximum_request_bytes,
            ),
            conversation_body_limit_bytes: CONVERSATION_BODY_LIMIT_BYTES,
            admin_body_limit_bytes: ADMIN_BODY_LIMIT_BYTES,
            non_streaming_timeout,
            // The SSE group's head phase (auth, policy, ledger writes, opening the
            // upstream stream) is itself bounded by the execution deadline, so the same
            // ceiling is the right one; only the *body* must escape it.
            streaming_head_timeout: non_streaming_timeout,
            request_body_idle_timeout: Duration::from_secs(REQUEST_BODY_IDLE_TIMEOUT_SECONDS),
        }
    }
}

/// Resolves the configured public body limit into an enforceable one.
///
/// `maximum_request_bytes` is an unbounded `i64`, so three cases have to be handled:
/// non-positive, larger than `usize`, and merely absurd. All three are clamped rather
/// than rejected — refusing to build the router over a tuning knob would turn a
/// misconfiguration into an outage — and all three warn.
fn public_body_limit_bytes(configured: i64) -> usize {
    let Some(bytes) = usize::try_from(configured).ok().filter(|bytes| *bytes > 0) else {
        tracing::warn!(
            configured,
            applied = CONVERSATION_BODY_LIMIT_BYTES,
            "public_api.maximum_request_bytes is not a usable positive byte count; \
             applying the default public body limit"
        );
        return CONVERSATION_BODY_LIMIT_BYTES;
    };

    if bytes > MAX_PUBLIC_BODY_LIMIT_BYTES {
        tracing::warn!(
            configured = bytes,
            applied = MAX_PUBLIC_BODY_LIMIT_BYTES,
            "public_api.maximum_request_bytes exceeds the enforceable ceiling; clamping. \
             Request bodies are buffered in memory, so an unbounded limit is a \
             memory-exhaustion vector"
        );
        return MAX_PUBLIC_BODY_LIMIT_BYTES;
    }

    bytes
}

impl Default for RouterPolicy {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

pub fn router(policy: RouterPolicy) -> Router<AppState> {
    let (router, openapi) = documented_router(policy).split_for_parts();
    #[cfg(feature = "test-routes")]
    let router = router.merge(middleware_probe_routes());
    router.layer(Extension(Arc::new(openapi)))
}

/// The timeout applied to the `test-routes` slow probe.
///
/// Deliberately tiny and deliberately *not* [`RouterPolicy::non_streaming_timeout`]: the
/// production value is `maximum_execution_timeout_seconds + 30`, i.e. never below 30 s,
/// and it always sits above every execution deadline by construction — so no production
/// route can be made to reach it inside a test. The production value and its placement
/// are asserted by `the_non_streaming_timeout_sits_above_the_execution_deadline`
/// (`src/lib.rs`); this probe exists only so an integration test can observe what the
/// *envelope mapper* does to a real `TimeoutLayer` rejection over a real socket.
#[cfg(feature = "test-routes")]
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Undocumented probe routes, compiled only under the off-by-default `test-routes`
/// feature (plan 03 / P1-3, Wave 3).
///
/// They are merged **after** `split_for_parts`, so they contribute nothing to the
/// OpenAPI document and cannot perturb the zero-diff spec requirement. `cargo build
/// --release --locked` does not enable the feature, so they cannot exist in a shipped
/// binary.
///
/// Why they are needed: `tests/*.rs` links `moira` compiled *without* `cfg(test)`, so a
/// `#[cfg(test)]` route is unreachable from an integration test, and neither
/// `CatchPanicLayer` nor the 504 branch of `normalize_infrastructure_error` is
/// reachable from any production route inside a test (nothing panics on demand, and the
/// production HTTP timeout is always above the execution deadline).
#[cfg(feature = "test-routes")]
fn middleware_probe_routes() -> Router<AppState> {
    use axum::routing::get;

    Router::new()
        .route("/internal/test/panic", get(probe_panic))
        .merge(
            Router::new()
                .route("/internal/test/slow", get(probe_slow))
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::GATEWAY_TIMEOUT,
                    PROBE_TIMEOUT,
                )),
        )
}

/// Panics with a payload built from fragments the test asserts never reach the client.
#[cfg(feature = "test-routes")]
async fn probe_panic() -> StatusCode {
    panic!("moira probe panic: credential row 42 pepper v1 unwrap on None");
}

/// Never completes, so the probe timeout above always wins. No `sleep`.
#[cfg(feature = "test-routes")]
async fn probe_slow() -> StatusCode {
    std::future::pending::<()>().await;
    StatusCode::OK
}

fn documented_router(policy: RouterPolicy) -> OpenApiRouter<AppState> {
    // `TimeoutLayer` bounds the time taken to *produce a response head*. `tower_http`'s
    // `Timeout::ResponseFuture` drops its sleep the moment the inner service resolves to
    // a `Response`, so a streaming body that has already been handed back is never
    // capped by it — which is exactly why the SSE group can carry one too.
    //
    // The SSE group used to be left entirely unlayered on the reasoning that "the SSE
    // group must not be capped". That reasoning conflated two different bounds: leaving
    // the group unlayered removed the bound on *request ingestion* as well, so a client
    // could write `POST /api/v1/responses/stream` headers with a `Content-Length` and
    // never send the body, pinning a connection task and a file descriptor forever —
    // before `public_actor` (which runs inside the handler, after `Json` extraction) ever
    // asks for a credential. It also left `/v1/responses` with `stream: false`, a fully
    // non-streaming JSON route that happens to share a path with the streaming one,
    // without any timeout at all.
    //
    // Two bounds now cover the group, neither of which can touch a response body:
    //   * `streaming_head_timeout` — head production only (see `RouterPolicy`).
    //   * `RequestBodyTimeoutLayer` — the gap between request-body frames.
    let timeout =
        TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, policy.non_streaming_timeout);
    let streaming_timeout =
        TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, policy.streaming_head_timeout);
    // Applied to every group: a stalled request body must never be able to park a
    // connection, whichever route it was addressed to.
    let body_timeout = RequestBodyTimeoutLayer::new(policy.request_body_idle_timeout);

    let mut router = OpenApiRouter::with_openapi(openapi::MoiraApiDoc::openapi())
        .merge(
            operational_routes()
                .layer(timeout)
                .layer(body_timeout.clone()),
        )
        .merge(
            public_execution_routes()
                .layer(DefaultBodyLimit::max(policy.public_body_limit_bytes))
                .layer(timeout)
                .layer(body_timeout.clone()),
        )
        .merge(
            streaming_routes()
                .layer(DefaultBodyLimit::max(policy.public_body_limit_bytes))
                .layer(streaming_timeout)
                .layer(body_timeout.clone()),
        )
        .merge(
            conversation_routes()
                .layer(DefaultBodyLimit::max(policy.conversation_body_limit_bytes))
                .layer(timeout)
                .layer(body_timeout.clone()),
        )
        .merge(
            admin_routes()
                .merge(admin_conversation_routes())
                .layer(DefaultBodyLimit::max(policy.admin_body_limit_bytes))
                .layer(timeout)
                .layer(body_timeout),
        );
    openapi::finalize_document(router.get_openapi_mut());
    router
}

fn operational_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health::healthz))
        .routes(routes!(health::readyz))
        .routes(routes!(observability::metrics))
        .routes(routes!(openapi::openapi_json))
        .routes(routes!(openapi::docs))
}

fn public_execution_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(public::create_response))
        .routes(routes!(public::get_response))
        .routes(routes!(public::list_executions))
        .routes(routes!(public::get_execution))
        .routes(routes!(public::list_usage))
        .routes(routes!(public::list_models))
        .routes(routes!(public::list_routes))
        .routes(routes!(public::capabilities))
}

/// Every route that can emit `text/event-stream`.
///
/// `/api/v1/responses/stream` always streams; `/v1/responses` streams whenever the
/// caller sets `stream: true` (`src/http/public.rs` — the OpenAI compatibility route
/// documents both `application/json` and `text/event-stream` for its 200 response).
fn streaming_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(public::stream_response))
        .routes(routes!(public::openai_responses_compat))
}

/// The caller-facing conversation / memory surface (`/api/v1/conversations*`,
/// `/api/v1/memories*`), layered with [`CONVERSATION_BODY_LIMIT_BYTES`].
fn conversation_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            conversation::list_conversations,
            conversation::create_conversation
        ))
        .routes(routes!(
            conversation::get_conversation,
            conversation::patch_conversation,
            conversation::delete_conversation
        ))
        .routes(routes!(conversation::archive_conversation))
        .routes(routes!(conversation::restore_conversation))
        .routes(routes!(
            conversation::list_messages,
            conversation::create_message
        ))
        .routes(routes!(
            conversation::list_memories,
            conversation::create_memory
        ))
        .routes(routes!(
            conversation::get_memory,
            conversation::patch_memory,
            conversation::delete_memory
        ))
}

/// The `/api/v1/admin/*` routes that happen to be *implemented* in `http::conversation`.
///
/// They are split out of [`conversation_routes`] so that the layering invariant is
/// simply stated and holds without exception: **every `/api/v1/admin/*` path is layered
/// with [`ADMIN_BODY_LIMIT_BYTES`]**. Previously these paths inherited
/// [`CONVERSATION_BODY_LIMIT_BYTES`] purely because of which Rust module their handlers
/// lived in, which contradicted `ADMIN_BODY_LIMIT_BYTES`'s own stated rationale — that
/// rationale cites "RAG document bodies" as a reason for the larger cap, while the RAG
/// document routes were the ones getting the smaller one. Measured before the move: a
/// 1536 KiB body got 400 on `/api/v1/admin/applications` (admitted) but 413 on
/// `/api/v1/admin/rag-collections`, `.../rag-collections/{id}/documents` and
/// `.../rag-documents/{id}/ingest`.
///
/// Moving rather than rewriting the rationale is the deliberate choice, because the
/// rationale is correct: `RagDocumentCreateRequest` and `RagDocumentIngestRequest` carry
/// the document text inline as `content: Option<String>` (`src/domain/conversation.rs`),
/// so these are the only Moira routes whose body size is bounded by a *document* rather
/// than by a prompt or a policy object. Capping an operator-supplied document at half
/// the cap granted to an application-create payload is not defensible. The four
/// application-scoped `*-policy` routes move with them only because they share the
/// `/api/v1/admin/` prefix and a one-sentence invariant is worth more than a 1 MiB
/// distinction on fixed-shape policy objects that are three orders of magnitude smaller.
///
/// This is a layering change only: no path, method, operation id, schema or security
/// requirement moves with it, so the OpenAPI document is unchanged (utoipa's `PathsMap`
/// is a `BTreeMap` — `preserve_path_order` is not enabled — so even serialisation order
/// is unaffected by which group registered a path).
fn admin_conversation_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            conversation::get_conversation_policy,
            conversation::put_conversation_policy
        ))
        .routes(routes!(
            conversation::get_memory_policy,
            conversation::put_memory_policy
        ))
        .routes(routes!(
            conversation::get_retrieval_policy,
            conversation::put_retrieval_policy
        ))
        .routes(routes!(
            conversation::get_embedding_policy,
            conversation::put_embedding_policy
        ))
        .routes(routes!(
            conversation::list_rag_collections,
            conversation::create_rag_collection
        ))
        .routes(routes!(
            conversation::get_rag_collection,
            conversation::patch_rag_collection,
            conversation::delete_rag_collection
        ))
        .routes(routes!(conversation::enable_rag_collection))
        .routes(routes!(conversation::disable_rag_collection))
        .routes(routes!(
            conversation::list_rag_documents,
            conversation::create_rag_document
        ))
        .routes(routes!(
            conversation::get_rag_document,
            conversation::delete_rag_document
        ))
        .routes(routes!(conversation::ingest_rag_document))
        .routes(routes!(conversation::reindex_rag_document))
}

fn admin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(admin::get_setup_status))
        // Plan 07's identity surface registers **here**, in `admin_routes`, not in
        // `documented_router`. `documented_router` only merges route groups and applies
        // layers, so a route registered directly on it would sit outside the admin body
        // limit and the admin timeout — including `POST /api/v1/admin/setup/claim`, which
        // accepts an operator-authored body.
        //
        // `get_setup_claim_status` is intentionally unauthenticated and is registered
        // alongside the rest anyway: the admin prefix is what keeps it inside
        // `public_document`'s admin strip, and no router-level auth middleware exists on
        // `/api/v1/admin` for it to need an exemption from — every handler self-
        // authenticates in its own body.
        .routes(routes!(identity::get_setup_claim_status))
        .routes(routes!(identity::claim_admin_identity))
        .routes(routes!(auth_settings::get_setup_auth_methods))
        // Also intentionally unauthenticated (finding F15), and registered here for the same
        // reason as `get_setup_claim_status`: the admin prefix is what keeps it inside the
        // admin strip, the admin body limit and the admin timeout. Moving it to a
        // non-admin path to "make it visible in the public spec" would drop all three.
        .routes(routes!(auth_settings::get_setup_sign_in_methods))
        .routes(routes!(
            auth_settings::list_auth_providers,
            auth_settings::create_auth_provider
        ))
        .routes(routes!(
            auth_settings::get_auth_provider,
            auth_settings::patch_auth_provider,
            auth_settings::delete_auth_provider
        ))
        .routes(routes!(auth_settings::enable_auth_provider))
        .routes(routes!(auth_settings::disable_auth_provider))
        .routes(routes!(admin::list_applications, admin::create_application))
        .routes(routes!(
            admin::get_application,
            admin::patch_application,
            admin::delete_application
        ))
        .routes(routes!(admin::enable_application))
        .routes(routes!(admin::disable_application))
        .routes(routes!(
            admin::get_application_execution_policy,
            admin::put_application_execution_policy
        ))
        .routes(routes!(admin::list_providers, admin::create_provider))
        .routes(routes!(
            admin::get_provider,
            admin::patch_provider,
            admin::delete_provider
        ))
        .routes(routes!(admin::enable_provider))
        .routes(routes!(admin::disable_provider))
        .routes(routes!(
            admin::get_provider_runtime_policy,
            admin::put_provider_runtime_policy
        ))
        .routes(routes!(
            admin::list_provider_models,
            admin::create_provider_model
        ))
        .routes(routes!(
            admin::list_route_definitions,
            admin::create_route_definition
        ))
        .routes(routes!(
            admin::get_route_definition,
            admin::patch_route_definition,
            admin::delete_route_definition
        ))
        .routes(routes!(admin::enable_route_definition))
        .routes(routes!(admin::disable_route_definition))
        .routes(routes!(
            admin::list_routing_policies,
            admin::create_routing_policy
        ))
        .routes(routes!(
            admin::get_routing_policy,
            admin::patch_routing_policy,
            admin::delete_routing_policy
        ))
        .routes(routes!(admin::enable_routing_policy))
        .routes(routes!(admin::disable_routing_policy))
        .routes(routes!(
            admin::list_agent_profiles,
            admin::create_agent_profile
        ))
        .routes(routes!(
            admin::get_agent_profile,
            admin::patch_agent_profile,
            admin::delete_agent_profile
        ))
        .routes(routes!(admin::enable_agent_profile))
        .routes(routes!(admin::disable_agent_profile))
        .routes(routes!(admin::diagnose_runtime))
        .routes(routes!(
            admin::get_provider_model,
            admin::patch_provider_model,
            admin::delete_provider_model
        ))
        .routes(routes!(admin::enable_provider_model))
        .routes(routes!(admin::disable_provider_model))
        .routes(routes!(admin::list_credentials, admin::create_credential))
        .routes(routes!(
            admin::get_credential,
            admin::patch_credential,
            admin::delete_credential
        ))
        .routes(routes!(admin::rotate_credential))
        .routes(routes!(admin::enable_credential))
        .routes(routes!(admin::disable_credential))
        .routes(routes!(
            admin::upsert_user_credential,
            admin::delete_user_credential
        ))
        .routes(routes!(admin::list_user_credentials))
        .routes(routes!(admin::list_system_keys, admin::create_system_key))
        .routes(routes!(admin::get_system_key, admin::delete_system_key))
        .routes(routes!(admin::rotate_system_key))
        .routes(routes!(admin::revoke_system_key))
        .routes(routes!(
            admin::list_consumer_keys,
            admin::create_consumer_key
        ))
        .routes(routes!(admin::get_consumer_key, admin::delete_consumer_key))
        .routes(routes!(admin::rotate_consumer_key))
        .routes(routes!(admin::revoke_consumer_key))
        .routes(routes!(
            admin::list_trusted_jwt_issuers,
            admin::create_trusted_jwt_issuer
        ))
        .routes(routes!(
            admin::get_trusted_jwt_issuer,
            admin::patch_trusted_jwt_issuer,
            admin::delete_trusted_jwt_issuer
        ))
        .routes(routes!(admin::refresh_trusted_jwt_issuer))
        .routes(routes!(admin::enable_trusted_jwt_issuer))
        .routes(routes!(admin::disable_trusted_jwt_issuer))
        .routes(routes!(admin::list_audit_events))
        .routes(routes!(admin::get_audit_event))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};

    use serde_json::Value;

    use super::*;

    const HTTP_METHODS: [&str; 8] = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];

    #[test]
    fn generated_openapi_covers_every_registered_route() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let paths = value["paths"].as_object().expect("OpenAPI paths");
        let expected: BTreeSet<_> = [
            "/health/live",
            "/health/ready",
            "/metrics",
            "/openapi.json",
            "/docs",
            "/api/v1/responses",
            "/api/v1/responses/stream",
            "/api/v1/responses/{response_id}",
            "/api/v1/executions",
            "/api/v1/executions/{execution_id}",
            "/api/v1/usage",
            "/api/v1/models",
            "/api/v1/routes",
            "/api/v1/capabilities",
            "/api/v1/conversations",
            "/api/v1/conversations/{id}",
            "/api/v1/conversations/{id}/archive",
            "/api/v1/conversations/{id}/restore",
            "/api/v1/conversations/{id}/messages",
            "/api/v1/memories",
            "/api/v1/memories/{id}",
            "/v1/responses",
            "/api/v1/admin/setup/status",
            "/api/v1/admin/setup/claim-status",
            "/api/v1/admin/setup/claim",
            "/api/v1/admin/setup/auth-methods",
            "/api/v1/admin/setup/sign-in-methods",
            "/api/v1/admin/auth/providers",
            "/api/v1/admin/auth/providers/{id}",
            "/api/v1/admin/auth/providers/{id}/enable",
            "/api/v1/admin/auth/providers/{id}/disable",
            "/api/v1/admin/applications",
            "/api/v1/admin/applications/{id}",
            "/api/v1/admin/applications/{id}/enable",
            "/api/v1/admin/applications/{id}/disable",
            "/api/v1/admin/applications/{id}/execution-policy",
            "/api/v1/admin/applications/{application_id}/conversation-policy",
            "/api/v1/admin/applications/{application_id}/memory-policy",
            "/api/v1/admin/applications/{application_id}/retrieval-policy",
            "/api/v1/admin/applications/{application_id}/embedding-policy",
            "/api/v1/admin/providers",
            "/api/v1/admin/providers/{id}",
            "/api/v1/admin/providers/{id}/enable",
            "/api/v1/admin/providers/{id}/disable",
            "/api/v1/admin/providers/{provider_id}/runtime-policy",
            "/api/v1/admin/providers/{provider_id}/models",
            "/api/v1/admin/routes",
            "/api/v1/admin/routes/{id}",
            "/api/v1/admin/routes/{id}/enable",
            "/api/v1/admin/routes/{id}/disable",
            "/api/v1/admin/routing-policies",
            "/api/v1/admin/routing-policies/{id}",
            "/api/v1/admin/routing-policies/{id}/enable",
            "/api/v1/admin/routing-policies/{id}/disable",
            "/api/v1/admin/agent-profiles",
            "/api/v1/admin/agent-profiles/{id}",
            "/api/v1/admin/agent-profiles/{id}/enable",
            "/api/v1/admin/agent-profiles/{id}/disable",
            "/api/v1/admin/runtime/diagnose",
            "/api/v1/admin/rag-collections",
            "/api/v1/admin/rag-collections/{id}",
            "/api/v1/admin/rag-collections/{id}/enable",
            "/api/v1/admin/rag-collections/{id}/disable",
            "/api/v1/admin/rag-collections/{collection_id}/documents",
            "/api/v1/admin/rag-documents/{id}",
            "/api/v1/admin/rag-documents/{id}/ingest",
            "/api/v1/admin/rag-documents/{id}/reindex",
            "/api/v1/admin/provider-models/{id}",
            "/api/v1/admin/provider-models/{id}/enable",
            "/api/v1/admin/provider-models/{id}/disable",
            "/api/v1/admin/provider-credentials",
            "/api/v1/admin/provider-credentials/{id}",
            "/api/v1/admin/provider-credentials/{id}/rotate",
            "/api/v1/admin/provider-credentials/{id}/enable",
            "/api/v1/admin/provider-credentials/{id}/disable",
            "/api/v1/admin/users/{external_user_id}/provider-credentials/{id}",
            "/api/v1/admin/users/{external_user_id}/provider-credentials",
            "/api/v1/admin/system-keys",
            "/api/v1/admin/system-keys/{id}",
            "/api/v1/admin/system-keys/{id}/rotate",
            "/api/v1/admin/system-keys/{id}/revoke",
            "/api/v1/admin/consumer-keys",
            "/api/v1/admin/consumer-keys/{id}",
            "/api/v1/admin/consumer-keys/{id}/rotate",
            "/api/v1/admin/consumer-keys/{id}/revoke",
            "/api/v1/admin/jwt-issuers",
            "/api/v1/admin/jwt-issuers/{id}",
            "/api/v1/admin/jwt-issuers/{id}/refresh-jwks",
            "/api/v1/admin/jwt-issuers/{id}/enable",
            "/api/v1/admin/jwt-issuers/{id}/disable",
            "/api/v1/admin/audit-events",
            "/api/v1/admin/audit-events/{id}",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<_> = paths.keys().map(String::as_str).collect();
        assert_eq!(actual, expected);

        let mut operation_ids = HashSet::new();
        let mut operation_count = 0;
        for item in paths.values() {
            for method in HTTP_METHODS {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                operation_count += 1;
                let operation_id = operation["operationId"].as_str().expect("operation id");
                assert!(
                    operation_ids.insert(operation_id),
                    "duplicate operation id: {operation_id}"
                );
            }
        }
        // 141 + the anonymous `GET /api/v1/admin/setup/sign-in-methods` (finding F15).
        assert_eq!(operation_count, 142);
    }

    #[test]
    fn public_document_filters_admin_paths_and_keeps_operational_paths() {
        let document =
            openapi::public_document(documented_router(RouterPolicy::default()).into_openapi());
        assert!(
            document
                .paths
                .paths
                .keys()
                .all(|path| !path.starts_with("/api/v1/admin/"))
        );
        for path in [
            "/health/live",
            "/metrics",
            "/openapi.json",
            "/docs",
            "/api/v1/responses",
            "/api/v1/conversations",
            "/api/v1/memories",
            "/v1/responses",
        ] {
            assert!(document.paths.paths.contains_key(path), "missing {path}");
        }
    }

    #[test]
    fn generated_openapi_contains_security_content_types_and_parameters() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        assert_eq!(value["openapi"], "3.1.0");

        let schemes = value["components"]["securitySchemes"]
            .as_object()
            .expect("security schemes");
        for name in ["bearerAuth", "systemKeyAuth", "consumerKeyAuth"] {
            assert!(schemes.contains_key(name), "missing {name}");
        }

        assert!(value["paths"]["/api/v1/responses/stream"]["post"]["responses"]["200"]["content"]
            ["text/event-stream"]
            .is_object());
        assert!(
            value["paths"]["/metrics"]["get"]["responses"]["200"]["content"]["text/plain"]
                .is_object()
        );
        assert!(parameter_named(
            &value["paths"]["/api/v1/executions"]["get"],
            "limit"
        ));
        assert!(parameter_named(
            &value["paths"]["/api/v1/admin/applications/{id}"]["patch"],
            "If-Match"
        ));
        assert!(parameter_named(
            &value["paths"]["/api/v1/responses"]["post"],
            "Idempotency-Key"
        ));
        assert!(
            value["paths"]["/api/v1/admin/applications"]["post"]["responses"]
                .as_object()
                .unwrap()
                .contains_key("201")
        );
        assert!(
            value["paths"]["/api/v1/admin/applications/{id}"]["delete"]["responses"]
                .as_object()
                .unwrap()
                .contains_key("204")
        );

        let upsert = &value["paths"]["/api/v1/admin/users/{external_user_id}/provider-credentials/{id}"]
            ["put"];
        assert!(upsert["responses"].as_object().unwrap().contains_key("201"));
        assert!(!parameter_named(upsert, "If-Match"));
        assert_eq!(
            upsert["parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|parameter| parameter["name"] == "id")
                .unwrap()["description"],
            "Provider identifier"
        );
    }

    #[test]
    fn every_operation_documents_request_ids_and_protected_operations_document_auth() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let paths = value["paths"].as_object().expect("OpenAPI paths");
        let public_operations = [
            ("/health/live", "get"),
            ("/health/ready", "get"),
            ("/metrics", "get"),
            ("/openapi.json", "get"),
            ("/docs", "get"),
            // Deliberately unauthenticated (plan 07 module 11): its entire response is
            // `{"claimed": bool}` — one bit an attacker could infer anyway from the fact
            // that the instance is freshly deployed, and one a setup wizard genuinely
            // needs before any human holds a credential. Its sibling
            // `/api/v1/admin/setup/auth-methods` stays authenticated because *its*
            // response is identity configuration; the dividing line is information
            // content, not endpoint category. Anyone adding a second unauthenticated
            // admin operation to this list must first explain why that line has moved.
            ("/api/v1/admin/setup/claim-status", "get"),
            // The second one, added for finding F15. **The line has not moved — this
            // operation was built to sit on the existing side of it.**
            //
            // The problem: a console cannot render a sign-in button without knowing which
            // methods a deployment offers, and every read of that was gated behind a
            // bearer JWT — the credential signing in *produces*. Circular. Plan 09's
            // public `/invite/[token]` page makes it blocking (its visitor is
            // unauthenticated by construction), and an operator who removes
            // `MOIRA_SYSTEM_KEY` after setup, the normal fate of a bootstrap credential,
            // is locked out.
            //
            // Serving `/setup/auth-methods` anonymously was considered and **rejected**:
            // its `PublicAuthMethod` carries `allowed_email_domains`, which is not
            // configuration a login screen needs but the deny-by-default admin-claim
            // policy (plan 07 D3) — an anonymous caller would receive the exact list of
            // email domains that can obtain Moira admin. So this is a *narrower* new
            // operation rather than a relaxed old one, returning `PublicSignInMethod`,
            // which drops the policy entirely.
            //
            // By the information-content test the comment above demands: every field it
            // returns is one the browser already transmits or receives during the sign-in
            // it is about to start — `client_id`, `issuer`, `authorization_url` and
            // `requested_scopes` all appear in the OAuth authorization URL the browser is
            // redirected to. An anonymous caller learns nothing it could not learn by
            // clicking the button, which is precisely the standard `claim-status` set.
            ("/api/v1/admin/setup/sign-in-methods", "get"),
        ];

        for (path, item) in paths {
            for method in HTTP_METHODS {
                let Some(operation) = item.get(method) else {
                    continue;
                };
                assert!(
                    parameter_named(operation, "X-Request-Id"),
                    "{method} {path} is missing the X-Request-Id parameter"
                );
                for (status, response) in operation["responses"]
                    .as_object()
                    .expect("operation responses")
                {
                    assert!(
                        response["headers"]["X-Request-Id"].is_object(),
                        "{method} {path} response {status} is missing X-Request-Id"
                    );
                }

                if !public_operations.contains(&(path.as_str(), method)) {
                    let security = operation["security"]
                        .as_array()
                        .expect("protected operation security");
                    let alternatives: BTreeSet<_> = security
                        .iter()
                        .flat_map(|requirement| {
                            requirement
                                .as_object()
                                .into_iter()
                                .flat_map(|requirement| requirement.keys().map(String::as_str))
                        })
                        .collect();
                    let expected = match path.as_str() {
                        // Setup-actor gated: `require_setup_actor` admits only
                        // `ActorType::SystemKey` and `TrustedJwt`, so a consumer key is
                        // not an alternative and must not be advertised as one.
                        "/api/v1/admin/setup/status" | "/api/v1/admin/setup/auth-methods" => {
                            BTreeSet::from(["bearerAuth", "systemKeyAuth"])
                        }
                        // System key only, and that narrowness is the security property:
                        // accepting a bearer JWT here would make "the first successful
                        // admin JWT wins" possible on a fresh deployment.
                        "/api/v1/admin/setup/claim" => BTreeSet::from(["systemKeyAuth"]),
                        _ => BTreeSet::from(["bearerAuth", "consumerKeyAuth", "systemKeyAuth"]),
                    };
                    assert_eq!(
                        alternatives, expected,
                        "unexpected security alternatives for {method} {path}"
                    );
                }
            }
        }
    }

    #[test]
    fn setup_status_contract_is_typed_and_exact() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operation = &value["paths"]["/api/v1/admin/setup/status"]["get"];
        assert_eq!(operation["operationId"], "get_setup_status");
        let responses = operation["responses"].as_object().unwrap();
        assert_eq!(
            responses
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["200", "401", "403", "500", "503"])
        );
        assert_eq!(
            responses["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/SetupStatusResponse"
        );

        for schema in [
            "SetupStatus",
            "SetupDeploymentEnvironment",
            "SetupCheckState",
            "SetupCheckName",
            "SetupChecks",
            "SetupStatusResponse",
        ] {
            assert!(
                value["components"]["schemas"][schema].is_object(),
                "missing setup schema {schema}"
            );
        }
    }

    #[test]
    fn once_only_key_responses_use_the_secret_envelope() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method, status) in [
            ("/api/v1/admin/system-keys", "post", "201"),
            ("/api/v1/admin/system-keys/{id}/rotate", "post", "200"),
            ("/api/v1/admin/consumer-keys", "post", "201"),
            ("/api/v1/admin/consumer-keys/{id}/rotate", "post", "200"),
        ] {
            assert_eq!(
                value["paths"][path][method]["responses"][status]["content"]["application/json"]["schema"]
                    ["$ref"],
                "#/components/schemas/ApiKeySecretResponse",
                "unexpected once-only key response for {method} {path}"
            );
        }

        let secret_schema = &value["components"]["schemas"]["ApiKeySecretResponse"];
        assert!(secret_schema["properties"]["secret"].is_object());
        assert!(secret_schema["properties"]["secret_retrievable"].is_object());
    }

    #[test]
    fn atomic_admin_idempotency_contract_is_explicit() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operations = [
            ("/api/v1/admin/applications", "post", "201", false, false),
            ("/api/v1/admin/providers", "post", "201", false, false),
            (
                "/api/v1/admin/providers/{provider_id}/models",
                "post",
                "201",
                false,
                false,
            ),
            (
                "/api/v1/admin/provider-credentials",
                "post",
                "201",
                false,
                false,
            ),
            (
                "/api/v1/admin/provider-credentials/{id}/rotate",
                "post",
                "200",
                true,
                false,
            ),
            ("/api/v1/admin/system-keys", "post", "201", false, true),
            (
                "/api/v1/admin/system-keys/{id}/rotate",
                "post",
                "200",
                false,
                true,
            ),
            ("/api/v1/admin/consumer-keys", "post", "201", false, true),
            (
                "/api/v1/admin/consumer-keys/{id}/rotate",
                "post",
                "200",
                false,
                true,
            ),
            ("/api/v1/admin/jwt-issuers", "post", "201", false, false),
            ("/api/v1/admin/setup/claim", "post", "201", false, false),
            ("/api/v1/admin/auth/providers", "post", "201", false, false),
        ];

        for (path, method, success_status, requires_if_match, once_only_secret) in operations {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} is missing Idempotency-Key"
            );
            assert_eq!(
                parameter_required(operation, "If-Match"),
                requires_if_match,
                "unexpected If-Match contract for {method} {path}"
            );

            let responses = operation["responses"]
                .as_object()
                .expect("operation responses");
            assert!(
                responses.contains_key(success_status),
                "{method} {path} is missing success status {success_status}"
            );
            assert!(
                responses.contains_key("409"),
                "{method} {path} must explicitly document idempotency conflicts"
            );
            assert_eq!(
                responses["409"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse",
                "{method} {path} must use ErrorResponse for idempotency conflicts"
            );
            let conflict_description = responses["409"]["description"]
                .as_str()
                .expect("409 description");
            for code in ["idempotency_conflict", "idempotency_in_progress"] {
                assert!(
                    conflict_description.contains(code),
                    "{method} {path} 409 description is missing {code}"
                );
            }

            if once_only_secret {
                let success_description = responses[success_status]["description"]
                    .as_str()
                    .expect("success description");
                assert!(
                    success_description.contains("replay")
                        && success_description.contains("secret_retrievable"),
                    "{method} {path} must document sanitized secret replays"
                );
            }
        }

        let error_schema = &value["components"]["schemas"]["ErrorResponse"];
        assert!(
            error_schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "error"))
        );
        let error_detail_schema = &value["components"]["schemas"]["ErrorDetail"];
        assert!(
            error_detail_schema["required"]
                .as_array()
                .is_some_and(|required| required.iter().any(|field| field == "request_id")),
            "replayed deterministic errors must carry the current request ID"
        );
    }

    /// Every operation in the generated document that declares an `If-Match` header, with the
    /// required-ness the API contract promises. This is a closed inventory: the test below
    /// asserts both directions, so adding, removing, or flipping an `If-Match` annotation
    /// anywhere in `http::admin` fails here until this table is updated deliberately.
    ///
    /// `atomic_admin_idempotency_contract_is_explicit` only ever looked at a hand-picked list
    /// of `POST` operations, which meant the `PUT` preconditions — including the
    /// execution-policy one that plan 04 made mandatory — could be downgraded to
    /// `Option<i64>` / "Optional…" with the entire suite still green. Plan 05 freezes
    /// `docs/openapi.json` from this generator, so a silently wrong precondition here would be
    /// baked into the published contract.
    ///
    /// `false` is reserved for preconditions that are genuinely advisory: the provider
    /// runtime-policy `PUT` reads the header through `optional_if_match`.
    const IF_MATCH_OPERATIONS: [(&str, &str, bool); 40] = [
        ("/api/v1/admin/agent-profiles/{id}", "delete", true),
        ("/api/v1/admin/agent-profiles/{id}", "patch", true),
        ("/api/v1/admin/agent-profiles/{id}/disable", "post", true),
        ("/api/v1/admin/agent-profiles/{id}/enable", "post", true),
        ("/api/v1/admin/applications/{id}", "delete", true),
        ("/api/v1/admin/applications/{id}", "patch", true),
        ("/api/v1/admin/applications/{id}/disable", "post", true),
        ("/api/v1/admin/applications/{id}/enable", "post", true),
        ("/api/v1/admin/auth/providers/{id}", "delete", true),
        ("/api/v1/admin/auth/providers/{id}", "patch", true),
        ("/api/v1/admin/auth/providers/{id}/disable", "post", true),
        ("/api/v1/admin/auth/providers/{id}/enable", "post", true),
        (
            "/api/v1/admin/applications/{id}/execution-policy",
            "put",
            true,
        ),
        ("/api/v1/admin/jwt-issuers/{id}", "delete", true),
        ("/api/v1/admin/jwt-issuers/{id}", "patch", true),
        ("/api/v1/admin/jwt-issuers/{id}/disable", "post", true),
        ("/api/v1/admin/jwt-issuers/{id}/enable", "post", true),
        ("/api/v1/admin/provider-credentials/{id}", "delete", true),
        ("/api/v1/admin/provider-credentials/{id}", "patch", true),
        (
            "/api/v1/admin/provider-credentials/{id}/disable",
            "post",
            true,
        ),
        (
            "/api/v1/admin/provider-credentials/{id}/enable",
            "post",
            true,
        ),
        (
            "/api/v1/admin/provider-credentials/{id}/rotate",
            "post",
            true,
        ),
        ("/api/v1/admin/provider-models/{id}", "delete", true),
        ("/api/v1/admin/provider-models/{id}", "patch", true),
        ("/api/v1/admin/provider-models/{id}/disable", "post", true),
        ("/api/v1/admin/provider-models/{id}/enable", "post", true),
        ("/api/v1/admin/providers/{id}", "delete", true),
        ("/api/v1/admin/providers/{id}", "patch", true),
        ("/api/v1/admin/providers/{id}/disable", "post", true),
        ("/api/v1/admin/providers/{id}/enable", "post", true),
        (
            "/api/v1/admin/providers/{provider_id}/runtime-policy",
            "put",
            false,
        ),
        ("/api/v1/admin/routes/{id}", "delete", true),
        ("/api/v1/admin/routes/{id}", "patch", true),
        ("/api/v1/admin/routes/{id}/disable", "post", true),
        ("/api/v1/admin/routes/{id}/enable", "post", true),
        ("/api/v1/admin/routing-policies/{id}", "delete", true),
        ("/api/v1/admin/routing-policies/{id}", "patch", true),
        ("/api/v1/admin/routing-policies/{id}/disable", "post", true),
        ("/api/v1/admin/routing-policies/{id}/enable", "post", true),
        (
            "/api/v1/admin/users/{external_user_id}/provider-credentials/{id}",
            "delete",
            true,
        ),
    ];

    /// Collects `(path, method, if_match_required)` for every operation that declares an
    /// `If-Match` header, so the assertion can be made in both directions.
    fn if_match_inventory(value: &Value) -> Vec<(String, String, bool)> {
        let mut found: Vec<(String, String, bool)> = value["paths"]
            .as_object()
            .expect("paths")
            .iter()
            .flat_map(|(path, item)| {
                item.as_object()
                    .expect("path item")
                    .iter()
                    .filter(|(_, operation)| parameter_named(operation, "If-Match"))
                    .map(|(method, operation)| {
                        (
                            path.clone(),
                            method.clone(),
                            parameter_required(operation, "If-Match"),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn every_if_match_operation_declares_the_documented_precondition() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let found = if_match_inventory(&value);

        let mut expected: Vec<(String, String, bool)> = IF_MATCH_OPERATIONS
            .iter()
            .map(|(path, method, required)| ((*path).to_string(), (*method).to_string(), *required))
            .collect();
        expected.sort();

        assert_eq!(
            found, expected,
            "the generated If-Match contract drifted from the documented inventory; \
             a required precondition must never silently become optional (plan 05 freezes \
             docs/openapi.json from this generator)"
        );

        // Required-ness alone is not the whole contract: an `If-Match` documented as a string
        // would still typecheck as "required" while telling clients the wrong thing.
        for (path, method, required) in &found {
            let operation = &value["paths"][path][method];
            let parameter = operation["parameters"]
                .as_array()
                .expect("operation parameters")
                .iter()
                .find(|parameter| parameter["name"] == "If-Match")
                .expect("If-Match parameter");
            assert_eq!(
                parameter["in"], "header",
                "{method} {path} must declare If-Match as a header"
            );
            let description = parameter["description"].as_str().unwrap_or_default();
            let expected_word = if *required { "Required" } else { "Optional" };
            assert!(
                description.starts_with(expected_word),
                "{method} {path} declares If-Match required={required} but describes it as \
                 {description:?}"
            );
        }
    }

    #[test]
    fn execution_policy_put_declares_if_match_as_required() {
        // Plan 04 made this precondition mandatory (`require_if_match`, not
        // `optional_if_match`). Pinned on its own as well as in the inventory above, because a
        // reviewer demonstrated that reverting only this annotation to `Option<i64>` left the
        // whole suite green.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operation = &value["paths"]["/api/v1/admin/applications/{id}/execution-policy"]["put"];
        assert!(
            parameter_required(operation, "If-Match"),
            "PUT execution-policy must document If-Match as required"
        );
    }

    #[test]
    fn every_local_schema_reference_resolves() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let schemas = value["components"]["schemas"]
            .as_object()
            .expect("component schemas");
        assert_refs_resolve(&value, schemas);
    }

    /// The four RAG write operations that carry `Idempotency-Key` and now implement
    /// real replay (`plans/02b-idempotency-replay.md`). 02a's interim disclaimer
    /// (Sentence B) used to live on these operations too; it is gone as of 02b.
    const RAG_WRITE_OPERATIONS: [(&str, &str); 4] = [
        ("/api/v1/admin/rag-collections", "post"),
        (
            "/api/v1/admin/rag-collections/{collection_id}/documents",
            "post",
        ),
        ("/api/v1/admin/rag-documents/{id}/ingest", "post"),
        ("/api/v1/admin/rag-documents/{id}/reindex", "post"),
    ];

    /// The seven conversation/memory/policy operations that carry only the short
    /// Sentence A (no `Idempotency-Key`, no Sentence B).
    const OTHER_PREVIEW_OPERATIONS: [(&str, &str); 7] = [
        ("/api/v1/conversations", "post"),
        ("/api/v1/conversations/{id}/messages", "post"),
        ("/api/v1/memories", "post"),
        (
            "/api/v1/admin/applications/{application_id}/conversation-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/memory-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/retrieval-policy",
            "put",
        ),
        (
            "/api/v1/admin/applications/{application_id}/embedding-policy",
            "put",
        ),
    ];

    const SENTENCE_A_RAG_WRITE: &str = "Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md.";
    const SENTENCE_A_SHORT: &str = "Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses.";
    const SENTENCE_B_INTERIM_IDEMPOTENCY: &str = "Idempotency-Key is accepted but replay is not implemented yet; retrying can duplicate side effects.";
    /// The truthful `Idempotency-Key` parameter description 02b installs on all four
    /// RAG write operations, replacing 02a's "not implemented yet" text
    /// (`plans/02b-idempotency-replay.md`, Architecture -> "API & OpenAPI changes").
    const IDEMPOTENCY_KEY_PARAMETER_DESCRIPTION: &str = "Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409.";

    #[test]
    fn rag_write_routes_still_declare_the_idempotency_key_parameter() {
        // Guard against a stale "remove the parameter" instinct: CONVENTIONS.md §0
        // decision D1 settled that P0-2 is fixed by implementing real replay (02b),
        // not by removing Idempotency-Key from these routes.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} must still declare Idempotency-Key"
            );
            assert_eq!(
                parameter_description(operation, "Idempotency-Key"),
                Some(IDEMPOTENCY_KEY_PARAMETER_DESCRIPTION),
                "{method} {path} Idempotency-Key description drifted"
            );
        }
    }

    #[test]
    fn rag_write_routes_declare_the_idempotency_replay_contract() {
        // A sibling to `atomic_admin_idempotency_contract_is_explicit`, kept separate
        // because these four routes are not admin-command routes in the operations
        // list that test enumerates.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            assert!(
                parameter_named(operation, "Idempotency-Key"),
                "{method} {path} must declare Idempotency-Key"
            );
            let description =
                parameter_description(operation, "Idempotency-Key").unwrap_or_else(|| {
                    panic!("{method} {path} is missing the Idempotency-Key parameter")
                });
            assert!(
                !description.to_lowercase().contains("not implemented"),
                "{method} {path} Idempotency-Key description still disclaims replay: {description}"
            );

            let responses = operation["responses"]
                .as_object()
                .expect("operation responses");
            assert!(
                responses.contains_key("409"),
                "{method} {path} must explicitly document a 409 idempotency response"
            );
            assert_eq!(
                responses["409"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse",
                "{method} {path} must use ErrorResponse for its 409 response"
            );
        }
    }

    #[test]
    fn rag_write_route_descriptions_no_longer_disclaim_idempotency() {
        // The paired half of 02a's `rag_write_routes_carry_the_interim_idempotency_disclaimer`,
        // which 02b deletes: Sentence B is gone, Sentence A survives verbatim.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.contains(SENTENCE_B_INTERIM_IDEMPOTENCY),
                "{method} {path} still carries 02a's interim idempotency disclaimer"
            );
            assert!(
                description.contains(SENTENCE_A_RAG_WRITE),
                "{method} {path} must still carry Sentence A verbatim"
            );
        }
    }

    #[test]
    fn rag_collection_document_route_keeps_its_collection_id_path_parameter() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let operation =
            &value["paths"]["/api/v1/admin/rag-collections/{collection_id}/documents"]["post"];
        let parameter = operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "collection_id")
            .expect("collection_id path parameter must survive");
        assert_eq!(parameter["in"], "path");
        assert_eq!(parameter["required"], true);
        assert_eq!(parameter["description"], "RAG collection identifier");
    }

    #[test]
    fn conversation_memory_rag_operations_document_the_mvp_preview_boundary() {
        // Assert Sentence A only (the permanent boundary text). Sentence B is
        // deliberately checked by a separate test that plan 02b deletes.
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        for (path, method) in RAG_WRITE_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.is_empty(),
                "{method} {path} description must not be empty"
            );
            assert!(
                description.contains("used to influence model responses"),
                "{method} {path} description is missing the invariant phrase"
            );
            assert!(
                description.contains(SENTENCE_A_RAG_WRITE),
                "{method} {path} description is missing Sentence A verbatim"
            );
        }
        for (path, method) in OTHER_PREVIEW_OPERATIONS {
            let operation = &value["paths"][path][method];
            let description = operation["description"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} is missing an operation description"));
            assert!(
                !description.is_empty(),
                "{method} {path} description must not be empty"
            );
            assert!(
                description.contains("used to influence model responses"),
                "{method} {path} description is missing the invariant phrase"
            );
            assert!(
                description.contains(SENTENCE_A_SHORT),
                "{method} {path} description is missing the short Sentence A verbatim"
            );
        }
    }

    #[test]
    fn rag_document_record_schema_exposes_ingestion_status() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let property =
            &value["components"]["schemas"]["RagDocumentRecord"]["properties"]["ingestion_status"];
        assert!(
            property.is_object(),
            "RagDocumentRecord.ingestion_status schema is missing"
        );
        // utoipa represents Option<RagIngestionStatus> as a `oneOf` of `null` and a
        // `$ref` to the enum schema rather than a bare `$ref`.
        let one_of = property["oneOf"]
            .as_array()
            .expect("ingestion_status must be a oneOf wrapping the optional enum");
        assert!(
            one_of.iter().any(|variant| variant["type"] == "null"),
            "ingestion_status oneOf must allow null"
        );
        assert!(
            one_of
                .iter()
                .any(|variant| variant["$ref"] == "#/components/schemas/RagIngestionStatus"),
            "ingestion_status oneOf must reference RagIngestionStatus"
        );

        let enum_schema = &value["components"]["schemas"]["RagIngestionStatus"];
        let variants: BTreeSet<_> = enum_schema["enum"]
            .as_array()
            .expect("RagIngestionStatus must enumerate its variants")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            variants,
            BTreeSet::from([
                "pending",
                "downloading",
                "parsing",
                "chunking",
                "embedding",
                "indexed",
                "failed",
                "superseded",
            ])
        );
    }

    #[test]
    fn public_response_schema_documents_always_empty_citations() {
        let value = serde_json::to_value(documented_router(RouterPolicy::default()).into_openapi())
            .unwrap();
        let description = value["components"]["schemas"]["PublicResponse"]["properties"]
            ["citations"]["description"]
            .as_str()
            .expect("PublicResponse.citations must carry a description");
        assert!(!description.is_empty());
        assert!(
            description.to_lowercase().contains("empty"),
            "citations description must state that the array is always empty: {description}"
        );
        assert!(
            description.to_lowercase().contains("not wired"),
            "citations description must state that retrieval is not wired: {description}"
        );
    }

    // ---------------------------------------------------------------------
    // OpenAPI drift gate (plan 05 / P1-10a)
    //
    // `docs/openapi.json` is the frozen contract every later plan diffs against.
    // Everything below exists so that a route, DTO, parameter, status code or
    // header change that is not accompanied by a regenerated spec fails
    // `cargo test` with an actionable message, rather than silently shipping a
    // document that no consumer of `docs/openapi.json` can see.
    // ---------------------------------------------------------------------

    /// Committed contract, relative to the crate root.
    const COMMITTED_OPENAPI_PATH: &str = "docs/openapi.json";

    /// The exact command that regenerates the committed contract.
    ///
    /// This constant is load-bearing, not decorative: a drift gate whose failure
    /// message does not say how to fix it gets bypassed, so
    /// `openapi_drift_failure_message_names_the_regeneration_command` asserts the
    /// panic text carries it.
    const REGENERATE_OPENAPI_COMMAND: &str = "UPDATE_SNAPSHOTS=1 cargo test --lib \
         http::tests::committed_openapi_matches_the_generated_document";

    fn committed_openapi_file() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(COMMITTED_OPENAPI_PATH)
    }

    /// Serializes the document that `/openapi.json` actually serves.
    ///
    /// [`documented_router`] is the only correct source. `MoiraApiDoc::openapi()`
    /// is *not*: it is the bare derive output, before the `.routes(...)`
    /// registrations in [`documented_router`] add any path and before
    /// [`openapi::finalize_document`] adds the `X-Request-Id` parameter and
    /// response header to every operation. Freezing that document would freeze a
    /// fiction — [`router`] hands `documented_router(policy).split_for_parts()`'s
    /// document to the `/openapi.json` handler as an `Extension`, and the handler
    /// serves exactly that value (`src/http/openapi.rs`, `openapi_json`).
    ///
    /// The output is canonical: serializing through [`Value`] first means every
    /// object key is emitted in sorted order, because `serde_json::Map` is a
    /// `BTreeMap` (serde_json's `preserve_order` feature is off in this
    /// workspace) and utoipa's `Paths` is a `BTreeMap` too (its
    /// `preserve_path_order` feature is off). Nothing in the pipeline is a
    /// `HashMap`, so the byte output cannot depend on iteration order — which is
    /// what `openapi_document_serialization_is_byte_stable_within_a_process`
    /// checks rather than assumes.
    fn generate_openapi_json(policy: RouterPolicy) -> String {
        let document = documented_router(policy).into_openapi();
        let value =
            serde_json::to_value(document).expect("serialize the generated OpenAPI document");
        let mut text = serde_json::to_string_pretty(&value)
            .expect("render the generated OpenAPI document as JSON");
        text.push('\n');
        text
    }

    /// Collects JSON pointers at which two documents disagree.
    ///
    /// A whole-document `assert_eq!` on a ~500 KB spec is unreadable, and an
    /// unreadable failure is a failure engineers learn to ignore.
    fn json_differences(left: &Value, right: &Value, pointer: &str, differences: &mut Vec<String>) {
        match (left, right) {
            (Value::Object(left_object), Value::Object(right_object)) => {
                let keys: BTreeSet<&String> =
                    left_object.keys().chain(right_object.keys()).collect();
                for key in keys {
                    let child = format!("{pointer}/{key}");
                    match (left_object.get(key), right_object.get(key)) {
                        (Some(left_value), Some(right_value)) => {
                            json_differences(left_value, right_value, &child, differences);
                        }
                        (Some(_), None) => differences.push(format!("{child}: only in committed")),
                        (None, Some(_)) => differences.push(format!("{child}: only in generated")),
                        (None, None) => unreachable!("key came from one of the two maps"),
                    }
                }
            }
            (Value::Array(left_values), Value::Array(right_values)) => {
                if left_values.len() != right_values.len() {
                    differences.push(format!(
                        "{pointer}: array length {} in committed, {} in generated",
                        left_values.len(),
                        right_values.len()
                    ));
                    return;
                }
                for (index, (left_value, right_value)) in
                    left_values.iter().zip(right_values.iter()).enumerate()
                {
                    let child = format!("{pointer}/{index}");
                    json_differences(left_value, right_value, &child, differences);
                }
            }
            _ => {
                if left != right {
                    differences.push(format!("{pointer}: committed != generated"));
                }
            }
        }
    }

    fn drift_failure_message(differences: &[String]) -> String {
        let shown: Vec<&str> = differences
            .iter()
            .take(25)
            .map(String::as_str)
            .collect::<Vec<_>>();
        let elided = differences.len().saturating_sub(shown.len());
        let mut message = format!(
            "{COMMITTED_OPENAPI_PATH} is stale: it no longer matches the document served at \
             GET /openapi.json.\nRegenerate and commit it with:\n\n    \
             {REGENERATE_OPENAPI_COMMAND}\n\nDo not hand-edit {COMMITTED_OPENAPI_PATH}.\n\
             {} difference(s):\n",
            differences.len()
        );
        for difference in shown {
            message.push_str("  - ");
            message.push_str(difference);
            message.push('\n');
        }
        if elided > 0 {
            message.push_str(&format!("  … and {elided} more\n"));
        }
        message
    }

    fn committed_openapi_value() -> Value {
        let path = committed_openapi_file();
        let text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}\nGenerate it with:\n\n    {REGENERATE_OPENAPI_COMMAND}\n",
                path.display()
            )
        });
        serde_json::from_str(&text).unwrap_or_else(|err| {
            panic!(
                "{} is not valid JSON: {err}\nRegenerate it with:\n\n    \
                 {REGENERATE_OPENAPI_COMMAND}\n",
                path.display()
            )
        })
    }

    /// Generating twice in one process must produce identical bytes.
    ///
    /// A gate that flaps on map iteration order gets disabled by the first person
    /// it annoys, so determinism is verified rather than assumed.
    #[test]
    fn openapi_document_serialization_is_byte_stable_within_a_process() {
        let first = generate_openapi_json(RouterPolicy::default());
        let second = generate_openapi_json(RouterPolicy::default());
        assert_eq!(
            first.len(),
            second.len(),
            "OpenAPI serialization is not byte-stable across two generations in one process"
        );
        assert!(
            first == second,
            "OpenAPI serialization is not byte-stable across two generations in one process"
        );

        // Key ordering must be sorted, not merely repeatable: a repeatable but
        // insertion-ordered document would still churn whenever an unrelated
        // route is added ahead of another.
        let document: Value = serde_json::from_str(&first).expect("the generated document parses");
        let paths: Vec<&str> = document["paths"]
            .as_object()
            .expect("paths object")
            .keys()
            .map(String::as_str)
            .collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(
            paths, sorted,
            "OpenAPI path keys must be emitted in sorted order"
        );
    }

    /// The frozen document must not depend on `RouterPolicy`.
    ///
    /// Body limits and timeouts are tower layers; if one ever leaked into the
    /// document, the committed contract would silently depend on whichever
    /// settings the generating machine had.
    #[test]
    fn openapi_document_is_independent_of_the_router_policy() {
        let default_policy = RouterPolicy::default();
        let alternative = RouterPolicy {
            public_body_limit_bytes: default_policy.public_body_limit_bytes / 2,
            conversation_body_limit_bytes: default_policy.conversation_body_limit_bytes / 2,
            admin_body_limit_bytes: default_policy.admin_body_limit_bytes / 2,
            non_streaming_timeout: default_policy.non_streaming_timeout + Duration::from_secs(7),
            streaming_head_timeout: default_policy.streaming_head_timeout + Duration::from_secs(7),
            request_body_idle_timeout: default_policy.request_body_idle_timeout
                + Duration::from_secs(7),
        };
        assert_ne!(default_policy, alternative);
        assert!(
            generate_openapi_json(default_policy) == generate_openapi_json(alternative),
            "the generated OpenAPI document must not vary with RouterPolicy"
        );
    }

    /// The drift gate itself.
    ///
    /// Run with `UPDATE_SNAPSHOTS=1` to rewrite the committed contract; the
    /// default run never writes.
    #[test]
    fn committed_openapi_matches_the_generated_document() {
        let generated = generate_openapi_json(RouterPolicy::default());
        let path = committed_openapi_file();

        if std::env::var("UPDATE_SNAPSHOTS").is_ok_and(|value| value == "1") {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create the docs directory");
            }
            std::fs::write(&path, generated.as_bytes())
                .unwrap_or_else(|err| panic!("cannot write {}: {err}", path.display()));
            return;
        }

        let committed_text = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "cannot read {}: {err}\nGenerate it with:\n\n    {REGENERATE_OPENAPI_COMMAND}\n",
                path.display()
            )
        });
        let committed_value: Value = serde_json::from_str(&committed_text).unwrap_or_else(|err| {
            panic!(
                "{} is not valid JSON: {err}\nRegenerate it with:\n\n    \
                 {REGENERATE_OPENAPI_COMMAND}\n",
                path.display()
            )
        });
        let generated_value: Value =
            serde_json::from_str(&generated).expect("generated document parses");

        // Structural comparison first: it produces the actionable diff, and it
        // cannot fail on key ordering.
        let mut differences = Vec::new();
        json_differences(&committed_value, &generated_value, "", &mut differences);
        assert!(
            differences.is_empty(),
            "{}",
            drift_failure_message(&differences)
        );

        // Byte comparison second: structurally-equal-but-differently-formatted
        // means the file was hand-edited or written by something other than this
        // test, which would make future diffs unreadable.
        assert!(
            committed_text == generated,
            "{COMMITTED_OPENAPI_PATH} is structurally correct but not in the canonical \
             formatting this gate writes. Do not hand-edit it; regenerate with:\n\n    \
             {REGENERATE_OPENAPI_COMMAND}\n"
        );
    }

    #[test]
    fn openapi_drift_failure_message_names_the_regeneration_command() {
        let message = drift_failure_message(&["/paths/x: committed != generated".to_string()]);
        assert!(
            message.contains(REGENERATE_OPENAPI_COMMAND),
            "the drift failure message must name the regeneration command: {message}"
        );
        assert!(
            message.contains(COMMITTED_OPENAPI_PATH),
            "the drift failure message must name the committed file: {message}"
        );
        assert!(
            message.contains("/paths/x"),
            "the drift failure message must name the differing locations: {message}"
        );
    }

    #[test]
    fn committed_openapi_declares_openapi_3_1() {
        let value = committed_openapi_value();
        let version = value["openapi"]
            .as_str()
            .expect("the committed document must declare an `openapi` version");
        assert!(
            version.starts_with("3.1"),
            "the committed document must declare OpenAPI 3.1, found {version}"
        );
    }

    /// Mechanical proof that plan 04 landed before the contract was frozen.
    ///
    /// Plan 04 flipped `If-Match` on this operation from optional to required — a
    /// breaking OpenAPI change. If this test is red, the branch was cut too
    /// early: rebase onto a `main` that contains plan 04 and regenerate. Never
    /// hand-edit the JSON to make it pass.
    #[test]
    fn committed_openapi_marks_if_match_required_on_execution_policy_put() {
        let value = committed_openapi_value();
        let operation = &value["paths"]["/api/v1/admin/applications/{id}/execution-policy"]["put"];
        assert!(
            operation.is_object(),
            "the committed document must describe PUT /api/v1/admin/applications/{{id}}/execution-policy"
        );
        assert!(
            parameter_named(operation, "If-Match"),
            "the committed document must document the If-Match parameter on the execution-policy PUT"
        );
        assert!(
            parameter_required(operation, "If-Match"),
            "If-Match must be required on the execution-policy PUT (plan 04 / P1-8); a false here \
             means the spec was frozen from a tree that predates plan 04"
        );
    }

    fn parameter_named(operation: &Value, name: &str) -> bool {
        operation["parameters"]
            .as_array()
            .is_some_and(|parameters| parameters.iter().any(|parameter| parameter["name"] == name))
    }

    fn parameter_description<'a>(operation: &'a Value, name: &str) -> Option<&'a str> {
        operation["parameters"].as_array().and_then(|parameters| {
            parameters
                .iter()
                .find(|parameter| parameter["name"] == name)
                .and_then(|parameter| parameter["description"].as_str())
        })
    }

    fn parameter_required(operation: &Value, name: &str) -> bool {
        operation["parameters"]
            .as_array()
            .is_some_and(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == name)
                    .is_some_and(|parameter| parameter["required"] == true)
            })
    }

    fn assert_refs_resolve(value: &Value, schemas: &serde_json::Map<String, Value>) {
        match value {
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                    && let Some(name) = reference.strip_prefix("#/components/schemas/")
                {
                    assert!(
                        schemas.contains_key(name),
                        "unresolved schema ref: {reference}"
                    );
                }
                for nested in object.values() {
                    assert_refs_resolve(nested, schemas);
                }
            }
            Value::Array(values) => {
                for nested in values {
                    assert_refs_resolve(nested, schemas);
                }
            }
            _ => {}
        }
    }
}
