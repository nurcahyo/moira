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

import { useEffect, useRef, useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  claudeSubscriptionAcquireStatusUrl,
  LLM_ENDPOINTS,
  type ClaudeCredentialStatusView,
} from "@/lib/llm-view";

import { postJson, sendGet, type LlmFailure } from "./request";
import styles from "./ConnectClaudeSubscriptionPanel.module.css";

/**
 * How often the panel polls `acquire/status` while a Mode A job is running.
 * Well under the job's own multi-minute budget (`lib/claude-cli.ts`) — this
 * only controls UI responsiveness, not the server-side deadline.
 */
const CLI_POLL_INTERVAL_MS = 2_000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

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
  /**
   * Test seam only: how often Mode A polls `acquire/status`. Shipped call
   * sites never pass this — they get `CLI_POLL_INTERVAL_MS`.
   */
  readonly cliPollIntervalMs?: number;
}

type ConnectOutcome = "created" | "rotated";

interface ConnectResponse {
  readonly provider_id: string;
  readonly credential_id: string;
  readonly outcome: ConnectOutcome;
}

interface AcquireStartResponse {
  readonly job_id: string;
  readonly authorization_url: string | null;
}

type AcquireStatusResponse =
  | { readonly status: "running"; readonly authorization_url: string | null }
  | ({ readonly status: "succeeded" } & ConnectResponse);

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "saved"; readonly outcome: ConnectOutcome }
  | { readonly kind: "failed"; readonly failure: LlmFailure };

/**
 * Mode A's own phase union — a superset of `Phase` because, unlike Modes B/C,
 * a job that started successfully is not yet done: the panel must show it is
 * WAITING on a human to finish signing in, and where to do that (issue #269).
 */
type CliPhase =
  | { readonly kind: "idle" }
  | { readonly kind: "starting" }
  | { readonly kind: "awaiting_login"; readonly jobId: string; readonly authorizationUrl: string | null }
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
  cliPollIntervalMs = CLI_POLL_INTERVAL_MS,
}: ConnectClaudeSubscriptionPanelProps) {
  /* --- Mode A: CLI-assisted, two-phase (issue #269) ------------------------ */
  const [cliPhase, setCliPhase] = useState<CliPhase>({ kind: "idle" });
  const cliPending = cliPhase.kind === "starting" || cliPhase.kind === "awaiting_login";
  const cliAlreadyConnected = subscriptionStatus.kind === "connected";
  // Guards against `setState` after unmount (the poll loop is a `while`, not
  // an effect) and against opening a second tab on a later poll that still
  // reports the same URL.
  const cliMountedRef = useRef(true);
  const cliOpenedUrlRef = useRef<string | null>(null);
  useEffect(
    () => () => {
      cliMountedRef.current = false;
    },
    [],
  );

  async function pollCli(jobId: string): Promise<void> {
    for (;;) {
      await sleep(cliPollIntervalMs);
      if (!cliMountedRef.current) return;

      const polled = await sendGet<AcquireStatusResponse>(
        claudeSubscriptionAcquireStatusUrl(jobId),
        fetchImpl,
      );
      if (!cliMountedRef.current) return;

      if (!polled.ok) {
        setCliPhase({ kind: "failed", failure: polled.failure });
        return;
      }
      if (polled.data.status === "succeeded") {
        setCliPhase({ kind: "saved", outcome: polled.data.outcome });
        onConnected?.();
        return;
      }

      const authorizationUrl = polled.data.authorization_url;
      setCliPhase({ kind: "awaiting_login", jobId, authorizationUrl });
      // Best-effort: open the sign-in tab the first time a URL is known. The
      // link rendered below is the affordance of record — a popup blocker
      // silently swallowing this is not a failure, just a no-op.
      if (authorizationUrl !== null && cliOpenedUrlRef.current !== authorizationUrl) {
        cliOpenedUrlRef.current = authorizationUrl;
        try {
          window.open(authorizationUrl, "_blank", "noopener,noreferrer");
        } catch {
          // Ignored — the rendered link still works.
        }
      }
    }
  }

  async function submitCli(): Promise<void> {
    setCliPhase({ kind: "starting" });
    cliOpenedUrlRef.current = null;
    const started = await postJson<AcquireStartResponse>(
      LLM_ENDPOINTS.claudeSubscriptionAcquireStart,
      {},
      fetchImpl,
    );
    if (!cliMountedRef.current) return;
    if (!started.ok) {
      setCliPhase({ kind: "failed", failure: started.failure });
      return;
    }
    setCliPhase({
      kind: "awaiting_login",
      jobId: started.data.job_id,
      authorizationUrl: started.data.authorization_url,
    });
    await pollCli(started.data.job_id);
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
              {cliPhase.kind === "starting" && t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_pending)}
              {cliPhase.kind === "awaiting_login" && (
                <>
                  {t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_awaiting_login)}
                  {cliPhase.authorizationUrl !== null && (
                    <>
                      {" "}
                      <a href={cliPhase.authorizationUrl} target="_blank" rel="noopener noreferrer">
                        {t(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_open_link)}
                      </a>
                    </>
                  )}
                </>
              )}
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
