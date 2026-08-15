// The `/flows` organisms: the step builder, the create-flow form, the flow
// list (edit/delete/expand), and the on-demand run-history panel.
//
// Same standard as `LlmScreen.test.tsx`: every assertion compares rendered
// text to `CONSOLE_CATALOG[key].message`, never an English literal.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { AgentFlowRecord, AgentFlowStepCreateRequest, AgentProfileRecord } from "@/lib/types";
import { FlowCreateForm } from "@/modules/flows/FlowCreateForm";
import { FlowList } from "@/modules/flows/FlowList";
import { FlowRunsPanel } from "@/modules/flows/FlowRunsPanel";
import { FlowStepsBuilder } from "@/modules/flows/FlowStepsBuilder";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

afterEach(cleanup);

function agentProfile(overrides: Partial<AgentProfileRecord> = {}): AgentProfileRecord {
  return {
    id: "profile-1",
    profile_key: "support_agent",
    display_name: "Support agent",
    tool_policy: {},
    context_policy: {},
    memory_policy: {},
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function flow(overrides: Partial<AgentFlowRecord> = {}): AgentFlowRecord {
  return {
    id: "33333333-3333-4333-8333-333333333333",
    flow_key: "escalation",
    display_name: "Escalation flow",
    description: "Escalates a ticket through two agents.",
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    steps: [],
    ...overrides,
  };
}

function scriptedFetch(
  responses: ReadonlyArray<{ status: number; body: unknown }>,
): typeof fetch & { calls: Array<{ url: string; method: string; body: unknown }> } {
  const calls: Array<{ url: string; method: string; body: unknown }> = [];
  let index = 0;
  const impl = (async (url: RequestInfo | URL, init?: RequestInit) => {
    calls.push({
      url: String(url),
      method: (init?.method ?? "GET").toUpperCase(),
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    });
    const next = responses[Math.min(index, responses.length - 1)]!;
    index += 1;
    return new Response(next.status === 204 ? null : JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch & { calls: typeof calls };
  impl.calls = calls;
  return impl;
}

/* -------------------------------------------------------------------------- */
/* FlowStepsBuilder                                                          */
/* -------------------------------------------------------------------------- */

describe("FlowStepsBuilder", () => {
  test("an empty step list says so", () => {
    render(<FlowStepsBuilder steps={[]} onStepsChange={() => {}} agentProfiles={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.flows_steps_empty))).toBeDefined();
  });

  test("Add step appends a blank row via onStepsChange", async () => {
    let steps: readonly AgentFlowStepCreateRequest[] = [];
    const { rerender } = render(
      <FlowStepsBuilder
        steps={steps}
        onStepsChange={(next) => {
          steps = next;
        }}
        agentProfiles={[agentProfile()]}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_step_add) }));
    expect(steps).toHaveLength(1);
    rerender(<FlowStepsBuilder steps={steps} onStepsChange={() => {}} agentProfiles={[agentProfile()]} />);
    expect(screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.flows_step_key_label))).toBeDefined();
  });

  test("the agent-profile picker degrades to a free-text field when the list failed to load", () => {
    render(
      <FlowStepsBuilder
        steps={[{ step_key: "step-1", step_order: 0, agent_profile_id: "" }]}
        onStepsChange={() => {}}
        agentProfiles={null}
      />,
    );
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.flows_step_agent_profiles_load_failed))).toBeDefined();
    // A free-text input, not a <select>, for the agent-profile field.
    const label = screen.getByText(copy(CONSOLE_MESSAGE_KEYS.flows_step_agent_profile_label));
    const field = label.parentElement?.querySelector("input, select");
    expect(field?.tagName).toBe("INPUT");
  });
});

/* -------------------------------------------------------------------------- */
/* FlowCreateForm                                                            */
/* -------------------------------------------------------------------------- */

