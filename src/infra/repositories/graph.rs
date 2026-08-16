//! Postgres reads for the derived relationship graph (plan 12 §4, issue #234).
//!
//! Read-only, and deliberately its own small repository rather than new methods scattered
//! across `RuntimeRepository`, `PgAgentPlatformRepository`, and `AdminRepository` — the same
//! three that would otherwise each gain one bespoke, unpaginated method for a single caller:
//!
//! * the graph needs three `agent_profiles` columns (`skill_refs`/`eval_suite_refs`/
//!   `memory_scope_refs`) that no existing `agent_profiles` query selects;
//! * `eval_suites` and `agent_flows`/`agent_flow_steps` have no list-all read yet at all
//!   (`PgAgentPlatformRepository`'s own doc comment calls that CRUD "a documented follow-up");
//! * the `route_definitions` ⨝ `routing_policies` ⨝ `provider_models` chain needs joining for
//!   the one edge kind buildable purely from tables that predate this feature, which no
//!   existing method does.
//!
//! One cohesive boundary for one derived, cross-cutting, read-only view is easier to reason
//! about than five scattered additions to repositories that each own only part of what the
//! graph needs. Every statement here is a plain `select` against a table another repository
//! already owns for writes; nothing here ever writes, and nothing here is paginated — see
//! [`GRAPH_ROW_LIMIT`].

use sqlx::PgPool;

use crate::{
    domain::{
        AgentProfileGraphRow, AgentRouteEdgeRow, FlowStepGraphRow, GraphRawData,
        NamedStatusGraphRow,
    },
    error::AppError,
    infra::pg_rows::{
        agent_profile_graph_row_from_row, agent_route_edge_row_from_row,
        flow_step_graph_row_from_row, named_status_graph_row_from_row,
    },
};

/// Row cap per registry read. The graph is deployment-wide admin configuration (agents,
/// skills, eval suites, flows, providers, models) — not tenant data — so every deployment this
/// ships for today is comfortably under this. A truncated-but-honest graph is the safe failure
/// mode for a read with no pagination contract of its own; raising the cap later is a one-line
/// change, and `GraphResponse` carries no `has_more` because a partial derived projection is
/// still a correct one — it just does not claim completeness beyond the cap.
const GRAPH_ROW_LIMIT: i64 = 2000;

#[derive(Clone)]
pub struct PgGraphRepository {
    pool: PgPool,
}

impl PgGraphRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reads every registry the graph draws on, in one call. Deliberately not split into
    /// several public methods: every caller of this repository wants the whole graph, so a
    /// single entry point is the honest shape rather than eight methods `GraphService` would
    /// always call together anyway.
    pub async fn fetch_graph_data(&self) -> Result<GraphRawData, AppError> {
        Ok(GraphRawData {
            agents: self.fetch_agents().await?,
            skills: self.fetch_named_status("skills").await?,
            eval_suites: self.fetch_named_status("eval_suites").await?,
            flows: self.fetch_named_status("agent_flows").await?,
            flow_steps: self.fetch_flow_steps().await?,
            providers: self.fetch_named_status("providers").await?,
            models: self.fetch_models().await?,
            agent_route_edges: self.fetch_agent_route_edges().await?,
        })
    }

    async fn fetch_agents(&self) -> Result<Vec<AgentProfileGraphRow>, AppError> {
        let rows = sqlx::query(
            "select id, display_name, status, skill_refs, eval_suite_refs, memory_scope_refs \
             from agent_profiles \
             where deleted_at is null \
             order by created_at desc, id desc \
             limit $1",
        )
        .bind(GRAPH_ROW_LIMIT)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(agent_profile_graph_row_from_row).collect()
    }

    /// `table` is always one of this file's own `&'static str` literals below, never caller
    /// input — interpolating it is the same pattern `PgAgentPlatformRepository` and
    /// `PgRuntimeRepository` already use for their own fixed column lists, not a SQL-injection
    /// surface.
    async fn fetch_named_status(
        &self,
        table: &'static str,
    ) -> Result<Vec<NamedStatusGraphRow>, AppError> {
        let sql = format!(
            "select id, display_name, status from {table} \
             where deleted_at is null \
             order by created_at desc, id desc \
             limit $1"
        );
        let rows = sqlx::query(&sql)
            .bind(GRAPH_ROW_LIMIT)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(named_status_graph_row_from_row).collect()
    }

    /// `provider_models.display_name` is nullable; `coalesce(display_name, model_key)` gives
    /// every row a non-null label so it fits [`NamedStatusGraphRow`]'s shape without a sixth
    /// row type just for models.
    async fn fetch_models(&self) -> Result<Vec<NamedStatusGraphRow>, AppError> {
        let rows = sqlx::query(
            "select id, coalesce(display_name, model_key) as display_name, status \
             from provider_models \
             where deleted_at is null \
             order by created_at desc, id desc \
             limit $1",
        )
        .bind(GRAPH_ROW_LIMIT)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(named_status_graph_row_from_row).collect()
    }

    async fn fetch_flow_steps(&self) -> Result<Vec<FlowStepGraphRow>, AppError> {
        let rows = sqlx::query(
            "select flow_id, agent_profile_id from agent_flow_steps \
             order by flow_id, step_order \
             limit $1",
        )
        .bind(GRAPH_ROW_LIMIT)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(flow_step_graph_row_from_row).collect()
    }

    /// The one edge type buildable purely from tables that predate this feature (plan 12 §4):
    /// `route_definitions.agent_profile_id` joined through `routing_policies` to
    /// `provider_models`. Only routes that actually name an agent, and only live rows on both
    /// sides of the join, are returned — `assemble_graph` additionally drops any pair whose
    /// agent or model id does not resolve to a node from this same read, so a row here is
    /// never treated as authoritative on its own.
    async fn fetch_agent_route_edges(&self) -> Result<Vec<AgentRouteEdgeRow>, AppError> {
        let rows = sqlx::query(
            "select rd.agent_profile_id as agent_profile_id, \
                    rp.provider_model_id as provider_model_id \
             from route_definitions rd \
             join routing_policies rp on rp.route_id = rd.id \
             where rd.deleted_at is null \
               and rd.agent_profile_id is not null \
               and rp.deleted_at is null \
             limit $1",
        )
        .bind(GRAPH_ROW_LIMIT)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(agent_route_edge_row_from_row).collect()
    }
}
