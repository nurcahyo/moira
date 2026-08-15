"use client";

// The "Connect a Claude credential" panel — three acquisition modes on one
// screen:
//
//   Mode A (CLI-assisted, primary)  server-side, no field: mint a token by
//                                    running the local `claude` CLI.
//   Mode B (API key)                paste an official `sk-ant-…` Anthropic
//                                    Console API key.
//   Mode C (paste, fallback)        paste the output of `claude setup-token`
//                                    by hand — the ORIGINAL shape this panel
//                                    shipped with.
//
// ============================================================================
// WHY THIS IS A SEPARATE PANEL, NOT A FIELD ON `ProviderForm`
// ============================================================================
//
// `ProviderForm` creates an `open_ai_compatible` provider by hand and
// deliberately offers no `provider_type` field — see its own header. This
// panel writes to TWO different dedicated `anthropic` provider rows it
// finds-or-creates itself (`lib/claude-subscription.ts`): one for the
// subscription token (Modes A/C), one for the API key (Mode B). Folding
// either into `ProviderForm` would make one form respond to resources it does
// not manage.
//
// ============================================================================
// NO SECRET EVER LEAVES THIS COMPONENT EXCEPT AS A REQUEST BODY
// ============================================================================
//
// Mode B's field is typed into a `type="password"` input, held in local state
// for exactly as long as its request is in flight, and cleared on success.
// Mode A has no field at all — the token is minted server-side and never
// reaches the browser in any form. Mode C is unchanged from how it always
// worked. None of the three responses this panel reads back carries a secret
// — see each route handler's own header for the projection.
//
// ============================================================================
// STATUS AND "RE-ACQUIRE" REUSE THE SAME REQUEST THE FIRST CONNECT USED
// ============================================================================
//
// `subscriptionStatus`/`keyStatus` are read on the SERVER, at page load
// (`lib/claude-subscription.ts`'s `loadClaudeSubscriptionStatus`/
// `loadClaudeApiKeyStatus`, called from `page.tsx`) — never fetched by this
// component. "Re-acquire" (Mode A) and a second "Save" (Modes B/C) are not
// separate code paths from the first connect: they POST to the exact same
// endpoint, and the chain underneath (`connectClaudeSubscription`/
// `connectClaudeApiKey`) rotates the existing row in place rather than
// creating a second one. This component only changes the BUTTON LABEL
// between "connect" and "re-acquire/rotate" phrasing, and only the label.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { LLM_ENDPOINTS, type ClaudeCredentialStatusView } from "@/lib/llm-view";

import { postJson, type LlmFailure } from "./request";
import styles from "./ConnectClaudeSubscriptionPanel.module.css";

/** The neutral default this panel renders with until the page passes a real read. */
const UNKNOWN_STATUS: ClaudeCredentialStatusView = { kind: "unknown", status: null, expiresAt: null };

export interface ConnectClaudeSubscriptionPanelProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites re-read the server-rendered data. */
  readonly onConnected?: () => void;
  /**
   * Server-resolved: is Mode A turned on for this deployment?
   * (`CONSOLE_ALLOW_LOCAL_CLI_CREDENTIALS`, see `lib/env.ts`.) Defaults to
   * `false` — the same fail-closed default the environment variable itself
   * has — so a caller that omits this prop gets the disabled notice, never a
   * button that would 403.
   */
  readonly cliAcquisitionEnabled?: boolean;
  /** Status of the Mode A/C (oauth2, subscription) row. */
  readonly subscriptionStatus?: ClaudeCredentialStatusView;
  /** Status of the Mode B (api_key) row. */
  readonly keyStatus?: ClaudeCredentialStatusView;
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

function apiKeySavedMessageKey(outcome: ConnectOutcome): string {
  return outcome === "created"
    ? CONSOLE_MESSAGE_KEYS.claude_api_key_created
    : CONSOLE_MESSAGE_KEYS.claude_api_key_updated;
}

/**
 * Presentational only: renders one of "unknown" / "not connected" /
 * "connected, active or disabled, with or without a known expiry". Never
 * renders `masked_secret` or a fingerprint — `ClaudeCredentialStatusView`
 * does not carry either; see its own header in `lib/llm-view.ts`.
 */
function StatusLine({ status }: { readonly status: ClaudeCredentialStatusView }) {
  if (status.kind === "unknown") {
    return <p className={styles.status}>{t(CONSOLE_MESSAGE_KEYS.claude_connect_status_unknown)}</p>;
  }
  if (status.kind === "not_connected") {
    return (
      <p className={styles.status}>
        <Badge tone="neutral">{t(CONSOLE_MESSAGE_KEYS.claude_connect_status_not_connected)}</Badge>
      </p>
    );
  }
  const active = status.status === "active";
  return (
    <p className={styles.status}>
      <Badge tone={active ? "success" : "warning"}>
        {active
          ? t(CONSOLE_MESSAGE_KEYS.claude_connect_status_connected)
          : t(CONSOLE_MESSAGE_KEYS.claude_connect_status_disabled)}
      </Badge>{" "}
      {status.expiresAt === null
        ? t(CONSOLE_MESSAGE_KEYS.claude_connect_status_no_expiry)
        : t(CONSOLE_MESSAGE_KEYS.claude_connect_status_expires, { date: status.expiresAt })}
    </p>
  );
}

