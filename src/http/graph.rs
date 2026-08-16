//! Admin HTTP handler for the derived relationship graph (plan 12 §4, issue #234).
//!
//! A new module rather than more lines in `admin.rs` or `agent_platform.rs`: the graph reads
//! across six registries owned by three different repositories, so it belongs to none of those
//! existing handler modules any more than another. Reuses `admin.rs`'s admin-plane wrapper
//! (`admin_actor`) so this GET gates on the exact same authentication contract every other
//! admin handler uses; there is no `If-Match` here because there is nothing to write.

use axum::{Json, extract::State, http::HeaderMap};

use crate::{
    app::AppState,
    application::GraphService,
    domain::GraphResponse,
    error::{AppError, ErrorResponse},
};

use super::admin::admin_actor;

#[utoipa::path(
    get, path = "/api/v1/admin/graph", tag = "admin-graph",
    responses(
        (status = 200, description = "Derived relationship graph over the agent-platform and provider/model registries", body = GraphResponse),
        (status = "4XX", description = "Authentication or authorization error", body = ErrorResponse),
        (status = "5XX", description = "Infrastructure or internal error", body = ErrorResponse)
    ),
    security(("bearerAuth" = []), ("systemKeyAuth" = []), ("consumerKeyAuth" = []))
)]
pub async fn get_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<GraphResponse>, AppError> {
    let actor = admin_actor(&state, &headers).await?;
    GraphService::new(&state)?.get_graph(&actor).await.map(Json)
}
