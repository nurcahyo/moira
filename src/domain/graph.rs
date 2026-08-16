//! Derived relationship-graph domain types (plan 12 §4, issue #234).
//!
//! The graph is a **read-only projection** over registries that already exist —
//! `agent_profiles`, `skills`, `eval_suites`, `agent_flows`/`agent_flow_steps`, `providers`,
//! `provider_models` — and the foreign keys / ref columns between them. Nothing here is
//! stored: `GraphResponse` is assembled fresh on every request by
//! `crate::application::GraphService` from raw rows [`PgGraphRepository`][repo] reads, via the
//! pure [`assemble_graph`] function below. Keeping assembly pure and separate from the
//! repository read is what lets the "graph from rows" logic be unit-tested with hand-built
//! rows and no database (`docs/project-structure.md`'s dependency-minimal spirit for
//! `src/domain`).
//!
//! [repo]: crate::infra::repositories::PgGraphRepository

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// Which registry a [`GraphNode`] was derived from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeType {
    Agent,
    Skill,
    EvalSuite,
    Flow,
    Provider,
    Model,
    /// Synthetic — one node per distinct memory-scope value actually referenced by some
    /// agent's `memory_scope_refs`, not one per `memory_records` row (plan 12 §4).
    MemoryScope,
}

/// The FK/ref an edge was derived from (plan 12 §4). Every edge is a straight read of an
/// existing column — no computed weight, no new join table.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    /// From `agent_profiles.skill_refs`.
    AgentUsesSkill,
    /// From `agent_profiles.eval_suite_refs`.
    AgentUsesEvalSuite,
    /// From `agent_profiles.memory_scope_refs`.
    AgentReadsMemoryScope,
    /// From `agent_flow_steps` (`flow_id` + `agent_profile_id` on the same row).
    FlowContainsAgent,
    /// From the existing model-routing chain: `route_definitions.agent_profile_id` joined
    /// through `routing_policies` to `provider_models` — the one edge kind buildable purely
    /// from tables that predate this feature.
    AgentRoutesToModel,
}

/// One node in the derived graph. `id` is `"<type>:<key>"` (e.g. `"agent:0199…"`,
/// `"memory_scope:tenant_application"`) — composite so an edge's `from`/`to` can never collide
/// across node types that would otherwise share a bare UUID namespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct GraphNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: GraphNodeType,
    pub label: String,
    /// The owning row's lifecycle status, passed through as-is. Registries do not share one
    /// status vocabulary (skills are `draft`/`enabled`/`disabled`; most others are
    /// `active`/`disabled`/`deleted`), so this is intentionally a bare string rather than one
    /// shared enum. `None` for the synthetic `memory_scope` node type, which has no row of its
    /// own to carry a status.
    pub status: Option<String>,
}

/// One edge in the derived graph. `from`/`to` are [`GraphNode::id`] values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GraphResponse {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub generated_at: DateTime<Utc>,
}

// =====================================================================================
// Raw rows — the wire between `PgGraphRepository` (read) and `assemble_graph` (assembly).
// Never serialized to a caller; deliberately outside the `GraphNode`/`GraphEdge` shape so a
// read-decode bug and an assembly bug fail in different, easier-to-localize tests.
// =====================================================================================

/// The three additive `agent_profiles` columns (`migrations/0031_agent_platform.sql`) plus
/// enough identity to render a node, in one row.
#[derive(Debug, Clone)]
pub struct AgentProfileGraphRow {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    pub skill_refs: Vec<Uuid>,
    pub eval_suite_refs: Vec<Uuid>,
    /// Contract (nothing writes this column yet — sub-plan 1 is schema-only, per the
    /// migration header): a JSON array of scope-name strings drawn from plan 11's four
    /// memory scopes. Anything else (an object, a null, unknown strings) is treated as "no
    /// scopes" by [`memory_scopes_in`] rather than as an error — a derived read must not 500
    /// on a shape this code does not itself control yet.
    pub memory_scope_refs: Value,
}

/// The shape shared by every plain "named registry row with a lifecycle status" the graph
/// reads: `skills`, `eval_suites`, `agent_flows`, `providers`, and `provider_models` (whose
/// `display_name` is `coalesce(display_name, model_key)` at the query, so it fits this same
/// shape instead of needing a sixth type).
#[derive(Debug, Clone)]
pub struct NamedStatusGraphRow {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
}

