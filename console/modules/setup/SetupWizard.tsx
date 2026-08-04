"use client";

// The setup wizard's coordinator: the fifth organism, holding the four step
// organisms mounted on one page.
//
// ============================================================================
// NAVIGATION IS `reachableSetupStep`, NOT FREE-FORM LOCAL STATE
// ============================================================================
//
// The wizard keeps a CURSOR (which step the operator is looking at) but the
// cursor is clamped to `reachableSetupStep(state)` from `lib/setup-steps.ts` —
// the same pure gate the server enforces on the claim action itself. Moving
// backward (to re-read the welcome copy, or to fix the allow-list after a
// domain refusal) is always allowed; moving beyond the furthest reachable step
// is impossible, so the claim surface cannot exist on screen while a gate
// condition is unconfirmed.
//
// ============================================================================
// WHY EVERY STEP STAYS MOUNTED
// ============================================================================
//
// All step organisms are mounted together and inactive ones carry `hidden`.
// That is what "kembali ke AuthSettingsStep dengan state terjaga" requires: the
// auth-settings form's envelope (including the typed client secret) is that
// organism's LOCAL state, deliberately never lifted here — lifting it would put
// the secret into this component's props or state and hand it back down as a
// prop, the exact shape `no-secret-props` exists to forbid. Keeping the
// organism mounted preserves the envelope across step changes without this
// coordinator ever seeing it.
//
// ============================================================================
// WHAT THIS COMPONENT KNOWS ABOUT THE SESSION
// ============================================================================
//
// Once provisioning is complete it probes the console's OWN session endpoint
// (`/api/auth/get-session`) for a signed-in identity. The probe informs
// NAVIGATION only — the claim request is authorised by the BFF's session check
// against the same allow-list Moira applies, never by this hint.

import { useEffect, useState } from "react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  EMPTY_PROVISIONING_STATE,
  SETUP_STEP_ORDER,
  isProvisioningComplete,
  reachableSetupStep,
  type SetupProvisioningState,
  type SetupStepId,
  type SetupWizardState,
} from "@/lib/setup-steps";

import { AuthSettingsStep } from "./AuthSettingsStep";
import { DoneStep } from "./DoneStep";
import { SignInClaimStep } from "./SignInClaimStep";
import { WelcomeStep } from "./WelcomeStep";
import type { SetupViewModel } from "./setup-view";
import styles from "./SetupWizard.module.css";

export type { SetupMethodSummary, SetupViewModel } from "./setup-view";


/** Better Auth's session read, mounted by the console itself. */
const SESSION_ENDPOINT = "/api/auth/get-session";

const STEP_LABEL_KEYS: Readonly<Record<SetupStepId, string>> = {
  welcome: CONSOLE_MESSAGE_KEYS.setup_step_welcome,
  auth_settings: CONSOLE_MESSAGE_KEYS.setup_step_auth_settings,
  sign_in: CONSOLE_MESSAGE_KEYS.setup_step_sign_in,
  claim: CONSOLE_MESSAGE_KEYS.setup_step_claim,
  done: CONSOLE_MESSAGE_KEYS.setup_step_done,
};

export interface SetupWizardProps {
  readonly view: SetupViewModel;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites navigate for real. */
  readonly navigate?: (url: string) => void;
}

function stepIndex(step: SetupStepId): number {
  return SETUP_STEP_ORDER.indexOf(step);
}

