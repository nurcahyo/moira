import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { PlaygroundToolCalls } from "@/modules/playground/PlaygroundToolCalls";
import type { RuntimeEventEnvelope } from "@/lib/types";

function envelope(eventType: RuntimeEventEnvelope["event_type"], payload: unknown): RuntimeEventEnvelope {
  return {
    request_id: "req_1",
    execution_id: "exec_1",
    sequence: 0,
    timestamp: "2026-08-15T00:00:00Z",
    event_type: eventType,
    payload: payload as RuntimeEventEnvelope["payload"],
  };
}

describe("PlaygroundToolCalls", () => {
  test("outside diagnostics mode (events=null), shows the honesty note rather than an empty list", () => {
    render(<PlaygroundToolCalls events={null} />);
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_unavailable_note))).toBeInTheDocument();
    expect(screen.queryByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_empty))).not.toBeInTheDocument();
  });

  test("in diagnostics mode with no tool events, shows the genuinely-empty state, not the honesty note", () => {
    render(<PlaygroundToolCalls events={[envelope("route_selected", {})]} />);
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_empty))).toBeInTheDocument();
    expect(screen.queryByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_unavailable_note))).not.toBeInTheDocument();
  });

  test("renders each tool call's name, arguments and outcome", () => {
    const events = [
      envelope("tool_call_started", { internal_call_id: "c1", name: "lookup_weather", arguments: '{"city":"Paris"}' }),
      envelope("tool_result", { attempt_id: "a1", tool_name: "lookup_weather", outcome: "success", failure_kind: null }),
    ];
    render(<PlaygroundToolCalls events={events} />);
    expect(screen.getByText("lookup_weather")).toBeInTheDocument();
    expect(screen.getByText('{"city":"Paris"}')).toBeInTheDocument();
    expect(screen.getByText("success")).toBeInTheDocument();
  });
});
