// Pure extraction of the two things `POST /api/v1/admin/runtime/diagnose`
// exists for and nothing else on this console's execution surface can show:
// the ranked candidate list, and the tool-call trace. Kept dependency-free
// (no React, no `fetch`) so it is unit-testable directly against a list of
// `RuntimeEventEnvelope` fixtures — see `tests/unit/lib/playground-runtime-events.test.ts`.
//
// `RuntimeEventEnvelope.payload` is `unknown` on the wire (Moira encodes it as
// `serde_json::Value`); every field read here is read defensively, field by
// field, against the exact shapes transcribed in `src/application/execution.rs`
// — see the doc comments on `RuntimeEventEnvelope` in `lib/types.ts`.

import type { RuntimeEventEnvelope } from "@/lib/types";

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function num(value: unknown): number | null {
  return typeof value === "number" ? value : null;
}

export interface CandidateRankEntry {
  readonly providerId: string | null;
  readonly providerModelId: string | null;
  readonly candidateRank: number | null;
  readonly candidateScore: number | null;
  readonly selectionReason: string | null;
}

/**
 * The ranked candidate list, from the ONE `candidate_ranked` event a
 * diagnostic run emits (`src/application/execution.rs`: "Emitted once per
 * execution... not once per candidate"). `[]` when the event is absent —
 * every candidate failed before routing even started, or this is not a
 * diagnostic run's event list at all.
 */
export function candidateRanking(events: readonly RuntimeEventEnvelope[]): readonly CandidateRankEntry[] {
  const event = events.find((entry) => entry.event_type === "candidate_ranked");
  const payload = event === undefined ? null : record(event.payload);
  const candidates = payload === null ? undefined : payload["candidates"];
  if (!Array.isArray(candidates)) return [];
  return candidates.map((candidate) => {
    const row = record(candidate) ?? {};
    return {
      providerId: str(row["provider_id"]),
      providerModelId: str(row["provider_model_id"]),
      candidateRank: num(row["candidate_rank"]),
      candidateScore: num(row["candidate_score"]),
      selectionReason: str(row["selection_reason"]),
    };
  });
}

export interface FallbackHop {
  readonly fromProviderId: string | null;
  readonly toProviderId: string | null;
  readonly failureClass: string | null;
}

/** Every `fallback_selected` event — a retry/failover hop, in the order they happened. */
export function fallbackHops(events: readonly RuntimeEventEnvelope[]): readonly FallbackHop[] {
  return events
    .filter((event) => event.event_type === "fallback_selected")
    .map((event) => {
      const payload = record(event.payload) ?? {};
      return {
        fromProviderId: str(payload["from_provider_id"]),
        toProviderId: str(payload["to_provider_id"]),
        failureClass: str(payload["failure_class"]),
      };
    });
}

export interface ToolCallEntry {
  readonly name: string | null;
  /** The model-authored arguments, from `tool_call_started` — `null` when no started event matched. */
  readonly arguments: string | null;
  readonly outcome: string | null;
  readonly failureKind: string | null;
}

/**
 * Pair `tool_call_started` with `tool_result` by tool name, in call order.
 *
 * There is no shared identifier to join them on: `ToolResult`'s payload
 * (`src/application/execution.rs`, issue #84's comment) deliberately carries
 * no arguments, no output and no `internal_call_id` — "no arguments
 * (model-authored), no output (target-authored, possibly a credential echo)".
 * Matching on name, first-unconsumed-first, is the best available join when
 * the same tool is called more than once in one run; it is exact whenever
 * each tool name is called at most once, which is the common case.
 */
export function toolCalls(events: readonly RuntimeEventEnvelope[]): readonly ToolCallEntry[] {
  const startedByName = new Map<string, Array<{ arguments: string | null; consumed: boolean }>>();
  for (const event of events) {
    if (event.event_type !== "tool_call_started") continue;
    const payload = record(event.payload) ?? {};
    const name = str(payload["name"]) ?? "";
    const list = startedByName.get(name) ?? [];
    list.push({ arguments: str(payload["arguments"]), consumed: false });
    startedByName.set(name, list);
  }

  const results: ToolCallEntry[] = [];
  for (const event of events) {
    if (event.event_type !== "tool_result") continue;
    const payload = record(event.payload) ?? {};
    const name = str(payload["tool_name"]) ?? "";
    const candidates = startedByName.get(name) ?? [];
    const match = candidates.find((entry) => !entry.consumed);
    if (match !== undefined) match.consumed = true;
    results.push({
      name: name === "" ? null : name,
      arguments: match?.arguments ?? null,
      outcome: str(payload["outcome"]),
      failureKind: str(payload["failure_kind"]),
    });
  }
  // A started call with no matching result yet (execution aborted mid-call) is
  // still shown, so a cut-off run does not silently drop it.
  for (const [name, list] of startedByName) {
    for (const entry of list) {
      if (entry.consumed) continue;
      results.push({ name, arguments: entry.arguments, outcome: null, failureKind: null });
    }
  }
  return results;
}