export function ConnectClaudeSubscriptionPanel({
  fetchImpl,
  onConnected,
  cliAcquisitionEnabled = false,
  subscriptionStatus = UNKNOWN_STATUS,
  keyStatus = UNKNOWN_STATUS,
}: ConnectClaudeSubscriptionPanelProps) {
  /* --- Mode A: CLI-assisted ------------------------------------------------ */
  const [cliPhase, setCliPhase] = useState<Phase>({ kind: "idle" });
  const cliPending = cliPhase.kind === "pending";
  const cliAlreadyConnected = subscriptionStatus.kind === "connected";

  async function submitCli(): Promise<void> {
    setCliPhase({ kind: "pending" });
    const result = await postJson<ConnectResponse>(
      LLM_ENDPOINTS.claudeSubscriptionAcquire,
      {},
      fetchImpl,
    );
    if (!result.ok) {
      setCliPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setCliPhase({ kind: "saved", outcome: result.data.outcome });
    onConnected?.();
  }

  /* --- Mode B: an official Anthropic API key ------------------------------- */
  const [apiKeyValue, setApiKeyValue] = useState("");
  const [apiKeyPhase, setApiKeyPhase] = useState<Phase>({ kind: "idle" });
  const apiKeyPending = apiKeyPhase.kind === "pending";

  async function submitApiKey(): Promise<void> {
    setApiKeyPhase({ kind: "pending" });
    const result = await postJson<ConnectResponse>(
      LLM_ENDPOINTS.claudeApiKey,
      { api_key: apiKeyValue },
      fetchImpl,
    );
    if (!result.ok) {
      setApiKeyPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setApiKeyValue("");
    setApiKeyPhase({ kind: "saved", outcome: result.data.outcome });
    onConnected?.();
  }

  /* --- Mode C: paste (fallback, unchanged) ---------------------------------- */
  // Named `value`, not `token`: it holds the same kind of raw secret
  // `ReplaceSecretForm.tsx` does, and that file's local binding is named the
  // same way for the same reason — a plain, generic identifier for the one
  // value in this section nothing here may log or forward except as the
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

      {/* --- Mode A -------------------------------------------------------- */}
      <div className={styles.subsection}>
        <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_heading)}</h3>
        <StatusLine status={subscriptionStatus} />
        {cliAcquisitionEnabled ? (
          <>
            <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_intro)}</p>
            <Button
              type="button"
              variant="primary"
              loading={cliPending}
              onClick={() => {
                void submitCli();
              }}
            >
              {t(
                cliAlreadyConnected
                  ? CONSOLE_MESSAGE_KEYS.claude_subscription_cli_reacquire
                  : CONSOLE_MESSAGE_KEYS.claude_subscription_cli_submit,
              )}
            </Button>
            <p className={styles.activity} role="status" aria-live="polite">
              {cliPending && t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_pending)}
              {cliPhase.kind === "saved" && t(savedMessageKey(cliPhase.outcome))}
            </p>
            {cliPhase.kind === "failed" && (
              <p className={styles.problem} role="alert">
                {t(cliPhase.failure.messageKey, cliPhase.failure.messageArgs, cliPhase.failure.message)}
              </p>
            )}
          </>
        ) : (
          <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled_notice)}</p>
        )}
      </div>

      {/* --- Mode B -------------------------------------------------------- */}
      <div className={styles.subsection}>
        <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.claude_api_key_heading)}</h3>
        <StatusLine status={keyStatus} />
        <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.claude_api_key_intro)}</p>

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.claude_api_key_label)}
          hint={t(CONSOLE_MESSAGE_KEYS.claude_api_key_hint)}
          required
          inputProps={{
            value: apiKeyValue,
            type: "password",
            autoComplete: "off",
            disabled: apiKeyPending,
            onChange: (event) => setApiKeyValue(event.target.value),
          }}
        />

        <Button
          type="button"
          variant="primary"
          loading={apiKeyPending}
          disabled={apiKeyValue.trim() === ""}
          onClick={() => {
            void submitApiKey();
          }}
        >
          {t(CONSOLE_MESSAGE_KEYS.claude_api_key_submit)}
        </Button>

        <p className={styles.activity} role="status" aria-live="polite">
          {apiKeyPending && t(CONSOLE_MESSAGE_KEYS.claude_api_key_pending)}
          {apiKeyPhase.kind === "saved" && t(apiKeySavedMessageKey(apiKeyPhase.outcome))}
        </p>

        {apiKeyPhase.kind === "failed" && (
          <p className={styles.problem} role="alert">
            {t(apiKeyPhase.failure.messageKey, apiKeyPhase.failure.messageArgs, apiKeyPhase.failure.message)}
          </p>
        )}
      </div>

      {/* --- Mode C ---------------------------------------------------------- */}
      <div className={styles.subsection}>
        <h3 className={styles.subheading}>
          {t(CONSOLE_MESSAGE_KEYS.claude_subscription_paste_heading)}
        </h3>

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
      </div>
    </section>
  );
}
