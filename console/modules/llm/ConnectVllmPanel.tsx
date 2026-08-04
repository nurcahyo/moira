"use client";

// The "Connect local vLLM" shortcut.
//
// ============================================================================
// TWO CLICKS, AND THE FIRST ONE WRITES NOTHING
// ============================================================================
//
// Ask the endpoint what it serves, show the answer, and only then register it.
// The alternative — one button that discovers and provisions together — would
// make the operator find out what was created by reading the result, and a
// mistyped address would leave a provider row behind before anyone could say it
// was wrong.
//
// The endpoint address is pre-filled from a CONSTANT
// (`lib/llm-view.ts: LOCAL_VLLM_BASE_URL`), rendered as a FIELD VALUE. It is
// deliberately not catalog copy: the catalog gate refuses any message containing
// a URL, and it is right to — a translated string that names a deployment's
// address has stopped being copy and become configuration.
//
// ============================================================================
// NEITHER STAGE TALKS TO THE ENDPOINT FROM HERE
// ============================================================================
//
// Both post to `/api/llm/connect-vllm`. The outbound call to the operator's own
// network is made by the BFF, behind the session gate, bounded and validated.
// This component never learns the endpoint's response shape, only the list of
// model ids the server was willing to vouch for.
//
// ============================================================================
// A PARTIAL CHAIN IS RENDERED, NOT SWALLOWED
// ============================================================================
//
// When the chain stops part-way the server answers 409 with the step, the state
// and the trace. All three are shown: the trace says what exists, so clicking
// again is an informed decision rather than a hopeful one. Every step is
// reuse-first, so clicking again continues instead of duplicating.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { LLM_ENDPOINTS, type LlmConnectStepView } from "@/lib/llm-view";

import { postJson, type LlmFailure } from "./request";
import styles from "./ConnectVllmPanel.module.css";

export interface ConnectVllmPanelProps {
  /** `LOCAL_VLLM_BASE_URL`, passed in so the page owns the default. */
  readonly defaultBaseUrl: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onConnected?: () => void;
}

interface DiscoveryResponse {
  readonly base_url: string;
  readonly models: readonly string[];
}

interface ConnectResponse {
  readonly provider_id: string;
  readonly trace: readonly LlmConnectStepView[];
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "discovering" }
  | { readonly kind: "connecting" }
  | { readonly kind: "connected" }
  | { readonly kind: "failed"; readonly failure: LlmFailure };

/** The catalog key for a chain step's name. Unknown steps fall back to the generic label. */
export function stepLabelKey(step: string): string {
  switch (step) {
    case "provider":
      return CONSOLE_MESSAGE_KEYS.llm_step_provider;
    case "provider_model":
      return CONSOLE_MESSAGE_KEYS.llm_step_provider_model;
    case "provider_credential":
      return CONSOLE_MESSAGE_KEYS.llm_step_provider_credential;
    case "provider_enable":
      return CONSOLE_MESSAGE_KEYS.llm_step_provider_enable;
    case "routing_policy":
      return CONSOLE_MESSAGE_KEYS.llm_step_routing_policy;
    default:
      return CONSOLE_MESSAGE_KEYS.llm_step_unknown;
  }
}

/** The catalog key for what happened at a step. */
export function outcomeLabelKey(outcome: string): string {
  switch (outcome) {
    case "created":
      return CONSOLE_MESSAGE_KEYS.llm_outcome_created;
    case "reused":
      return CONSOLE_MESSAGE_KEYS.llm_outcome_reused;
    default:
      return CONSOLE_MESSAGE_KEYS.llm_outcome_skipped;
  }
}

export function ConnectVllmPanel({ defaultBaseUrl, fetchImpl, onConnected }: ConnectVllmPanelProps) {
  const [baseUrl, setBaseUrl] = useState(defaultBaseUrl);
  const [offered, setOffered] = useState<readonly string[]>([]);
  const [chosen, setChosen] = useState<readonly string[]>([]);
  const [trace, setTrace] = useState<readonly LlmConnectStepView[]>([]);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const busy = phase.kind === "discovering" || phase.kind === "connecting";

  async function discover(): Promise<void> {
    setPhase({ kind: "discovering" });
    setTrace([]);
    const result = await postJson<DiscoveryResponse>(
      LLM_ENDPOINTS.connectVllm,
      { action: "discover", base_url: baseUrl },
      fetchImpl,
    );
    if (!result.ok) {
      setOffered([]);
      setChosen([]);
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    // The canonical base comes BACK from the server. What was probed is what
    // gets stored, or "discovery worked" and "the provider works" are two
    // different addresses.
    setBaseUrl(result.data.base_url);
    setOffered(result.data.models);
    setChosen(result.data.models.slice(0, 1));
    setPhase({ kind: "idle" });
  }

  async function connect(): Promise<void> {
    setPhase({ kind: "connecting" });
    const result = await postJson<ConnectResponse>(
      LLM_ENDPOINTS.connectVllm,
      { action: "connect", base_url: baseUrl, model_keys: chosen },
      fetchImpl,
    );
    if (!result.ok) {
      const partial = result.failure.detail?.["trace"];
      setTrace(Array.isArray(partial) ? (partial as LlmConnectStepView[]) : []);
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setTrace(result.data.trace);
    setPhase({ kind: "connected" });
    onConnected?.();
  }

  function toggle(modelKey: string): void {
    setChosen((current) =>
      current.includes(modelKey)
        ? current.filter((entry) => entry !== modelKey)
        : [...current, modelKey],
    );
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.llm_connect_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.llm_connect_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.llm_connect_intro)}</p>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.llm_connect_endpoint_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_hint)}
        required
        inputProps={{
          value: baseUrl,
          disabled: busy,
          onChange: (event) => setBaseUrl(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="secondary"
        loading={phase.kind === "discovering"}
        onClick={() => {
          void discover();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.llm_connect_discover_submit)}
      </Button>

      {offered.length > 0 && (
        <fieldset className={styles.models}>
          <legend className={styles.legend}>
            {t(CONSOLE_MESSAGE_KEYS.llm_connect_discovered_heading)}
          </legend>
          {offered.map((modelKey) => (
            <label key={modelKey} className={styles.choice}>
              <input
                type="checkbox"
                name="llm-discovered-model"
                value={modelKey}
                checked={chosen.includes(modelKey)}
                disabled={busy}
                onChange={() => toggle(modelKey)}
              />
              <span className={styles.modelKey}>{modelKey}</span>
            </label>
          ))}
          <Button
            type="button"
            variant="primary"
            loading={phase.kind === "connecting"}
            disabled={chosen.length === 0}
            onClick={() => {
              void connect();
            }}
          >
            {t(CONSOLE_MESSAGE_KEYS.llm_connect_submit)}
          </Button>
        </fieldset>
      )}

      <p className={styles.activity} role="status" aria-live="polite">
        {busy && t(CONSOLE_MESSAGE_KEYS.llm_connect_pending)}
        {phase.kind === "connected" && t(CONSOLE_MESSAGE_KEYS.llm_connect_done)}
      </p>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}

      {trace.length > 0 && (
        <div className={styles.trace}>
          <h3 className={styles.traceHeading}>{t(CONSOLE_MESSAGE_KEYS.llm_trace_heading)}</h3>
          <ul className={styles.traceList}>
            {trace.map((entry, index) => (
              <li key={`${entry.step}-${entry.detail ?? index}`} className={styles.traceItem}>
                <span className={styles.traceStep}>{t(stepLabelKey(entry.step))}</span>
                <span className={styles.traceOutcome}>{t(outcomeLabelKey(entry.outcome))}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
