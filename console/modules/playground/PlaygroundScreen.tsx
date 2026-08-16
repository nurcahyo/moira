"use client";

// The playground (issue #261): prompt in, real Moira execution out, with
// routing transparency and — only in Detailed diagnostics mode — tool-call
// visibility. See `docs/playground.md`-shaped reasoning distributed across
// this module's files; the short version:
//
//   normal send    -> `POST /api/playground/stream` (SSE, default) or
//                      `POST /api/playground/run` (non-streaming fallback
//                      toggle), then `GET /api/playground/executions/{id}`
//                      for the baseline routing summary.
//   diagnostics on -> `POST /api/playground/diagnose` instead — the only way
//                      to see candidate ranking or tool calls at all, and the
//                      only path `priority`/`complexity_hint` reach a server
//                      that reads them.
//
// This file owns ALL of that state and every fetch; `PlaygroundControls`,
// `PlaygroundRoutingSummary` and `PlaygroundToolCalls` are presentational
// (well, `PlaygroundControls` fetches its own dependent model list, the one
// exception — see its own header).

import { useRef, useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Label } from "@/components/atoms/Label";
import { Textarea } from "@/components/atoms/Textarea";
import { type ConsoleApiFailure, getJson, postJson, readConsoleApiFailure } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { readSseEnvelopes } from "@/lib/sse";
import type {
  AgentProfileRecord,
  DiagnosticExecutionResponse,
  ProviderRecord,
  PublicExecutionSummary,
  PublicOutputItem,
  PublicResponse,
  RouteDefinitionRecord,
  RuntimeEventEnvelope,
} from "@/lib/types";

import { PlaygroundControls } from "./PlaygroundControls";
import { PlaygroundRoutingSummary } from "./PlaygroundRoutingSummary";
import { PlaygroundToolCalls } from "./PlaygroundToolCalls";
import { EMPTY_PLAYGROUND_CONTROLS, type PlaygroundControlsValue, type RoutingSummaryState } from "./types";
import type { FallbackHop } from "./runtime-events";
import styles from "./PlaygroundScreen.module.css";

export interface PlaygroundScreenProps {
  readonly routes: readonly RouteDefinitionRecord[] | null;
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  readonly providers: readonly ProviderRecord[] | null;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "sending" }
  | { readonly kind: "streaming" }
  | { readonly kind: "diagnosing" }
  | { readonly kind: "done" }
  | { readonly kind: "cancelled" }
  | { readonly kind: "failed"; readonly failure: ConsoleApiFailure };

const TRANSPORT_FAILURE = (messageKey: string, status = 0): ConsoleApiFailure => ({
  messageKey,
  message: undefined,
  messageArgs: undefined,
  step: null,
  detail: null,
  status,
});

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function extractOutputText(output: readonly PublicOutputItem[]): string {
  const parts: string[] = [];
  for (const item of output) {
    for (const part of item.content) {
      if (part.type === "output_text") parts.push(part.text);
    }
  }
  return parts.join("");
}

function extractFallbackHop(payload: unknown): FallbackHop {
  const row = record(payload) ?? {};
  return {
    fromProviderId: str(row["from_provider_id"]),
    toProviderId: str(row["to_provider_id"]),
    failureClass: str(row["failure_class"]),
  };
}

/** The terminal `response.failed` event's payload carries `code`/`message` but no `message_key` — build one the same way `moira.error.<code>` is built everywhere else in this console (`tests/support/moira-stub.ts`'s `errorEnvelope`, `src/error.rs`). */
function failureFromTerminalEvent(payload: unknown): ConsoleApiFailure {
  const row = record(payload) ?? {};
  const error = record(row["error"]) ?? {};
  const code = str(error["code"]) ?? "internal_error";
  return {
    messageKey: `moira.error.${code}`,
    message: str(error["message"]) ?? undefined,
    messageArgs: undefined,
    step: null,
    detail: null,
    status: 0,
  };
}

