// The /graph organism (plan 12 §4, issue #234).
//
// Same standard as `AdminsScreen.test.tsx`: every assertion compares rendered text to
// `CONSOLE_CATALOG[key].message`, read from the catalog at test time, never an English literal
// — so a copy edit cannot silently break this test while the component still calls `t()`.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { GraphEdge, GraphNode, GraphResponse } from "@/lib/types";
import { RelationshipGraph } from "@/modules/graph/RelationshipGraph";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

function node(overrides: Partial<GraphNode> & Pick<GraphNode, "id" | "type" | "label">): GraphNode {
  return { status: null, ...overrides };
}

function graph(nodes: readonly GraphNode[], edges: readonly GraphEdge[] = []): GraphResponse {
  return { nodes: [...nodes], edges: [...edges], generated_at: "2026-08-15T00:00:00Z" };
}

describe("RelationshipGraph", () => {
  test("an empty graph renders the empty-state copy, not a blank canvas", () => {
    render(<RelationshipGraph graph={graph([])} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.graph_empty))).toBeTruthy();
  });

  test("a non-empty graph renders the canvas and a legend entry per node type present", () => {
    const data = graph(
      [
        node({ id: "agent:a1", type: "agent", label: "Support Agent", status: "active" }),
        node({ id: "skill:s1", type: "skill", label: "Order Lookup", status: "enabled" }),
      ],
      [{ from: "agent:a1", to: "skill:s1", kind: "agent_uses_skill" }],
    );
    render(<RelationshipGraph graph={data} />);

    expect(
      screen.getByRole("img", { name: copy(CONSOLE_MESSAGE_KEYS.graph_canvas_label) }),
    ).toBeTruthy();

    // Legend shows exactly the node types present in this graph — agent and skill, not the
    // other five kinds the graph can carry.
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.graph_node_type_agent))).toBeTruthy();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.graph_node_type_skill))).toBeTruthy();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.graph_node_type_flow))).toBeNull();
    expect(screen.queryByText(copy(CONSOLE_MESSAGE_KEYS.graph_node_type_provider))).toBeNull();

    // The node labels themselves are rendered by react-flow into the canvas, status appended.
    expect(screen.getByText("Support Agent (active)")).toBeTruthy();
    expect(screen.getByText("Order Lookup (enabled)")).toBeTruthy();
  });

  test("a synthetic memory_scope node (no status) renders its bare label", () => {
    const data = graph([
      node({
        id: "memory_scope:tenant_application",
        type: "memory_scope",
        label: "tenant_application",
      }),
    ]);
    render(<RelationshipGraph graph={data} />);
    expect(screen.getByText("tenant_application")).toBeTruthy();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.graph_node_type_memory_scope))).toBeTruthy();
  });
});
