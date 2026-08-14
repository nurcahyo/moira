"use client";

// The "Connect Claude subscription" panel — paste the output of
// `claude setup-token` and store it as an `oauth2` provider credential.
//
// ============================================================================
// WHY THIS IS A SEPARATE PANEL, NOT A FIELD ON `ProviderForm`
// ============================================================================
//
// `ProviderForm` creates an `open_ai_compatible` provider by hand and
// deliberately offers no `provider_type` field — see its own header. This
// panel writes to a DIFFERENT provider row entirely (a dedicated, console-
// managed `anthropic` row it finds-or-creates itself; see
// `lib/claude-subscription.ts`), and it writes a credential, not a provider.
// Folding the two together would make one form respond to two unrelated
// resources, which is the same shape `ConnectVllmPanel`'s two-stage design
// avoids for a different reason.
//
// ============================================================================
// THE VALUE NEVER LEAVES THIS COMPONENT EXCEPT AS THE REQUEST BODY
// ============================================================================
//
// It is typed into a `type="password"` field, held in local state for exactly
// as long as the request is in flight, and cleared on success. It is never
// logged, never put in an error, and the response this panel reads back
// (`{ provider_id, credential_id, outcome }`) does not carry it — the server
// route projects the response by hand, exactly as
// `app/api/llm/providers/[id]/credentials/route.ts` does for an API key.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { LLM_ENDPOINTS } from "@/lib/llm-view";

import { postJson, type LlmFailure } from "./request";
import styles from "./ConnectClaudeSubscriptionPanel.module.css";

export interface ConnectClaudeSubscriptionPanelProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites re-read the server-rendered data. */
  readonly onConnected?: () => void;
}

type ConnectOutcome = "created" | "rotated";

interface ConnectResponse {
  readonly provider_id: string;
  readonly credential_id: string;
  readonly outcome: ConnectOutcome;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "saved"; readonly outcome: ConnectOutcome }
  | { readonly kind: "failed"; readonly failure: LlmFailure };

function savedMessageKey(outcome: ConnectOutcome): string {
  return outcome === "created"
    ? CONSOLE_MESSAGE_KEYS.claude_subscription_created
    : CONSOLE_MESSAGE_KEYS.claude_subscription_rotated;
}

export function ConnectClaudeSubscriptionPanel({
  fetchImpl,
  onConnected,
}: ConnectClaudeSubscriptionPanelProps) {
  // Named `value`, not `token`: it holds the same kind of raw secret
  // `ReplaceSecretForm.tsx` does, and that file's local binding is named the
  // same way for the same reason — a plain, generic identifier for the one
  // value in this component nothing here may log or forward except as the
  // request body.
  const [value, setValue] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    setPhase({ kind: "pending" });
    const result = await postJson<ConnectResponse>(
      LLM_ENDPOINTS.claudeSubscription,
      { token: value },
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setValue("");
    setPhase({ kind: "saved", outcome: result.data.outcome });
    onConnected?.();
  }

  return (
    <section
      className={styles.panel}
      aria-label={t(CONSOLE_MESSAGE_KEYS.claude_subscription_heading)}
    >
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.claude_subscription_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.claude_subscription_intro)}</p>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.claude_subscription_token_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.claude_subscription_token_hint)}
        required
        inputProps={{
          value,
          type: "password",
          autoComplete: "off",
          disabled: pending,
          onChange: (event) => setValue(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={value.trim() === ""}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.claude_subscription_submit)}
      </Button>

      <p className={styles.activity} role="status" aria-live="polite">
        {pending && t(CONSOLE_MESSAGE_KEYS.claude_subscription_pending)}
        {phase.kind === "saved" && t(savedMessageKey(phase.outcome))}
      </p>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </section>
  );
}
