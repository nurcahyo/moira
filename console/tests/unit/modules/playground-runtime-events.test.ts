import { describe, expect, test } from "bun:test";

import { candidateRanking, fallbackHops, toolCalls } from "@/modules/playground/runtime-events";
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

describe("candidateRanking", () => {
  test("returns [] when there is no candidate_ranked event", () => {
    expect(candidateRanking([envelope("route_selected", {})])).toEqual([]);
  });

  test("extracts every candidate from the one candidate_ranked event", () => {
    const events = [
      envelope("candidate_ranked", {
        candidates: [
          {
            provider_id: "p1",
            provider_model_id: "m1",
            candidate_rank: 0,
            candidate_score: null,
            selection_reason: "priority",
          },
          {
            provider_id: "p2",
            provider_model_id: "m2",
            candidate_rank: 1,
            candidate_score: 0.5,
            selection_reason: "fallback_after_failure",
          },
        ],
      }),
    ];
    expect(candidateRanking(events)).toEqual([
      { providerId: "p1", providerModelId: "m1", candidateRank: 0, candidateScore: null, selectionReason: "priority" },
      { providerId: "p2", providerModelId: "m2", candidateRank: 1, candidateScore: 0.5, selectionReason: "fallback_after_failure" },
    ]);
  });

  test("returns [] rather than throwing when the payload is malformed", () => {
    expect(candidateRanking([envelope("candidate_ranked", { candidates: "not-an-array" })])).toEqual([]);
    expect(candidateRanking([envelope("candidate_ranked", null)])).toEqual([]);
  });
});

describe("fallbackHops", () => {
  test("extracts every fallback_selected event in order", () => {
    const events = [
      envelope("fallback_selected", { from_provider_id: "p1", to_provider_id: "p2", failure_class: "provider_timeout" }),
      envelope("output_text_delta", { text: "hi" }),
      envelope("fallback_selected", { from_provider_id: "p2", to_provider_id: null, failure_class: "provider_rate_limited" }),
    ];
    expect(fallbackHops(events)).toEqual([
      { fromProviderId: "p1", toProviderId: "p2", failureClass: "provider_timeout" },
      { fromProviderId: "p2", toProviderId: null, failureClass: "provider_rate_limited" },
    ]);
  });
});

describe("toolCalls", () => {
  test("pairs a single tool_call_started with its tool_result by name", () => {
    const events = [
      envelope("tool_call_started", { internal_call_id: "c1", name: "lookup_weather", arguments: '{"city":"Paris"}' }),
      envelope("tool_result", { attempt_id: "a1", tool_name: "lookup_weather", outcome: "success", failure_kind: null }),
    ];
    expect(toolCalls(events)).toEqual([
      { name: "lookup_weather", arguments: '{"city":"Paris"}', outcome: "success", failureKind: null },
    ]);
  });

  test("matches repeated calls of the same tool in order, first-unconsumed-first", () => {
    const events = [
      envelope("tool_call_started", { name: "search", arguments: "1" }),
      envelope("tool_call_started", { name: "search", arguments: "2" }),
      envelope("tool_result", { tool_name: "search", outcome: "success" }),
      envelope("tool_result", { tool_name: "search", outcome: "failure", failure_kind: "guard_denied" }),
    ];
    expect(toolCalls(events)).toEqual([
      { name: "search", arguments: "1", outcome: "success", failureKind: null },
      { name: "search", arguments: "2", outcome: "failure", failureKind: "guard_denied" },
    ]);
  });

  test("shows a started call with no matching result yet", () => {
    const events = [envelope("tool_call_started", { name: "long_running", arguments: "{}" })];
    expect(toolCalls(events)).toEqual([{ name: "long_running", arguments: "{}", outcome: null, failureKind: null }]);
  });

  test("returns [] when there are no tool events at all", () => {
    expect(toolCalls([envelope("route_selected", {})])).toEqual([]);
  });
});
