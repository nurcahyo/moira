"use client";

// Renders the derived relationship graph (plan 12 §4, issue #234) with `@xyflow/react`.
//
// ============================================================================
// WHY THIS IS AN ORGANISM
// ============================================================================
//
// It owns a third-party rendering library and its own layout computation —
// neither belongs in a feature-agnostic atom or molecule (CONVENTIONS §6 rule 2).
// The page (`app/(console)/graph/page.tsx`) does the server-side fetch and hands
// this component the already-fetched `GraphResponse`; this component has no
// network calls of its own.
//
// ============================================================================
// LAYOUT: A FIXED GRID BY NODE TYPE, NOT DAGRE
// ============================================================================
//
// `GraphNode` carries no position — the backend is explicit that the graph is a
// derived projection with no stored coordinates (plan 12 §4: "the graph has no
// stored coordinates to persist"). Rather than add a second dependency (dagre or
// ELK) for automatic layout, this component buckets nodes into a fixed column
// per `GraphNodeType` and stacks each column's members vertically. It is
// deterministic (the same graph always lays out the same way), computed rather
// than hand-placed, and adequate for the graph's actual scale — deployment-wide
// admin configuration, not a dense diagram. If that stops being true, replacing
// `layoutNodes` with a dagre-backed one is a localized change; nothing about the
// node/edge shape below depends on how positions were computed.

import { Background, Controls, type Edge, MiniMap, type Node, ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMemo } from "react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { GraphEdgeKind, GraphNode, GraphNodeType, GraphResponse } from "@/lib/types";

import styles from "./RelationshipGraph.module.css";

export interface RelationshipGraphProps {
  readonly graph: GraphResponse;
}

const COLUMN_ORDER: readonly GraphNodeType[] = [
  "provider",
  "model",
  "agent",
  "skill",
  "eval_suite",
  "flow",
  "memory_scope",
];

const COLUMN_WIDTH = 220;
const ROW_HEIGHT = 72;
const NODE_WIDTH = 180;

const NODE_TYPE_LABEL_KEYS: Record<GraphNodeType, string> = {
  agent: CONSOLE_MESSAGE_KEYS.graph_node_type_agent,
  skill: CONSOLE_MESSAGE_KEYS.graph_node_type_skill,
  eval_suite: CONSOLE_MESSAGE_KEYS.graph_node_type_eval_suite,
  flow: CONSOLE_MESSAGE_KEYS.graph_node_type_flow,
  provider: CONSOLE_MESSAGE_KEYS.graph_node_type_provider,
  model: CONSOLE_MESSAGE_KEYS.graph_node_type_model,
  memory_scope: CONSOLE_MESSAGE_KEYS.graph_node_type_memory_scope,
};

// CSS module imports type every property as `string | undefined`. Bun's test runner does not
// run the real CSS-module loader Next.js's build does (`styles.nodeAgent` reads `undefined`
// there even though the class exists in `RelationshipGraph.module.css`), so — unlike a
// dead-key check that could run at module load — these two maps are read defensively, the same
// `[...].filter(Boolean).join(" ")` pattern `components/atoms/Badge.tsx` already uses for its
// own per-tone class lookup.
const NODE_TYPE_CLASS: Record<GraphNodeType, string | undefined> = {
  agent: styles.nodeAgent,
  skill: styles.nodeSkill,
  eval_suite: styles.nodeEvalSuite,
  flow: styles.nodeFlow,
  provider: styles.nodeProvider,
  model: styles.nodeModel,
  memory_scope: styles.nodeMemoryScope,
};

const NODE_TYPE_SWATCH_CLASS: Record<GraphNodeType, string | undefined> = {
  agent: styles.swatchAgent,
  skill: styles.swatchSkill,
  eval_suite: styles.swatchEvalSuite,
  flow: styles.swatchFlow,
  provider: styles.swatchProvider,
  model: styles.swatchModel,
  memory_scope: styles.swatchMemoryScope,
};

/**
 * Line style per edge kind (plan 12 §4: "one visual style ... per edge kind (line style)").
 * `agent_routes_to_model` is dashed to set the model-routing chain apart from the
 * agent-platform reference edges, which are all solid.
 */
const EDGE_KIND_STYLE: Record<GraphEdgeKind, { readonly strokeDasharray?: string }> = {
  agent_uses_skill: {},
  agent_uses_eval_suite: {},
  agent_reads_memory_scope: { strokeDasharray: "4 2" },
  flow_contains_agent: {},
  agent_routes_to_model: { strokeDasharray: "6 3" },
};

function layoutNodes(nodes: readonly GraphNode[]): Node[] {
  const rowByColumn = new Map<GraphNodeType, number>();
  return nodes.map((node) => {
    const columnIndex = Math.max(COLUMN_ORDER.indexOf(node.type), 0);
    const row = rowByColumn.get(node.type) ?? 0;
    rowByColumn.set(node.type, row + 1);
    return {
      id: node.id,
      position: { x: columnIndex * COLUMN_WIDTH, y: row * ROW_HEIGHT },
      data: { label: node.status ? `${node.label} (${node.status})` : node.label },
      className: [styles.node, NODE_TYPE_CLASS[node.type]].filter(Boolean).join(" "),
      style: { width: NODE_WIDTH },
      draggable: false,
      connectable: false,
    };
  });
}

function layoutEdges(graph: GraphResponse): Edge[] {
  return graph.edges.map((edge, index) => ({
    // Edge ids need only be unique within one render; the backend does not mint one.
    id: `${edge.kind}:${edge.from}->${edge.to}:${index}`,
    source: edge.from,
    target: edge.to,
    style: EDGE_KIND_STYLE[edge.kind],
  }));
}

export function RelationshipGraph({ graph }: RelationshipGraphProps) {
  const nodes = useMemo(() => layoutNodes(graph.nodes), [graph.nodes]);
  const edges = useMemo(() => layoutEdges(graph), [graph]);
  const presentTypes = useMemo(
    () => COLUMN_ORDER.filter((type) => graph.nodes.some((node) => node.type === type)),
    [graph.nodes],
  );

  if (graph.nodes.length === 0) {
    return <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.graph_empty)}</p>;
  }

  return (
    <div className={styles.wrapper}>
      <div
        className={styles.canvas}
        role="img"
        aria-label={t(CONSOLE_MESSAGE_KEYS.graph_canvas_label)}
      >
        <ReactFlow
          nodes={nodes}
          edges={edges}
          fitView
          nodesDraggable={false}
          nodesConnectable={false}
        >
          <Background />
          <Controls showInteractive={false} />
          <MiniMap pannable zoomable />
        </ReactFlow>
      </div>
      <div className={styles.legend} aria-label={t(CONSOLE_MESSAGE_KEYS.graph_legend_label)}>
        <p className={styles.legendHeading}>{t(CONSOLE_MESSAGE_KEYS.graph_legend_label)}</p>
        {presentTypes.map((type) => (
          <div className={styles.legendItem} key={type}>
            <span
              className={[styles.swatch, NODE_TYPE_SWATCH_CLASS[type]].filter(Boolean).join(" ")}
              aria-hidden="true"
            />
            <span>{t(NODE_TYPE_LABEL_KEYS[type])}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
