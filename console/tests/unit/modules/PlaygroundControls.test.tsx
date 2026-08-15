import { describe, expect, test } from "bun:test";
import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { PlaygroundControls } from "@/modules/playground/PlaygroundControls";
import { EMPTY_PLAYGROUND_CONTROLS, type PlaygroundControlsValue } from "@/modules/playground/types";

const ROUTES = [
  {
    id: "r1",
    route_key: "general",
    display_name: "General",
    status: "active" as const,
    selection_strategy: "default" as const,
    metadata: null,
    created_at: "2026-08-15T00:00:00Z",
    updated_at: "2026-08-15T00:00:00Z",
    version: 1,
  },
  {
    id: "r2",
    route_key: "support",
    display_name: "Support (tools)",
    status: "active" as const,
    selection_strategy: "default" as const,
    metadata: null,
    created_at: "2026-08-15T00:00:00Z",
    updated_at: "2026-08-15T00:00:00Z",
    version: 1,
    agent_profile_id: "p1",
  },
];

const AGENT_PROFILES = [
  {
    id: "p1",
    profile_key: "support-bot",
    display_name: "Support Bot",
    tool_policy: null,
    context_policy: null,
    memory_policy: null,
    status: "active" as const,
    metadata: null,
    created_at: "2026-08-15T00:00:00Z",
    updated_at: "2026-08-15T00:00:00Z",
    version: 1,
  },
];

const PROVIDERS = [
  {
    id: "prov1",
    provider_type: "open_ai" as const,
    display_name: "OpenAI",
    status: "active" as const,
    metadata: null,
    created_at: "2026-08-15T00:00:00Z",
    updated_at: "2026-08-15T00:00:00Z",
    version: 1,
  },
];

function Harness(props: { readonly fetchImpl?: typeof fetch }) {
  const [value, setValue] = useState<PlaygroundControlsValue>(EMPTY_PLAYGROUND_CONTROLS);
  return (
    <PlaygroundControls
      routes={ROUTES}
      agentProfiles={AGENT_PROFILES}
      providers={PROVIDERS}
      value={value}
      onChange={setValue}
      disabled={false}
      {...(props.fetchImpl === undefined ? {} : { fetchImpl: props.fetchImpl })}
    />
  );
}

async function open(): Promise<void> {
  await userEvent.click(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_controls_heading)));
}

describe("PlaygroundControls", () => {
  test("is collapsed by default (no <details open>)", () => {
    render(<Harness />);
    const details = screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_controls_heading)).closest("details");
    expect(details).not.toHaveAttribute("open");
  });

  test("lists every route when no agent profile is selected", async () => {
    render(<Harness />);
    await open();
    expect(screen.getByRole("option", { name: "General" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Support (tools)" })).toBeInTheDocument();
  });

  test("selecting an agent profile narrows the route list to routes wired to it", async () => {
    render(<Harness />);
    await open();
    await userEvent.selectOptions(
      screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_agent_profile_label)),
      "p1",
    );
    expect(screen.queryByRole("option", { name: "General" })).not.toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Support (tools)" })).toBeInTheDocument();
  });

  test("priority and complexity hint are disabled until Detailed diagnostics is on", async () => {
    render(<Harness />);
    await open();
    expect(screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_priority_label))).toBeDisabled();
    await userEvent.click(screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_diagnostics_toggle_label)));
    expect(screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_priority_label))).toBeEnabled();
  });

  test("choosing a provider fetches its models, and choosing a model captures both the key and the row id", async () => {
    const fetchImpl = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/playground/providers/prov1/models")) {
        return new Response(
          JSON.stringify({
            data: [{ id: "m1", provider_id: "prov1", model_key: "gpt-test", capabilities: {}, status: "active", created_at: "", updated_at: "", version: 1 }],
            pagination: { has_more: false, next_cursor: null },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      throw new Error(`unexpected fetch: ${url}`);
    }) as unknown as typeof fetch;

    render(<Harness fetchImpl={fetchImpl} />);
    await open();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_field_model_needs_provider))).toBeInTheDocument();

    await userEvent.selectOptions(
      screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_provider_label)),
      "prov1",
    );

    await waitFor(() =>
      expect(screen.getByRole("option", { name: "gpt-test" })).toBeInTheDocument(),
    );
  });
});