/// One `agent_flow_steps` row, reduced to the two columns the `flow_contains_agent` edge
/// needs.
#[derive(Debug, Clone, Copy)]
pub struct FlowStepGraphRow {
    pub flow_id: Uuid,
    pub agent_profile_id: Uuid,
}

/// One `route_definitions` ⨝ `routing_policies` pair, reduced to the two ids the
/// `agent_routes_to_model` edge needs.
#[derive(Debug, Clone, Copy)]
pub struct AgentRouteEdgeRow {
    pub agent_profile_id: Uuid,
    pub provider_model_id: Uuid,
}

#[derive(Debug, Clone, Default)]
pub struct GraphRawData {
    pub agents: Vec<AgentProfileGraphRow>,
    pub skills: Vec<NamedStatusGraphRow>,
    pub eval_suites: Vec<NamedStatusGraphRow>,
    pub flows: Vec<NamedStatusGraphRow>,
    pub flow_steps: Vec<FlowStepGraphRow>,
    pub providers: Vec<NamedStatusGraphRow>,
    pub models: Vec<NamedStatusGraphRow>,
    pub agent_route_edges: Vec<AgentRouteEdgeRow>,
}

/// Plan 11's four memory scopes. `memory_scope_refs` entries outside this set are dropped
/// rather than surfaced — see [`AgentProfileGraphRow::memory_scope_refs`].
const KNOWN_MEMORY_SCOPES: [&str; 4] = [
    "conversation",
    "user_application",
    "tenant_application",
    "application",
];

fn memory_scopes_in(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|scope| KNOWN_MEMORY_SCOPES.contains(scope))
        .map(str::to_string)
        .collect()
}

fn node_id(prefix: &str, key: impl std::fmt::Display) -> String {
    format!("{prefix}:{key}")
}

