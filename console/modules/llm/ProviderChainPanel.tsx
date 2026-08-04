"use client";

// The three steps that come after a provider row exists, for one provider.
//
// ============================================================================
// THE PANEL IS ORDERED THE WAY THE CHAIN IS, AND SAYS WHAT IS MISSING
// ============================================================================
//
// model -> credential row -> routing. A half-finished chain is the normal state
// of this screen for as long as it takes an operator to click three times, and
// the two ways it fails in production name none of their causes:
//
//   no credential row  -> `404 credential_not_found`, which reads as "your key
//                         is wrong" when the truth is "there is no key at all".
//   no routing policy  -> the provider is simply never selected, with no error
//                         at all until a completion picks something else.
//
// So the readiness list is rendered ALWAYS, not only on failure, and it is
// derived from the same server-side view the table renders rather than from what
// this component believes it just did.
//
// ============================================================================
// THE KEY FIELD IS WRITE-ONLY AND USUALLY BLANK
// ============================================================================
//
// Nothing populates it, nothing reads it back, and the server never returns it.
// Blank is the normal case: a local endpoint ignores `Authorization` entirely,
// and the BFF generates the placeholder that the required credential row stores.
// The value is held in one local binding for the duration of one submit and
// cleared on success; it is never passed to another component, which is the
// property `tests/unit/architecture/no-secret-props.test.ts` exists to keep.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  chainReadiness,
  isChainComplete,
  providerEndpoint,
  type LlmProviderView,
} from "@/lib/llm-view";

import { postJson, type LlmFailure } from "./request";
import styles from "./ProviderChainPanel.module.css";

export interface ProviderChainPanelProps {
  readonly provider: LlmProviderView;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onChanged?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly failure: LlmFailure };

/** The keys naming the steps that are not done yet, in chain order. */
export function missingStepKeys(provider: LlmProviderView): readonly string[] {
  const readiness = chainReadiness(provider);
  const missing: string[] = [];
  if (!readiness.hasModel) missing.push(CONSOLE_MESSAGE_KEYS.llm_step_model_missing);
  if (!readiness.hasKeyRow) missing.push(CONSOLE_MESSAGE_KEYS.llm_key_row_missing);
  if (!readiness.isEnabled) missing.push(CONSOLE_MESSAGE_KEYS.llm_step_enable_missing);
  if (!readiness.hasPolicy) missing.push(CONSOLE_MESSAGE_KEYS.llm_policy_missing);
  return missing;
}

export function ProviderChainPanel({ provider, fetchImpl, onChanged }: ProviderChainPanelProps) {
  const [modelKey, setModelKey] = useState("");
  const [keyMaterial, setKeyMaterial] = useState("");
  const [routedModelId, setRoutedModelId] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";
  const missing = missingStepKeys(provider);
  const complete = isChainComplete(chainReadiness(provider));

  // The return type is deliberately inferred rather than written. `Promise<void>`
  // after a parameter list containing `() => void` reads to the hardcoded-copy
  // scanner as a JSX text node ("void): Promise"), and a guard that has to be
  // argued with is a guard that gets weakened.
  async function run(url: string, body: unknown, onDone: () => void) {
    setPhase({ kind: "pending" });
    const result = await postJson<{ id: string }>(url, body, fetchImpl);
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    onDone();
    setPhase({ kind: "idle" });
    onChanged?.();
  }

  return (
    <div className={styles.panel}>
      <h4 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.llm_chain_heading)}</h4>

      <p className={complete ? styles.ready : styles.notReady} role="status">
        {complete
          ? t(CONSOLE_MESSAGE_KEYS.llm_chain_complete)
          : t(CONSOLE_MESSAGE_KEYS.llm_chain_incomplete)}
      </p>

      {missing.length > 0 && (
        <ul className={styles.missing}>
          {missing.map((key) => (
            <li key={key}>{t(key)}</li>
          ))}
        </ul>
      )}

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.llm_add_model_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.llm_add_model_hint)}
        inputProps={{
          value: modelKey,
          disabled: pending,
          onChange: (event) => setModelKey(event.target.value),
        }}
      />
      <Button
        type="button"
        variant="secondary"
        loading={pending}
        disabled={modelKey.trim() === ""}
        onClick={() => {
          void run(
            providerEndpoint(provider.id, "/models"),
            { model_key: modelKey.trim() },
            () => setModelKey(""),
          );
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.llm_add_model_submit)}
      </Button>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.llm_key_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.llm_add_key_row_hint)}
        inputProps={{
          value: keyMaterial,
          type: "password",
          autoComplete: "off",
          disabled: pending,
          onChange: (event) => setKeyMaterial(event.target.value),
        }}
      />
      <Button
        type="button"
        variant="secondary"
        loading={pending}
        onClick={() => {
          void run(providerEndpoint(provider.id, "/credentials"), { api_key: keyMaterial }, () =>
            setKeyMaterial(""),
          );
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.llm_add_key_row_submit)}
      </Button>

      <label className={styles.selectRow}>
        <span className={styles.selectLabel}>
          {t(CONSOLE_MESSAGE_KEYS.llm_bind_routing_model_label)}
        </span>
        <select
          className={styles.select}
          value={routedModelId}
          disabled={pending || provider.models.length === 0}
          onChange={(event) => setRoutedModelId(event.target.value)}
        >
          <option value="">{t(CONSOLE_MESSAGE_KEYS.llm_bind_routing_no_model)}</option>
          {provider.models.map((model) => (
            <option key={model.id} value={model.id}>
              {model.modelKey}
            </option>
          ))}
        </select>
      </label>
      <Button
        type="button"
        variant="secondary"
        loading={pending}
        disabled={routedModelId === ""}
        onClick={() => {
          void run(
            providerEndpoint(provider.id, "/routing"),
            { provider_model_id: routedModelId },
            () => setRoutedModelId(""),
          );
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.llm_bind_routing_submit)}
      </Button>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </div>
  );
}