export function SetupWizard({ view, fetchImpl, navigate }: SetupWizardProps) {
  const [provisioning, setProvisioning] = useState<SetupProvisioningState>(
    EMPTY_PROVISIONING_STATE,
  );
  const [oauthProviderId, setOauthProviderId] = useState<string | null>(null);
  const [signedInEmail, setSignedInEmail] = useState<string | null>(null);
  const [claimSucceeded, setClaimSucceeded] = useState(false);
  const [claimedEmail, setClaimedEmail] = useState<string | null>(null);
  const [refusedDomain, setRefusedDomain] = useState<string | null>(null);
  const [cursor, setCursor] = useState<SetupStepId>(view.kind === "claimed" ? "done" : "welcome");

  const claimed = view.kind === "claimed" || (view.kind === "ready" && view.claimed);
  const wizard: SetupWizardState = {
    claimed,
    provisioning,
    signedInWithAllowedIdentity: signedInEmail !== null,
    claimSucceeded,
  };
  const reachable = reachableSetupStep(wizard);
  // The clamp: backwards is free, beyond `reachable` is impossible.
  const active: SetupStepId = stepIndex(cursor) <= stepIndex(reachable) ? cursor : reachable;

  const provisioningComplete = isProvisioningComplete(provisioning);

  useEffect(() => {
    if (!provisioningComplete || signedInEmail !== null || claimSucceeded || claimed) return;
    let cancelled = false;
    const send = fetchImpl ?? globalThis.fetch;
    void (async () => {
      try {
        const response = await send(SESSION_ENDPOINT, {
          headers: { accept: "application/json" },
        });
        if (!response.ok) return;
        const body = (await response.json()) as { user?: { email?: unknown } } | null;
        const email = body?.user?.email;
        if (!cancelled && typeof email === "string" && email !== "") {
          setSignedInEmail(email);
        }
      } catch {
        // Signed out until proven otherwise; the claim path re-checks anyway.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [provisioningComplete, signedInEmail, claimSucceeded, claimed, fetchImpl]);

  if (view.kind === "unavailable") {
    return (
      <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_unavailable_heading)}>
        <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.setup_unavailable_heading)}</h2>
        <p className={styles.problem} role="alert">
          {t(view.messageKey, view.messageArgs, view.message)}
        </p>
      </section>
    );
  }

  const methods = view.kind === "ready" ? view.methods : [];

  function onProvisioned(state: SetupProvisioningState, providerId: string | null): void {
    setProvisioning(state);
    if (providerId !== null) setOauthProviderId(providerId);
    setRefusedDomain(null);
    if (isProvisioningComplete(state)) setCursor("sign_in");
  }

  function onClaimed(email: string | null): void {
    setClaimedEmail(email);
    setClaimSucceeded(true);
    setCursor("done");
  }

  function onDomainRefused(domain: string): void {
    setRefusedDomain(domain === "" ? null : domain);
    setCursor("auth_settings");
  }

  const showSignInClaim = provisioningComplete && (active === "sign_in" || active === "claim");

  return (
    <div className={styles.wizard}>
      <nav aria-label={t(CONSOLE_MESSAGE_KEYS.setup_steps_label)}>
        <ol className={styles.steps}>
          {SETUP_STEP_ORDER.map((step) => (
            <li
              key={step}
              className={step === active ? styles.stepactive : styles.stepitem}
              aria-current={step === active ? "step" : undefined}
            >
              {t(STEP_LABEL_KEYS[step])}
            </li>
          ))}
        </ol>
      </nav>

      <div hidden={active !== "welcome"}>
        <WelcomeStep onContinue={() => setCursor("auth_settings")} />
      </div>

      <div hidden={active !== "auth_settings"}>
        <AuthSettingsStep
          methods={methods}
          refusedDomain={refusedDomain}
          onProvisioned={onProvisioned}
          {...(fetchImpl === undefined ? {} : { fetchImpl })}
        />
      </div>

      {showSignInClaim && (
        <SignInClaimStep
          stage={reachable === "claim" ? "claim" : "sign_in"}
          methods={methods.filter((method) => method.interactive)}
          oauthProviderId={oauthProviderId}
          signedInEmail={signedInEmail}
          provisioning={provisioning}
          onClaimed={onClaimed}
          onDomainRefused={onDomainRefused}
          {...(fetchImpl === undefined ? {} : { fetchImpl })}
          {...(navigate === undefined ? {} : { navigate })}
        />
      )}

      {active === "done" && <DoneStep email={claimedEmail} />}
    </div>
  );
}
