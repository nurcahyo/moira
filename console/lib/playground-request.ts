// Shared body-shaping for the playground's three execution BFF routes
// (`app/api/playground/run/route.ts`, `.../stream/route.ts`,
// `.../diagnose/route.ts`) — one place that reads a client-supplied JSON body
// into a typed input, and one place that turns that input into the exact
// wire shape Moira expects, so the three routes cannot silently drift on
// what a control means.
//
// CLIENT-SAFE by construction (no `process.env`, no credential, no `fetch`):
// every function here is pure. It is imported only from route handlers today,
// but nothing about it requires that to stay true.

import type { ComplexityTier, DiagnosticExecutionRequest, PublicResponseRequest } from "./types";

function trimmedStringOrNull(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

function finiteNumberOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/* -------------------------------------------------------------------------- */
/* The chat path — `POST /api/v1/responses` and `.../responses/stream`        */
/* -------------------------------------------------------------------------- */

export interface PlaygroundRunInput {
  readonly prompt: string;
  readonly route: string | null;
  readonly provider: string | null;
  readonly model: string | null;
  readonly temperature: number | null;
  readonly maxOutputTokens: number | null;
}

/** Read a client-supplied JSON body, or `null` when the prompt is missing or blank. */
export function readPlaygroundRunBody(body: Record<string, unknown>): PlaygroundRunInput | null {
  const prompt = typeof body["prompt"] === "string" ? body["prompt"] : "";
  if (prompt.trim() === "") return null;
  return {
    prompt,
    route: trimmedStringOrNull(body["route"]),
    provider: trimmedStringOrNull(body["provider"]),
    model: trimmedStringOrNull(body["model"]),
    temperature: finiteNumberOrNull(body["temperature"]),
    maxOutputTokens: finiteNumberOrNull(body["max_output_tokens"]),
  };
}

/**
 * Build the exact `PublicResponseRequest` Moira expects.
 * `additionalProperties: false` on that schema — every key here is one it
 * actually declares (`docs/openapi.json`), and nothing else may be added
 * without a matching schema change. There is deliberately no `agent_profile`
 * field: the public request DTO has none (`src/application/public.rs`
 * hardcodes `agent_profile_hint: None` regardless of what a caller sends) — a
 * route's OWN `agent_profile_id` is the only thing that decides whether the
 * tool loop runs. See `modules/playground/PlaygroundControls.tsx`.
 */
export function buildPublicResponseRequest(input: PlaygroundRunInput): PublicResponseRequest {
  return {
    input: [{ role: "user", content: [{ type: "input_text", text: input.prompt }] }],
    route: input.route,
    provider: input.provider,
    model: input.model,
    temperature: input.temperature,
    max_output_tokens: input.maxOutputTokens,
  };
}

/* -------------------------------------------------------------------------- */
/* The diagnostics path — `POST /api/v1/admin/runtime/diagnose`               */
/* -------------------------------------------------------------------------- */

const COMPLEXITY_TIERS: readonly ComplexityTier[] = ["trivial", "standard", "heavy"];

function complexityTierOrNull(value: unknown): ComplexityTier | null {
  return typeof value === "string" && (COMPLEXITY_TIERS as readonly string[]).includes(value)
    ? (value as ComplexityTier)
    : null;
}

export interface PlaygroundDiagnoseInput {
  readonly prompt: string;
  readonly route: string | null;
  readonly providerId: string | null;
  readonly providerModelId: string | null;
  readonly temperature: number | null;
  readonly maxTokens: number | null;
  /** Gated server-side on `moira:execution:override-priority` — see `diagnoseRuntime`'s doc comment. */
  readonly priority: number | null;
  /** Gated server-side on `moira:execution:override-complexity-hint`. */
  readonly complexityHint: ComplexityTier | null;
}

/** Read a client-supplied JSON body, or `null` when the prompt is missing or blank. */
export function readPlaygroundDiagnoseBody(body: Record<string, unknown>): PlaygroundDiagnoseInput | null {
  const prompt = typeof body["prompt"] === "string" ? body["prompt"] : "";
  if (prompt.trim() === "") return null;
  return {
    prompt,
    route: trimmedStringOrNull(body["route"]),
    providerId: trimmedStringOrNull(body["provider_id"]),
    providerModelId: trimmedStringOrNull(body["provider_model_id"]),
    temperature: finiteNumberOrNull(body["temperature"]),
    maxTokens: finiteNumberOrNull(body["max_tokens"]),
    priority: finiteNumberOrNull(body["priority"]),
    complexityHint: complexityTierOrNull(body["complexity_hint"]),
  };
}

/**
 * Build the exact `DiagnosticExecutionRequest` Moira expects.
 * `additionalProperties: false` (`#[serde(deny_unknown_fields)]` on
 * `DiagnosticExecutionRequest`, `src/domain/runtime.rs`) — every key here is
 * one it declares. `prompt` is a single string on this endpoint, unlike the
 * chat path's `input: PublicInputMessage[]` — the diagnose request has no
 * conversation-turn shape at all.
 */
export function buildDiagnosticExecutionRequest(
  input: PlaygroundDiagnoseInput,
): DiagnosticExecutionRequest {
  return {
    prompt: input.prompt,
    route: input.route,
    provider_id: input.providerId,
    provider_model_id: input.providerModelId,
    stream: false,
    options: {
      temperature: input.temperature,
      max_tokens: input.maxTokens,
      priority: input.priority,
      complexity_hint: input.complexityHint,
    },
  };
}
