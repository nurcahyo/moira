"use client";

// Correct the sign-in provider. The one form that can repair a claimed
// deployment's OAuth configuration.
//
// ============================================================================
// BLANK MEANS "LEAVE IT", AND THAT IS FORCED BY MOIRA
// ============================================================================
//
// Every patch column is written `coalesce($n, column)`, so an absent key means
// "leave it" and there is NO way to clear a field through this API. The fields
// are therefore PRE-FILLED from the live row and a blank one is a no-op rather
// than a deletion — which also makes the common edit (widen the allow-list)
// safe: everything else round-trips unchanged.
//
// The client SECRET is the exception. It is never pre-filled — the console
// cannot read it back, and a field that appeared to hold it would be a lie —
// and blank means "keep the stored one".
//
// ============================================================================
// THE ONE RULE THIS FORM CANNOT ENFORCE
// ============================================================================
//
// A moving client id needs its secret in the same save, or the console's seal is
// left bound to the old id and sign-in stops working. This form WARNS as soon as
// the field diverges, but the refusal that matters is on the server
// (`lib/auth-settings.ts`), before any request reaches Moira. A client-side check
// alone is a suggestion.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AuthProviderSummary } from "@/lib/auth-settings-view";
import type { ResponseText } from "@/lib/types";

import styles from "./ProviderUpdateForm.module.css";

/** The console's own endpoint. Not Moira's. */
const SAVE_ENDPOINT = "/api/settings/auth";

export interface ProviderUpdateFormProps {
  readonly provider: AuthProviderSummary;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites re-read the server data. */
  readonly onSaved?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "saved" }
  | { readonly kind: "failed"; readonly messageKey: string; readonly text: ResponseText | null };

export function ProviderUpdateForm({ provider, fetchImpl, onSaved }: ProviderUpdateFormProps) {
  const [displayName, setDisplayName] = useState(provider.displayName);
  const [clientId, setClientId] = useState(provider.clientId ?? "");
  const [clientSecret, setClientSecret] = useState("");
  const [discoveryUrl, setDiscoveryUrl] = useState(provider.discoveryUrl ?? "");
  const [issuer, setIssuer] = useState(provider.issuer ?? "");
  const [tokenUrl, setTokenUrl] = useState(provider.tokenUrl ?? "");
  const [domains, setDomains] = useState(provider.allowedEmailDomains.join(", "));
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";
  // The warning, shown while there is still time to act on it.
  const movingClientId = clientId.trim() !== "" && clientId.trim() !== (provider.clientId ?? "");
  const needsSecret = movingClientId && clientSecret.trim() === "";

  async function submit(): Promise<void> {
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(SAVE_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          action: "update",
          display_name: displayName,
          client_id: clientId,
          client_secret: clientSecret,
          discovery_url: discoveryUrl,
          issuer,
          token_url: tokenUrl,
          allowed_email_domains: domains,
        }),
      });
    } catch {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.authsettings_request_failed,
        text: null,
      });
      return;
    }

    if (!response.ok) {
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        body = undefined;
      }
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setPhase({
        kind: "failed",
        messageKey:
          text?.message_key ??
          error?.message_key ??
          CONSOLE_MESSAGE_KEYS.authsettings_request_failed,
        text,
      });
      return;
    }

    // The secret is dropped from local state on success: it has been sealed, and
    // keeping it in a React state cell buys nothing and holds a live credential
    // in the page for as long as the tab is open.
    setClientSecret("");
    setPhase({ kind: "saved" });
    onSaved?.();
  }

  return (
    <section
      className={styles.panel}
      aria-label={t(CONSOLE_MESSAGE_KEYS.authsettings_update_heading)}
    >
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.authsettings_update_heading)}</h2>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_display_name_label)}
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_id_label)}
        inputProps={{
          value: clientId,
          disabled: pending,
          onChange: (event) => setClientId(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_secret_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.authsettings_secret_hint)}
        {...(needsSecret
          ? { error: t(CONSOLE_MESSAGE_KEYS.authsettings_secret_required_for_new_client_id) }
          : {})}
        inputProps={{
          value: clientSecret,
          type: "password",
          autoComplete: "new-password",
          disabled: pending,
          onChange: (event) => setClientSecret(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_discovery_url_label)}
        inputProps={{
          value: discoveryUrl,
          disabled: pending,
          onChange: (event) => setDiscoveryUrl(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_issuer_label)}
        inputProps={{
          value: issuer,
          disabled: pending,
          onChange: (event) => setIssuer(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_token_url_label)}
        inputProps={{
          value: tokenUrl,
          disabled: pending,
          onChange: (event) => setTokenUrl(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_allowed_domains_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.setup_auth_allowed_domains_hint)}
        required
        inputProps={{
          value: domains,
          disabled: pending,
          onChange: (event) => setDomains(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={needsSecret}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.authsettings_update_button)}
      </Button>

      {phase.kind === "saved" && (
        <p className={styles.saved} role="status">
          {t(CONSOLE_MESSAGE_KEYS.authsettings_saved)}
        </p>
      )}

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}
    </section>
  );
}
