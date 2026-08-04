"use client";

// The wizard's combined sign-in and claim surface.
//
// Rendered only once the provisioning gate is complete (`isProvisioningComplete`
// — the parent mounts it for the `sign_in` and `claim` steps and nothing
// earlier), so its sign-in buttons can never drive a flow the server has not
// fully provisioned, and its claim control can never fire the request Moira is
// guaranteed to 403.
//
// Sign-in drives the SAME Better Auth flow as `/login` — a POST to the mounted
// `/api/auth/sign-in/oauth2` handler with a `providerId` the SERVER derived
// (the provision response's `provider_id`; the derivation is server-only in
// `lib/auth-config.ts` and is not re-implemented here). The callback returns to
// `/setup`, where the wizard's session probe picks the identity up.
//
// The claim goes through `POST /api/setup {action: "claim"}` and carries NO
// provisioning state: the BFF derives the state from Moira's records plus the
// console's secret store on every claim, because this component's memory does
// not survive the sign-in navigation it itself initiates. A `403
// admin_claim_domain_not_allowed` is NOT rendered as a banner here: it is
// reported upward so the wizard returns the operator to the auth-settings step
// with the offending domain named and the allow-list field focused — the one
// place the refusal can still be fixed.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Spinner } from "@/components/atoms/Spinner";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { JsonValue } from "@/lib/types";

import type { SetupMethodSummary } from "./setup-view";
import styles from "./SignInClaimStep.module.css";


/** Where Better Auth's sign-in endpoint is mounted. Mirrors `AUTH_BASE_PATH`. */
const SIGN_IN_ENDPOINT = "/api/auth/sign-in/oauth2";
/** The wizard's own door for the claim action. */
const SETUP_ENDPOINT = "/api/setup";
/** A completed sign-in lands back on the wizard. Relative on purpose. */
const CALLBACK_PATH = "/setup";
/** Where sign-in happens when no in-session provider id is known. */
const LOGIN_PATH = "/login";

export interface SignInClaimStepProps {
  /** Which sub-surface is active, per `reachableSetupStep`. */
  readonly stage: "sign_in" | "claim";
  /**
   * Interactive provider rows, for the sign-in buttons' labels. On a FRESH
   * deployment this is empty — the rows come from the server render of
   * `GET /api/setup`, which ran before anything was provisioned — so an empty
   * list must still render a working control: one generic sign-in button
   * driving the just-provisioned `oauthProviderId`.
   */
  readonly methods: readonly SetupMethodSummary[];
  /** Better Auth's provider id, from the provision response. Null on revisit. */
  readonly oauthProviderId: string | null;
  /** The probed console session's email, when one exists. */
  readonly signedInEmail: string | null;
  readonly onClaimed: (email: string | null) => void;
  readonly onDomainRefused: (domain: string) => void;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites navigate for real. */
  readonly navigate?: (url: string) => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "signing_in" }
  | { readonly kind: "claiming" }
  | {
      readonly kind: "failed";
      readonly messageKey: string;
      readonly message?: string;
      readonly messageArgs?: JsonValue;
    };