describe("FlowCreateForm", () => {
  test("it posts flow_key, display_name and the (empty) step list", async () => {
    const send = scriptedFetch([{ status: 201, body: flow() }]);
    const created: string[] = [];
    render(<FlowCreateForm agentProfiles={[agentProfile()]} fetchImpl={send} onCreated={() => created.push("yes")} />);

    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.flows_field_flow_key_label)}`)),
      "escalation",
    );
    await userEvent.type(
      screen.getByLabelText(new RegExp(`^${copy(CONSOLE_MESSAGE_KEYS.flows_field_display_name_label)}`)),
      "Escalation flow",
    );
    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_create_submit) }),
    );

    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.url).toBe("/api/flows");
    expect(send.calls[0]?.body).toMatchObject({ flow_key: "escalation", display_name: "Escalation flow", steps: [] });
    expect(created).toEqual(["yes"]);
  });
});

/* -------------------------------------------------------------------------- */
/* FlowList                                                                  */
/* -------------------------------------------------------------------------- */

describe("FlowList", () => {
  test("an empty deployment gets an honest empty state", () => {
    render(<FlowList flows={[]} agentProfiles={[]} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.flows_list_empty))).toBeDefined();
  });

  test("delete asks first, and only confirming sends DELETE — there is no undo on this surface", async () => {
    const send = scriptedFetch([{ status: 204, body: null }]);
    const changed: string[] = [];
    render(<FlowList flows={[flow()]} agentProfiles={[]} fetchImpl={send} onChanged={() => changed.push("yes")} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_delete) }));
    expect(screen.getByRole("alert")).toHaveTextContent(
      copy(CONSOLE_MESSAGE_KEYS.flows_delete_confirm_body),
    );
    expect(send.calls).toEqual([]);

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_delete_confirm_action) }),
    );
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]).toMatchObject({
      url: "/api/flows/33333333-3333-4333-8333-333333333333",
      method: "DELETE",
    });
    expect(changed).toEqual(["yes"]);
  });

  test("edit pre-populates the step builder from the flow's own steps and PATCHes the whole list on save", async () => {
    const send = scriptedFetch([{ status: 200, body: flow() }]);
    const withStep = flow({
      steps: [
        {
          id: "step-1",
          flow_id: "33333333-3333-4333-8333-333333333333",
          step_key: "first",
          step_order: 0,
          agent_profile_id: "profile-1",
          on_failure: "abort",
          input_mapping: {},
          metadata: {},
          created_at: "2026-08-14T00:00:00Z",
        },
      ],
    });
    render(<FlowList flows={[withStep]} agentProfiles={[agentProfile()]} fetchImpl={send} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_edit) }));
    expect((screen.getByLabelText(copy(CONSOLE_MESSAGE_KEYS.flows_step_key_label)) as HTMLInputElement).value).toBe(
      "first",
    );

    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_edit_save) }));
    await waitFor(() => expect(send.calls.length).toBe(1));
    expect(send.calls[0]?.method).toBe("PATCH");
    expect(send.calls[0]?.body).toMatchObject({
      steps: [{ step_key: "first", step_order: 0, agent_profile_id: "profile-1", on_failure: "abort" }],
    });
  });

  test("expanding a flow renders its run-history panel", async () => {
    const send = scriptedFetch([{ status: 200, body: { data: [], pagination: { has_more: false } } }]);
    render(<FlowList flows={[flow()]} agentProfiles={[]} fetchImpl={send} />);
    await userEvent.click(screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_expand) }));
    expect(await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.flows_runs_heading))).toBeDefined();
  });
});

/* -------------------------------------------------------------------------- */
/* FlowRunsPanel                                                             */
/* -------------------------------------------------------------------------- */

describe("FlowRunsPanel — the deferred trigger", () => {
  test("a 501 stub is tolerated gracefully, rendered as a notice rather than a crash", async () => {
    const send = scriptedFetch([
      { status: 200, body: { data: [], pagination: { has_more: false } } },
      {
        status: 501,
        body: { error: { code: "flow_run_not_available", message_key: CONSOLE_MESSAGE_KEYS.flows_run_not_available } },
      },
    ]);
    render(<FlowRunsPanel flowId="f1" fetchImpl={send} />);
    await waitFor(() => expect(send.calls.length).toBe(1));

    await userEvent.click(
      screen.getByRole("button", { name: copy(CONSOLE_MESSAGE_KEYS.flows_run_trigger) }),
    );
    const notice = await screen.findByText(copy(CONSOLE_MESSAGE_KEYS.flows_run_not_available));
    expect(notice.getAttribute("role")).toBe("status");
  });
});
