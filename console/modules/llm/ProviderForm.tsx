"use client";

// Register one OpenAI-compatible provider by hand.
//
// The long way round the shortcut, kept because the shortcut needs the endpoint
// to be reachable RIGHT NOW and an operator configuring a deployment they cannot
// currently reach still has work to do. It creates the provider row and nothing
// else — the remaining three steps are the chain panel's, and the readiness
// summary beside each provider says which of them are still missing.
//
// `provider_type` is not a field. See the route handler's header: seven of the
// eight enum values are a vendor with its own credential ceremony, and a form
// that offered them without those ceremonies would create rows nobody can
// finish.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { LLM_ENDPOINTS } from "@/lib/llm-view";

import { postJson, type LlmFailure } from "./request";
import styles from "./ProviderForm.module.css";

export interface ProviderFormProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onCreated?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "created" }
  | { readonly kind: "failed"; readonly failure: LlmFailure };

export function ProviderForm({ fetchImpl, onCreated }: ProviderFormProps) {
  const [displayName, setDisplayName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    setPhase({ kind: "pending" });
    const result = await postJson<{ id: string }>(
      LLM_ENDPOINTS.providers,
      { display_name: displayName.trim(), base_url: baseUrl.trim() },
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setDisplayName("");
    setBaseUrl("");
    setPhase({ kind: "created" });
    onCreated?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.llm_add_provider_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.llm_add_provider_heading)}</h2>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.llm_provider_name_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.llm_provider_name_hint)}
        required
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.llm_provider_base_url_hint)}
        required
        inputProps={{
          value: baseUrl,
          disabled: pending,
          onChange: (event) => setBaseUrl(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={displayName.trim() === "" || baseUrl.trim() === ""}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.llm_add_provider_submit)}
      </Button>

      <p className={styles.activity} role="status" aria-live="polite">
        {phase.kind === "created" && t(CONSOLE_MESSAGE_KEYS.llm_provider_created)}
      </p>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </section>
  );
}