/// Assembles a [`GraphResponse`] from raw rows. Pure — no I/O, no clock read beyond the
/// `generated_at` the caller supplies — so it is unit-tested directly against hand-built
/// [`GraphRawData`] with no database.
///
/// An edge is only emitted when **both** endpoints resolve to a node this same call produced:
/// a `skill_refs`/`eval_suite_refs` entry naming a deleted or never-existed row, or a flow step
/// naming a soft-deleted agent, is a dangling ref rather than a rendering error — it simply
/// does not appear as an edge. This mirrors the registries' own fail-closed-by-omission
/// posture (`docs/agent-profile-resolution.md`) rather than inventing a new failure mode for a
/// read-only projection.
pub fn assemble_graph(data: GraphRawData, generated_at: DateTime<Utc>) -> GraphResponse {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let agent_ids: BTreeSet<Uuid> = data.agents.iter().map(|row| row.id).collect();
    let skill_ids: BTreeSet<Uuid> = data.skills.iter().map(|row| row.id).collect();
    let eval_suite_ids: BTreeSet<Uuid> = data.eval_suites.iter().map(|row| row.id).collect();
    let flow_ids: BTreeSet<Uuid> = data.flows.iter().map(|row| row.id).collect();
    let model_ids: BTreeSet<Uuid> = data.models.iter().map(|row| row.id).collect();

    for row in &data.agents {
        nodes.push(GraphNode {
            id: node_id("agent", row.id),
            node_type: GraphNodeType::Agent,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }
    for row in &data.skills {
        nodes.push(GraphNode {
            id: node_id("skill", row.id),
            node_type: GraphNodeType::Skill,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }
    for row in &data.eval_suites {
        nodes.push(GraphNode {
            id: node_id("eval_suite", row.id),
            node_type: GraphNodeType::EvalSuite,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }
    for row in &data.flows {
        nodes.push(GraphNode {
            id: node_id("flow", row.id),
            node_type: GraphNodeType::Flow,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }
    for row in &data.providers {
        nodes.push(GraphNode {
            id: node_id("provider", row.id),
            node_type: GraphNodeType::Provider,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }
    for row in &data.models {
        nodes.push(GraphNode {
            id: node_id("model", row.id),
            node_type: GraphNodeType::Model,
            label: row.display_name.clone(),
            status: Some(row.status.clone()),
        });
    }

    // agent -> skill / eval_suite / memory_scope, straight off agent_profiles' ref columns.
    let mut memory_scopes_in_use: BTreeSet<String> = BTreeSet::new();
    for agent in &data.agents {
        let agent_node = node_id("agent", agent.id);
        for skill_id in &agent.skill_refs {
            if skill_ids.contains(skill_id) {
                edges.push(GraphEdge {
                    from: agent_node.clone(),
                    to: node_id("skill", skill_id),
                    kind: GraphEdgeKind::AgentUsesSkill,
                });
            }
        }
        for eval_suite_id in &agent.eval_suite_refs {
            if eval_suite_ids.contains(eval_suite_id) {
                edges.push(GraphEdge {
                    from: agent_node.clone(),
                    to: node_id("eval_suite", eval_suite_id),
                    kind: GraphEdgeKind::AgentUsesEvalSuite,
                });
            }
        }
        for scope in memory_scopes_in(&agent.memory_scope_refs) {
            edges.push(GraphEdge {
                from: agent_node.clone(),
                to: node_id("memory_scope", &scope),
                kind: GraphEdgeKind::AgentReadsMemoryScope,
            });
            memory_scopes_in_use.insert(scope);
        }
    }

    // memory_scope nodes: one per distinct scope actually in use, not per possible scope value
    // and not per memory_records row (plan 12 §4).
    for scope in &memory_scopes_in_use {
        nodes.push(GraphNode {
            id: node_id("memory_scope", scope),
            node_type: GraphNodeType::MemoryScope,
            label: scope.clone(),
            status: None,
        });
    }

    // flow -> agent, from agent_flow_steps.
    for step in &data.flow_steps {
        if flow_ids.contains(&step.flow_id) && agent_ids.contains(&step.agent_profile_id) {
            edges.push(GraphEdge {
                from: node_id("flow", step.flow_id),
                to: node_id("agent", step.agent_profile_id),
                kind: GraphEdgeKind::FlowContainsAgent,
            });
        }
    }

    // agent -> model, from the route_definitions -> routing_policies -> provider_models chain.
    for edge in &data.agent_route_edges {
        if agent_ids.contains(&edge.agent_profile_id) && model_ids.contains(&edge.provider_model_id)
        {
            edges.push(GraphEdge {
                from: node_id("agent", edge.agent_profile_id),
                to: node_id("model", edge.provider_model_id),
                kind: GraphEdgeKind::AgentRoutesToModel,
            });
        }
    }

    GraphResponse {
        nodes,
        edges,
        generated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: Uuid, status: &str) -> AgentProfileGraphRow {
        AgentProfileGraphRow {
            id,
            display_name: format!("agent-{id}"),
            status: status.to_string(),
            skill_refs: Vec::new(),
            eval_suite_refs: Vec::new(),
            memory_scope_refs: Value::Null,
        }
    }

    fn named(id: Uuid, status: &str) -> NamedStatusGraphRow {
        NamedStatusGraphRow {
            id,
            display_name: format!("row-{id}"),
            status: status.to_string(),
        }
    }

    #[test]
    fn a_bare_agent_row_becomes_exactly_one_node_and_no_edges() {
        let agent_id = Uuid::now_v7();
        let data = GraphRawData {
            agents: vec![agent(agent_id, "active")],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, format!("agent:{agent_id}"));
        assert_eq!(graph.nodes[0].node_type, GraphNodeType::Agent);
        assert_eq!(graph.nodes[0].status.as_deref(), Some("active"));
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn agent_skill_refs_become_agent_uses_skill_edges() {
        let agent_id = Uuid::now_v7();
        let skill_id = Uuid::now_v7();
        let mut agent_row = agent(agent_id, "active");
        agent_row.skill_refs = vec![skill_id];
        let data = GraphRawData {
            agents: vec![agent_row],
            skills: vec![named(skill_id, "enabled")],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, format!("agent:{agent_id}"));
        assert_eq!(graph.edges[0].to, format!("skill:{skill_id}"));
        assert_eq!(graph.edges[0].kind, GraphEdgeKind::AgentUsesSkill);
    }

    #[test]
    fn a_skill_ref_naming_a_row_that_does_not_exist_is_a_dangling_ref_not_an_edge() {
        let agent_id = Uuid::now_v7();
        let mut agent_row = agent(agent_id, "active");
        agent_row.skill_refs = vec![Uuid::now_v7()]; // never present in `skills`
        let data = GraphRawData {
            agents: vec![agent_row],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.nodes.len(), 1, "only the agent node should exist");
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn eval_suite_refs_become_agent_uses_eval_suite_edges() {
        let agent_id = Uuid::now_v7();
        let eval_suite_id = Uuid::now_v7();
        let mut agent_row = agent(agent_id, "active");
        agent_row.eval_suite_refs = vec![eval_suite_id];
        let data = GraphRawData {
            agents: vec![agent_row],
            eval_suites: vec![named(eval_suite_id, "active")],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].kind, GraphEdgeKind::AgentUsesEvalSuite);
        assert_eq!(graph.edges[0].to, format!("eval_suite:{eval_suite_id}"));
    }

    #[test]
    fn memory_scope_refs_produce_one_synthetic_node_per_distinct_scope_in_use() {
        let agent_a = Uuid::now_v7();
        let agent_b = Uuid::now_v7();
        let mut row_a = agent(agent_a, "active");
        row_a.memory_scope_refs = json!(["conversation", "tenant_application"]);
        let mut row_b = agent(agent_b, "active");
        // Same scope as row_a's second entry — must not double the memory_scope node.
        row_b.memory_scope_refs = json!(["tenant_application"]);
        let data = GraphRawData {
            agents: vec![row_a, row_b],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());

        let memory_scope_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.node_type == GraphNodeType::MemoryScope)
            .collect();
        assert_eq!(
            memory_scope_nodes.len(),
            2,
            "conversation + tenant_application, once each"
        );
        assert!(memory_scope_nodes.iter().all(|node| node.status.is_none()));

        let memory_edges: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.kind == GraphEdgeKind::AgentReadsMemoryScope)
            .collect();
        assert_eq!(memory_edges.len(), 3, "2 scopes for a, 1 for b");
    }

    #[test]
    fn unknown_memory_scope_values_are_dropped_not_surfaced() {
        let agent_id = Uuid::now_v7();
        let mut row = agent(agent_id, "active");
        row.memory_scope_refs = json!(["conversation", "not_a_real_scope", 42, null]);
        let data = GraphRawData {
            agents: vec![row],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        let memory_scope_nodes: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| node.node_type == GraphNodeType::MemoryScope)
            .collect();
        assert_eq!(memory_scope_nodes.len(), 1);
        assert_eq!(memory_scope_nodes[0].label, "conversation");
    }

    #[test]
    fn flow_steps_become_flow_contains_agent_edges() {
        let flow_id = Uuid::now_v7();
        let agent_id = Uuid::now_v7();
        let data = GraphRawData {
            agents: vec![agent(agent_id, "active")],
            flows: vec![named(flow_id, "active")],
            flow_steps: vec![FlowStepGraphRow {
                flow_id,
                agent_profile_id: agent_id,
            }],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, format!("flow:{flow_id}"));
        assert_eq!(graph.edges[0].to, format!("agent:{agent_id}"));
        assert_eq!(graph.edges[0].kind, GraphEdgeKind::FlowContainsAgent);
    }

    #[test]
    fn a_flow_step_naming_a_soft_deleted_agent_is_a_dangling_ref_not_an_edge() {
        let flow_id = Uuid::now_v7();
        let missing_agent_id = Uuid::now_v7();
        let data = GraphRawData {
            flows: vec![named(flow_id, "active")],
            flow_steps: vec![FlowStepGraphRow {
                flow_id,
                agent_profile_id: missing_agent_id,
            }],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn agent_route_edges_become_agent_routes_to_model_edges() {
        let agent_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();
        let data = GraphRawData {
            agents: vec![agent(agent_id, "active")],
            models: vec![named(model_id, "active")],
            agent_route_edges: vec![AgentRouteEdgeRow {
                agent_profile_id: agent_id,
                provider_model_id: model_id,
            }],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, format!("agent:{agent_id}"));
        assert_eq!(graph.edges[0].to, format!("model:{model_id}"));
        assert_eq!(graph.edges[0].kind, GraphEdgeKind::AgentRoutesToModel);
    }

    #[test]
    fn an_empty_graph_is_empty_nodes_and_edges_not_an_error() {
        let graph = assemble_graph(GraphRawData::default(), Utc::now());
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn providers_and_models_become_nodes_independent_of_any_route() {
        let provider_id = Uuid::now_v7();
        let model_id = Uuid::now_v7();
        let data = GraphRawData {
            providers: vec![named(provider_id, "active")],
            models: vec![named(model_id, "active")],
            ..Default::default()
        };
        let graph = assemble_graph(data, Utc::now());
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.edges.is_empty(), "no route ties them together yet");
    }
}
