"use client";

// Provision a containerised Claude runner.
//
// ============================================================================
// SCOPE IS COLLECTED HERE BECAUSE IT CAN ONLY EVER BE COLLECTED HERE
// ============================================================================
//
// The scope this form submits is sealed into the resulting credential's AAD at
// provisioning time — `POST .../finalize` has no scope field and refuses one.
// So this is the console's one chance to ask "whose Claude account is this
// for", and the choice is rendered as an explicit either/or rather than a free
// text field: PLATFORM ACCOUNT (the default — omit `scope` entirely) or A
// TENANT'S OWN SUBSCRIPTION (`{"type":"tenant", external_tenant_id}`, the
// existing `CredentialScope` wire shape, verbatim).
//
// ============================================================================
// SUCCESS NAVIGATES, IT DOES NOT JUST REFRESH A LIST
// ============================================================================
//
// Provisioning is the start of a multi-step, human-in-the-loop flow — the
// container has to print an authorization URL, the operator has to open it in
// their OWN browser, and the code has to come back. There is nothing useful to
// show on the list page immediately afterwards, so a successful submit takes
// the operator straight to the new runner's detail page, which is the one that
// renders the next step.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Input } from "@/components/atoms/Input";
import { Label } from "@/components/atoms/Label";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  CLAUDE_RUNNER_LABEL_PATTERN,
  CLAUDE_RUNNER_TTL_SECONDS_DEFAULT,
  CLAUDE_RUNNER_TTL_SECONDS_MAX,
  CLAUDE_RUNNER_TTL_SECONDS_MIN,
  type ClaudeRunnerRecord,
} from "@/lib/types";

import { postRunnerJson, type RunnerFailure } from "./request";
import styles from "./RunnerProvisionForm.module.css";

export interface RunnerProvisionFormProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Called with the new runner's id once provisioning succeeds. */
  readonly onProvisioned?: (id: string) => void;
}

type ScopeChoice = "platform" | "tenant";
type Phase = { readonly kind: "idle" } | { readonly kind: "pending" } | { readonly kind: "failed"; readonly failure: RunnerFailure };

export function RunnerProvisionForm({ fetchImpl, onProvisioned }: RunnerProvisionFormProps) {
  const [label, setLabel] = useState("");
  const [ttlSeconds, setTtlSeconds] = useState("");
  const [scopeChoice, setScopeChoice] = useState<ScopeChoice>("platform");
  const [tenantId, setTenantId] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";
  const labelValid = CLAUDE_RUNNER_LABEL_PATTERN.test(label);
  const tenantIdValid = scopeChoice === "platform" || tenantId.trim() !== "";
  const canSubmit = labelValid && tenantIdValid && !pending;

  async function submit(): Promise<void> {
    if (!canSubmit) return;
    setPhase({ kind: "pending" });

    const trimmedTtl = ttlSeconds.trim();
    const body: Record<string, unknown> = { label };
    if (trimmedTtl !== "") body["ttl_seconds"] = Number(trimmedTtl);
    if (scopeChoice === "tenant") {
      body["scope"] = { type: "tenant", external_tenant_id: tenantId.trim() };
    }

    const result = await postRunnerJson<ClaudeRunnerRecord>("/api/runners", body, fetchImpl);
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setPhase({ kind: "idle" });
    setLabel("");
    setTtlSeconds("");
    setScopeChoice("platform");
    setTenantId("");
    onProvisioned?.(result.data.id);
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.runners_provision_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.runners_provision_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.runners_provision_intro)}</p>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.runners_provision_label_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.runners_provision_label_hint)}
        required
        inputProps={{
          value: label,
          disabled: pending,
          onChange: (event) => setLabel(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.runners_provision_ttl_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.runners_provision_ttl_hint)}
        inputProps={{
          type: "number",
          min: CLAUDE_RUNNER_TTL_SECONDS_MIN,
          max: CLAUDE_RUNNER_TTL_SECONDS_MAX,
          placeholder: String(CLAUDE_RUNNER_TTL_SECONDS_DEFAULT),
          value: ttlSeconds,
          disabled: pending,
          onChange: (event) => setTtlSeconds(event.target.value),
        }}
      />

      <fieldset className={styles.scope} disabled={pending}>
        <legend>{t(CONSOLE_MESSAGE_KEYS.runners_provision_scope_legend)}</legend>

        <div className={styles.scopeOption}>
          <Input
            type="radio"
            id="runner-scope-platform"
            name="runner-scope"
            checked={scopeChoice === "platform"}
            onChange={() => setScopeChoice("platform")}
          />
          <Label htmlFor="runner-scope-platform">
            {t(CONSOLE_MESSAGE_KEYS.runners_provision_scope_platform_option)}
          </Label>
        </div>

        <div className={styles.scopeOption}>
          <Input
            type="radio"
            id="runner-scope-tenant"
            name="runner-scope"
            checked={scopeChoice === "tenant"}
            onChange={() => setScopeChoice("tenant")}
          />
          <Label htmlFor="runner-scope-tenant">
            {t(CONSOLE_MESSAGE_KEYS.runners_provision_scope_tenant_option)}
          </Label>
        </div>

        {scopeChoice === "tenant" && (
          <FormField
            label={t(CONSOLE_MESSAGE_KEYS.runners_provision_tenant_id_label)}
            hint={t(CONSOLE_MESSAGE_KEYS.runners_provision_tenant_id_hint)}
            required
            inputProps={{
              value: tenantId,
              disabled: pending,
              onChange: (event) => setTenantId(event.target.value),
            }}
          />
        )}
      </fieldset>

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={!canSubmit}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.runners_provision_submit)}
      </Button>

      <p className={styles.activity} role="status" aria-live="polite">
        {pending && t(CONSOLE_MESSAGE_KEYS.runners_provision_pending)}
      </p>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </section>
  );
}