function numberOrNull(value: string): number | null {
  if (value.trim() === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function PlaygroundScreen({ routes, agentProfiles, providers, fetchImpl }: PlaygroundScreenProps) {
  const [prompt, setPrompt] = useState("");
  const [controls, setControls] = useState<PlaygroundControlsValue>(EMPTY_PLAYGROUND_CONTROLS);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [responseText, setResponseText] = useState("");
  const [routingState, setRoutingState] = useState<RoutingSummaryState>({ kind: "empty" });
  const [toolEvents, setToolEvents] = useState<readonly RuntimeEventEnvelope[] | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const send = fetchImpl ?? globalThis.fetch;
  const busy = phase.kind === "sending" || phase.kind === "streaming" || phase.kind === "diagnosing";

  function resetRunState(): void {
    setResponseText("");
    setToolEvents(null);
    setRoutingState({ kind: "empty" });
  }

  async function loadExecutionSummary(executionId: string, hops: readonly FallbackHop[]): Promise<void> {
    setRoutingState({ kind: "pending" });
    const result = await getJson<PublicExecutionSummary>(
      `/api/playground/executions/${encodeURIComponent(executionId)}`,
      CONSOLE_MESSAGE_KEYS.playground_execution_summary_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setRoutingState({ kind: "failed" });
      return;
    }
    setRoutingState({ kind: "public", summary: result.data, fallbackHops: hops });
  }

  function runRequestBody(): Record<string, unknown> {
    return {
      prompt,
      route: controls.route === "" ? null : controls.route,
      provider: controls.providerId === "" ? null : controls.providerId,
      model: controls.modelKey === "" ? null : controls.modelKey,
      temperature: numberOrNull(controls.temperature),
      max_output_tokens: numberOrNull(controls.maxTokens),
    };
  }

  function diagnoseRequestBody(): Record<string, unknown> {
    return {
      prompt,
      route: controls.route === "" ? null : controls.route,
      provider_id: controls.providerId === "" ? null : controls.providerId,
      provider_model_id: controls.providerModelId === "" ? null : controls.providerModelId,
      temperature: numberOrNull(controls.temperature),
      max_tokens: numberOrNull(controls.maxTokens),
      priority: numberOrNull(controls.priority),
      complexity_hint: controls.complexityHint === "" ? null : controls.complexityHint,
    };
  }

  async function runNonStreaming(): Promise<void> {
    setPhase({ kind: "sending" });
    const result = await postJson<PublicResponse>(
      "/api/playground/run",
      runRequestBody(),
      CONSOLE_MESSAGE_KEYS.playground_run_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setResponseText(extractOutputText(result.data.output));
    setPhase({ kind: "done" });
    void loadExecutionSummary(result.data.execution_id, []);
  }

  async function runStreaming(): Promise<void> {
    setPhase({ kind: "streaming" });
    const controller = new AbortController();
    abortRef.current = controller;

    let response: Response;
    try {
      response = await send("/api/playground/stream", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(runRequestBody()),
        signal: controller.signal,
      });
    } catch {
      abortRef.current = null;
      if (controller.signal.aborted) {
        setPhase({ kind: "cancelled" });
        return;
      }
      setPhase({ kind: "failed", failure: TRANSPORT_FAILURE(CONSOLE_MESSAGE_KEYS.playground_stream_failed) });
      return;
    }

    if (!response.ok || response.body === null) {
      abortRef.current = null;
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        body = undefined;
      }
      setPhase({
        kind: "failed",
        failure: readConsoleApiFailure(body, CONSOLE_MESSAGE_KEYS.playground_stream_failed, response.status),
      });
      return;
    }

    let executionId: string | null = null;
    const hops: FallbackHop[] = [];
    let accumulated = "";
    let terminalFailure: ConsoleApiFailure | null = null;
    let sawTerminal = false;

    try {
      for await (const envelope of readSseEnvelopes(response.body, controller.signal)) {
        executionId = envelope.execution_id;
        if (envelope.type === "response.output_text.delta") {
          const text = str(record(envelope.payload)?.["text"]);
          if (text !== null) {
            accumulated += text;
            setResponseText(accumulated);
          }
        } else if (envelope.type === "response.fallback.selected") {
          hops.push(extractFallbackHop(envelope.payload));
        } else if (envelope.type === "response.completed") {
          sawTerminal = true;
        } else if (envelope.type === "response.cancelled") {
          sawTerminal = true;
        } else if (envelope.type === "response.failed") {
          sawTerminal = true;
          terminalFailure = failureFromTerminalEvent(envelope.payload);
        }
      }
    } catch {
      // A read error mid-stream (connection reset) is handled the same as an
      // unexpected end below — `sawTerminal` stays false either way.
    }

    abortRef.current = null;

    if (controller.signal.aborted) {
      setPhase({ kind: "cancelled" });
      return;
    }
    if (terminalFailure !== null) {
      setPhase({ kind: "failed", failure: terminalFailure });
      return;
    }
    if (!sawTerminal) {
      setPhase({ kind: "failed", failure: TRANSPORT_FAILURE(CONSOLE_MESSAGE_KEYS.playground_stream_failed) });
      return;
    }

    setPhase({ kind: "done" });
    if (executionId !== null) void loadExecutionSummary(executionId, hops);
  }

  async function runDiagnose(): Promise<void> {
    setPhase({ kind: "diagnosing" });
    const result = await postJson<DiagnosticExecutionResponse>(
      "/api/playground/diagnose",
      diagnoseRequestBody(),
      CONSOLE_MESSAGE_KEYS.playground_diagnose_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setResponseText(result.data.outcome.output_text ?? "");
    setToolEvents(result.data.events);
    setRoutingState({ kind: "diagnostic", result: result.data });
    setPhase({ kind: "done" });
  }

  async function onSend(): Promise<void> {
    if (prompt.trim() === "" || busy) return;
    resetRunState();
    if (controls.diagnostics) {
      await runDiagnose();
    } else if (controls.stream) {
      await runStreaming();
    } else {
      await runNonStreaming();
    }
  }

  function onStop(): void {
    abortRef.current?.abort();
  }

  const statusKey =
    phase.kind === "sending"
      ? CONSOLE_MESSAGE_KEYS.playground_status_sending
      : phase.kind === "streaming"
        ? CONSOLE_MESSAGE_KEYS.playground_status_streaming
        : phase.kind === "diagnosing"
          ? CONSOLE_MESSAGE_KEYS.playground_status_diagnosing
          : phase.kind === "cancelled"
            ? CONSOLE_MESSAGE_KEYS.playground_status_cancelled
            : CONSOLE_MESSAGE_KEYS.playground_status_idle;

  return (
    <div className={styles.screen}>
      <PlaygroundControls
        routes={routes}
        agentProfiles={agentProfiles}
        providers={providers}
        value={controls}
        onChange={setControls}
        disabled={busy}
        {...(fetchImpl === undefined ? {} : { fetchImpl })}
      />

      <section className={styles.promptPanel}>
        <Label htmlFor="playground-prompt">{t(CONSOLE_MESSAGE_KEYS.playground_prompt_label)}</Label>
        <Textarea
          id="playground-prompt"
          value={prompt}
          disabled={busy}
          placeholder={t(CONSOLE_MESSAGE_KEYS.playground_prompt_placeholder)}
          onChange={(event) => setPrompt(event.target.value)}
          rows={5}
        />

        <div className={styles.actions}>
          <Button
            type="button"
            variant="primary"
            loading={busy}
            disabled={prompt.trim() === ""}
            onClick={() => {
              void onSend();
            }}
          >
            {t(CONSOLE_MESSAGE_KEYS.playground_send)}
          </Button>
          {phase.kind === "streaming" && (
            <Button type="button" variant="secondary" onClick={onStop}>
              {t(CONSOLE_MESSAGE_KEYS.playground_stop)}
            </Button>
          )}
          <span className={styles.status} role="status" aria-live="polite">
            {t(statusKey)}
          </span>
        </div>

        {phase.kind === "failed" && (
          <p className={styles.problem} role="alert">
            {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
          </p>
        )}
      </section>

      <section className={styles.responsePanel} aria-label={t(CONSOLE_MESSAGE_KEYS.playground_response_heading)}>
        <h2 className={styles.responseHeading}>{t(CONSOLE_MESSAGE_KEYS.playground_response_heading)}</h2>
        {responseText === "" ? (
          <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.playground_response_empty)}</p>
        ) : (
          <pre className={styles.responseText}>{responseText}</pre>
        )}
      </section>

      <PlaygroundRoutingSummary state={routingState} />
      <PlaygroundToolCalls events={toolEvents} />
    </div>
  );
}