export function SignInClaimStep({
  stage,
  methods,
  oauthProviderId,
  signedInEmail,
  onClaimed,
  onDomainRefused,
  fetchImpl,
  navigate,
}: SignInClaimStepProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const send = fetchImpl ?? globalThis.fetch;
  const go = navigate ?? ((url: string) => globalThis.location.assign(url));
  const interactive = methods.filter((method) => method.interactive);

  async function signIn(): Promise<void> {
    if (oauthProviderId === null) {
      // Revisit with no in-session provider id: the sign-in page resolves the
      // provider server-side and offers the working buttons.
      go(LOGIN_PATH);
      return;
    }
    setPhase({ kind: "signing_in" });
    let response: Response;
    try {
      response = await send(SIGN_IN_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ providerId: oauthProviderId, callbackURL: CALLBACK_PATH }),
      });
    } catch {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }
    if (response.status === 429) {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_rate_limited });
      return;
    }
    if (!response.ok) {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }
    let url: unknown;
    try {
      url = ((await response.json()) as { url?: unknown }).url;
    } catch {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }
    if (typeof url !== "string" || url === "") {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_no_redirect_url });
      return;
    }
    go(url);
  }

  async function claim(): Promise<void> {
    setPhase({ kind: "claiming" });
    let response: Response;
    try {
      response = await send(SETUP_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        // No state: the BFF re-derives the provisioning state from Moira and
        // the console's secret store, so nothing here depends on what this
        // document happens to remember.
        body: JSON.stringify({ action: "claim" }),
      });
    } catch {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_request_unreachable });
      return;
    }

    let payload: Record<string, unknown> | null = null;
    try {
      payload = (await response.json()) as Record<string, unknown>;
    } catch {
      payload = null;
    }

    if (response.ok) {
      const identity =
        payload !== null && typeof payload["identity"] === "object" && payload["identity"] !== null
          ? (payload["identity"] as Record<string, unknown>)
          : null;
      const email = identity?.["email"];
      setPhase({ kind: "idle" });
      onClaimed(typeof email === "string" ? email : null);
      return;
    }

    const error =
      payload !== null && typeof payload["error"] === "object" && payload["error"] !== null
        ? (payload["error"] as Record<string, unknown>)
        : null;

    if (error !== null && error["code"] === "admin_claim_domain_not_allowed") {
      // The expected first-run misconfiguration. Reported upward, never a
      // generic banner: the wizard returns to the auth-settings step with the
      // offending domain named and the allow-list focused.
      const args =
        typeof error["message_args"] === "object" && error["message_args"] !== null
          ? (error["message_args"] as Record<string, unknown>)
          : null;
      const domain = args?.["domain"];
      setPhase({ kind: "idle" });
      onDomainRefused(typeof domain === "string" ? domain : "");
      return;
    }

    if (error !== null) {
      const text =
        typeof error["text"] === "object" && error["text"] !== null
          ? (error["text"] as Record<string, unknown>)
          : null;
      const messageKey = error["message_key"] ?? text?.["messageKey"];
      const message = text?.["message"];
      const messageArgs = error["message_args"] ?? text?.["messageArgs"];
      setPhase({
        kind: "failed",
        messageKey: typeof messageKey === "string" ? messageKey : CONSOLE_MESSAGE_KEYS.setup_request_unreachable,
        ...(typeof message === "string" ? { message } : {}),
        ...(messageArgs === undefined ? {} : { messageArgs: messageArgs as JsonValue }),
      });
      return;
    }

    setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_request_unreachable });
  }

  /** One sign-in button label per provider — same fallback rules as /login. */
  function buttonLabel(method: SetupMethodSummary, total: number): string {
    if (method.displayName !== "") {
      return t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: method.displayName });
    }
    if (total === 1) return t(CONSOLE_MESSAGE_KEYS.sign_in_button_generic);
    return t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: method.id });
  }

  const busy = phase.kind === "signing_in" || phase.kind === "claiming";

  return (
    <section className={styles.step} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_sign_in_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.setup_sign_in_heading)}</h2>

      {stage === "sign_in" && (
        <>
          <p className={styles.body}>{t(CONSOLE_MESSAGE_KEYS.setup_sign_in_intro)}</p>
          <div className={styles.actions}>
            {interactive.length === 0 ? (
              // Fresh deployment: the server-rendered method list predates the
              // provision that just completed, so no row exists to label a
              // button — but `oauthProviderId` from the provision response
              // does, and the flow works. One generic button, never zero.
              <Button
                type="button"
                variant="primary"
                loading={phase.kind === "signing_in"}
                disabled={busy}
                onClick={() => {
                  void signIn();
                }}
              >
                {t(CONSOLE_MESSAGE_KEYS.sign_in_button_generic)}
              </Button>
            ) : (
              interactive.map((method) => (
                <Button
                  key={method.id}
                  type="button"
                  variant="primary"
                  loading={phase.kind === "signing_in"}
                  disabled={busy}
                  onClick={() => {
                    void signIn();
                  }}
                >
                  {buttonLabel(method, interactive.length)}
                </Button>
              ))
            )}
          </div>
          {phase.kind === "signing_in" && <Spinner label={t(CONSOLE_MESSAGE_KEYS.sign_in_pending)} />}
        </>
      )}

      {stage === "claim" && (
        <div className={styles.claim}>
          <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.setup_claim_heading)}</h3>
          {signedInEmail !== null && (
            <p className={styles.body}>{t(CONSOLE_MESSAGE_KEYS.setup_claim_signed_in_as, { email: signedInEmail })}</p>
          )}
          <div className={styles.actions}>
            <Button
              type="button"
              variant="primary"
              loading={phase.kind === "claiming"}
              disabled={busy}
              onClick={() => {
                void claim();
              }}
            >
              {t(CONSOLE_MESSAGE_KEYS.setup_claim_button)}
            </Button>
            {phase.kind === "claiming" && <Spinner label={t(CONSOLE_MESSAGE_KEYS.setup_claim_pending)} />}
          </div>
        </div>
      )}

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.messageArgs, phase.message)}
        </p>
      )}
    </section>
  );
}
