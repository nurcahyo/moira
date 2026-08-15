"use client";

// Author one flow, with its ordered steps.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, postJson } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AgentFlowRecord, AgentFlowStepCreateRequest, AgentProfileRecord } from "@/lib/types";

import { FlowStepsBuilder } from "./FlowStepsBuilder";
import styles from "./FlowCreateForm.module.css";

export interface FlowCreateFormProps {
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onCreated?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "created" }
  | { readonly kind: "failed"; readonly failure: ConsoleApiFailure };

export function FlowCreateForm({ agentProfiles, fetchImpl, onCreated }: FlowCreateFormProps) {
  const [flowKey, setFlowKey] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [description, setDescription] = useState("");
  const [steps, setSteps] = useState<readonly AgentFlowStepCreateRequest[]>([]);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    setPhase({ kind: "pending" });
    const result = await postJson<AgentFlowRecord>(
      "/api/flows",
      {
        flow_key: flowKey.trim(),
        display_name: displayName.trim(),
        description: description.trim() === "" ? null : description.trim(),
        steps,
      },
      CONSOLE_MESSAGE_KEYS.flows_request_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setFlowKey("");
    setDisplayName("");
    setDescription("");
    setSteps([]);
    setPhase({ kind: "created" });
    onCreated?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.flows_create_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.flows_create_heading)}</h2>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.flows_field_flow_key_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.flows_field_flow_key_hint)}
        required
        inputProps={{
          value: flowKey,
          disabled: pending,
          onChange: (event) => setFlowKey(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.flows_field_display_name_label)}
        required
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.flows_field_description_label)}
        inputProps={{
          value: description,
          disabled: pending,
          onChange: (event) => setDescription(event.target.value),
        }}
      />

      <FlowStepsBuilder
        steps={steps}
        onStepsChange={setSteps}
        agentProfiles={agentProfiles}
        disabled={pending}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={flowKey.trim() === "" || displayName.trim() === ""}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.flows_create_submit)}
      </Button>

      {phase.kind === "created" && (
        <p className={styles.status} role="status" aria-live="polite">
          {t(CONSOLE_MESSAGE_KEYS.flows_create_success)}
        </p>
      )}
      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </section>
  );
}
