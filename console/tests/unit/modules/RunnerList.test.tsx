// `RunnerList` — presentational, so this file only asserts on what it renders
// given a set of records: the empty state, the columns, and — the property
// CONVENTIONS calls out explicitly — that a tenant's runner is visibly
// distinguishable from the platform's, in this table.

import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { ClaudeRunnerRecord } from "@/lib/types";
import { RunnerList } from "@/modules/runners/RunnerList";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

function runner(overrides: Partial<ClaudeRunnerRecord> = {}): ClaudeRunnerRecord {
  return {
    id: "runner-1",
    label: "claude-platform",
    runner_reference: "ref-1",
    state: "provisioning",
    scope: { type: "global" },
    metadata: {},
    created_at: "2026-08-16T00:00:00Z",
    updated_at: "2026-08-16T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

describe("RunnerList", () => {
  test("an empty list renders the empty-state copy, not a table", () => {
    render(<RunnerList runners={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.runners_list_empty))).toBeDefined();
    expect(screen.queryByRole("table")).toBeNull();
  });

  test("a runner's own label is rendered", () => {
    render(<RunnerList runners={[runner({ label: "claude-nightly" })]} />);
    expect(screen.getByText("claude-nightly")).toBeDefined();
  });

  test("a link to the runner's detail page is rendered, by id", () => {
    render(<RunnerList runners={[runner({ id: "runner-xyz" })]} />);
    const link = screen.getByRole("link", { name: copy(CONSOLE_MESSAGE_KEYS.runners_view) });
    expect(link.getAttribute("href")).toBe("/runners/runner-xyz");
  });

  test("the platform account and a tenant's account render VISIBLY DIFFERENT badge text", () => {
    render(
      <RunnerList
        runners={[
          runner({ id: "runner-platform", scope: { type: "global" } }),
          runner({
            id: "runner-tenant",
            label: "claude-acme",
            scope: { type: "tenant", external_tenant_id: "acme" },
          }),
        ]}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.runners_scope_platform))).toBeDefined();
    // Interpolated — the catalog message carries `{tenant_id}`, not the literal.
    expect(screen.getByText("Tenant: acme")).toBeDefined();
  });

  test("every declared lifecycle state has a distinct, rendered badge label", () => {
    const states: readonly ClaudeRunnerRecord["state"][] = [
      "provisioning",
      "awaiting_authorization",
      "exchanging",
      "ready",
      "linked",
      "failed",
      "expired",
    ];
    render(
      <RunnerList
        runners={states.map((state, index) => runner({ id: `runner-${index}`, state }))}
      />,
    );
    const labels = [
      CONSOLE_MESSAGE_KEYS.runners_state_provisioning,
      CONSOLE_MESSAGE_KEYS.runners_state_awaiting_authorization,
      CONSOLE_MESSAGE_KEYS.runners_state_exchanging,
      CONSOLE_MESSAGE_KEYS.runners_state_ready,
      CONSOLE_MESSAGE_KEYS.runners_state_linked,
      CONSOLE_MESSAGE_KEYS.runners_state_failed,
      CONSOLE_MESSAGE_KEYS.runners_state_expired,
    ].map(copy);
    for (const label of labels) {
      expect(screen.getByText(label)).toBeDefined();
    }
    // Distinct, not merely present: the whole point of a per-state badge.
    expect(new Set(labels).size).toBe(labels.length);
  });
});
