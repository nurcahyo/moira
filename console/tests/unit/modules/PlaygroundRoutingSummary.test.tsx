import { describe, expect, test } from "bun:test";
import { render, screen } from "@testing-library/react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { PlaygroundRoutingSummary } from "@/modules/playground/PlaygroundRoutingSummary";
import type { RoutingSummaryState } from "@/modules/playground/types";

describe("PlaygroundRoutingSummary", () => {
  test("empty state before any run", () => {
    render(<PlaygroundRoutingSummary state={{ kind: "empty" }} />);
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_unavailable))).toBeInTheDocument();
  });

  test("pending state while the follow-up loads", () => {
    render(<PlaygroundRoutingSummary state={{ kind: "pending" }} />);
    expect(screen.getByRole("status")).toHaveTextContent(
      t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_pending),
    );
  });

  test("failed state renders as an alert, not silently nothing", () => {
    render(<PlaygroundRoutingSummary state={{ kind: "failed" }} />);
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  test("public state shows route, model, status, latency, attempts, usage and fallback hops", () => {
    const state: RoutingSummaryState = {
      kind: "public",
      summary: {
        execution_id: "exec_1",
        response_id: "resp_1",
        request_id: "req_1",
        status: "completed",
        attempt_count: 2,
        usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 },
        latency_ms: 842,
        route: { id: "r1", key: "general" },
        model: { id: "m1", provider: "open_ai", key: "gpt-test" },
      },
      fallbackHops: [{ fromProviderId: "p1", toProviderId: "p2", failureClass: "provider_timeout" }],
    };
    render(<PlaygroundRoutingSummary state={state} />);
    expect(screen.getByText("general")).toBeInTheDocument();
    expect(screen.getByText("gpt-test")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByText("842 ms")).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_routing_fallback_heading))).toBeInTheDocument();
    expect(screen.getByText(/p1 → p2/)).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_public_note))).toBeInTheDocument();
  });

  test("diagnostic state shows candidate ranking and provider attempts", () => {
    const state: RoutingSummaryState = {
      kind: "diagnostic",
      result: {
        outcome: {
          request_id: "req_1",
          execution_id: "11111111-1111-4111-8111-111111111111",
          status: "succeeded",
          usage: {},
          attempts: [
            {
              attempt_id: "a1",
              attempt_number: 1,
              provider_id: "p1",
              provider_model_id: "m1",
              credential_id: "c1",
              status: "succeeded",
              usage: { input_tokens: 3 },
              latency_ms: 55,
            },
          ],
        },
        events: [
          {
            request_id: "req_1",
            execution_id: "11111111-1111-4111-8111-111111111111",
            sequence: 0,
            timestamp: "2026-08-15T00:00:00Z",
            event_type: "candidate_ranked",
            payload: {
              candidates: [
                {
                  provider_id: "p1",
                  provider_model_id: "m1",
                  candidate_rank: 0,
                  candidate_score: null,
                  selection_reason: "priority",
                },
              ],
            },
          },
        ],
      },
    };
    render(<PlaygroundRoutingSummary state={state} />);
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_candidates_heading))).toBeInTheDocument();
    expect(screen.getByText("priority")).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_attempts_heading))).toBeInTheDocument();
    expect(screen.getByText("55 ms")).toBeInTheDocument();
  });
});
