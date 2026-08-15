// Shared shapes for the playground's client components. Kept separate from
// `PlaygroundScreen.tsx` so `PlaygroundControls.tsx` does not have to import
// the orchestrator to get at the value shape it edits.

import type { ComplexityTier, DiagnosticExecutionResponse, PublicExecutionSummary } from "@/lib/types";

import type { FallbackHop } from "./runtime-events";

/**
 * Every control's CURRENT value, as strings for the numeric fields — the
 * controls are ordinary `<input type="number">`/`<select>` elements, and
 * keeping them string-controlled avoids the classic "0 vs empty" ambiguity a
 * `number | undefined` state forces onto every keystroke. Parsed to the wire
 * shape only at send time — see `lib/playground-request.ts` on the SERVER
 * side, and `toRunBody`/`toDiagnoseBody` in `PlaygroundScreen.tsx` on the
 * client side.
 */
export interface PlaygroundControlsValue {
  /** A `route_key`, not a `route_id` — `PublicResponseRequest.route` takes the key. */
  readonly route: string;
  /**
   * An `agent_profile_id`, used ONLY to filter the route list below —
   * there is no agent-profile field on the wire. See
   * `console.playground.field_agent_profile_hint`.
   */
  readonly agentProfileFilter: string;
  readonly providerId: string;
  /** The model's key (`PublicResponseRequest.model`) — what the chat path sends. */
  readonly modelKey: string;
  /**
   * The SAME model's row id (`DiagnosticExecutionRequest.provider_model_id`) —
   * what the diagnostics path sends. Kept alongside `modelKey` because the two
   * requests identify "which model" differently (a key string vs. a row uuid)
   * and only `ProviderModelRecord` carries both; see the model `<select>`'s
   * `onChange` in `PlaygroundControls.tsx`.
   */
  readonly providerModelId: string;
  readonly temperature: string;
  readonly maxTokens: string;
  /** Diagnostics-only — see `console.playground.field_priority_hint`. */
  readonly priority: string;
  /** Diagnostics-only, same gating as `priority`. */
  readonly complexityHint: ComplexityTier | "";
  readonly stream: boolean;
  readonly diagnostics: boolean;
}

/**
 * What the routing panel has to show, after a run.
 *
 *   empty      — no run yet.
 *   pending    — the run finished; the `GET .../executions/{id}` follow-up is
 *                still loading (public path only — the diagnostic path has no
 *                follow-up, its own response already carries everything).
 *   failed     — the follow-up read failed; the response text itself is
 *                still shown elsewhere, this only affects the routing panel.
 *   public     — the baseline transparency available on the normal chat path,
 *                with no extra scope: route/model/status/latency/usage plus
 *                any `fallback_selected` events observed live while streaming.
 *   diagnostic — the full result of `POST /api/v1/admin/runtime/diagnose`:
 *                candidate ranking, provider attempts, and (rendered by
 *                `PlaygroundToolCalls`) the tool-call trace.
 */
export type RoutingSummaryState =
  | { readonly kind: "empty" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed" }
  | {
      readonly kind: "public";
      readonly summary: PublicExecutionSummary;
      readonly fallbackHops: readonly FallbackHop[];
    }
  | { readonly kind: "diagnostic"; readonly result: DiagnosticExecutionResponse };

export const EMPTY_PLAYGROUND_CONTROLS: PlaygroundControlsValue = {
  route: "",
  agentProfileFilter: "",
  providerId: "",
  modelKey: "",
  providerModelId: "",
  temperature: "",
  maxTokens: "",
  priority: "",
  complexityHint: "",
  stream: true,
  diagnostics: false,
};
