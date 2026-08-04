"use client";

// The auth-settings step: the one form that drives the whole ordered
// provisioning sequence (trusted issuer -> provider -> console secret -> enable)
// through `POST /api/setup {action: "provision"}`.
//
// ============================================================================
// THE SECRET STAYS INSIDE THIS FILE
// ============================================================================
//
// The OAuth client secret lives in this organism's own form-state envelope and
// goes exactly one place: the body of the fetch to the console's OWN
// `/api/setup` route. It is never a prop on any atom, molecule, or organism
// (`no-secret-props.test.ts` rule (a) scans every `*Props` interface), never
// pre-filled from anywhere (the console has no read-back path for it, by D7),
// and never part of what this component reports upward — `onProvisioned`
// receives the display-safe provisioning state, which has no secret field.
//
// The organism talks to `/api/setup` and to nothing else; it cannot address
// Moira, so the secret cannot reach a Moira request from here. The BFF route
// keeps the same property on its side via the `storeClientSecret` closure.
//
// ============================================================================
// FAILURE HANDLING MIRRORS `SetupProvisioningError`, STEP BY STEP
// ============================================================================
//
// A partial write comes back as `{error: {code: "setup_provisioning_failed",
// step, remedy, message_key, state, requires_client_secret_re_entry}}`. The
// message key is one of the four `console.error.*` provisioning keys and is
// rendered through `t()`; `state` is recorded and sent back as `resume` so a
// retry REPLAYS the submission instead of restarting it — restarting is what
// re-POSTs the trusted issuer into a unique index Moira reports as an opaque
// 500. `retry_or_discard_provider` additionally offers Discard; the enable
// failure does not ask for the secret again, because the console already
// stored it (`requires_client_secret_re_entry` is derived from the state).

import { useEffect, useId, useRef, useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Input } from "@/components/atoms/Input";
import { Label } from "@/components/atoms/Label";
import { Spinner } from "@/components/atoms/Spinner";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  isProvisioningComplete,
  narrowProvisioningState,
  type SetupProvisioningState,
} from "@/lib/setup-steps";
import type { JsonValue } from "@/lib/types";

import type { SetupMethodSummary } from "./setup-view";
import styles from "./AuthSettingsStep.module.css";

/** The one door every wizard mutation goes through. */
const SETUP_ENDPOINT = "/api/setup";


export interface AuthSettingsStepProps {
  /** Existing provider rows, display-safe. Presence only — see `setup-view.ts`. */
  readonly methods: readonly SetupMethodSummary[];
  /**
   * The wizard's current provisioning state — server-rehydrated on load,
   * updated by every provision response. When it names a provider row, a save
   * RESUMES against that row (the BFF patches it) instead of creating a second
   * one, and a stored console secret means the operator is not asked to re-type
   * a secret the console already holds. Display-safe; never carries the secret.
   */
  readonly provisioning: SetupProvisioningState;
  /** A claim was refused for this domain; focus the allow-list and instruct. */
  readonly refusedDomain: string | null;
  /** Reports the state Moira confirmed. Never carries the secret. */
  readonly onProvisioned: (state: SetupProvisioningState, providerId: string | null) => void;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

/** The whole form, held as ONE envelope inside this organism. */
interface FormEnvelope {
  readonly method: "google_oauth" | "generic_oidc";
  readonly displayName: string;
  readonly clientId: string;
  readonly clientSecret: string;
  readonly discoveryUrl: string;
  readonly issuer: string;
  readonly authorizationUrl: string;
  readonly tokenUrl: string;
  readonly allowedDomains: string;
}

const EMPTY_FORM: FormEnvelope = {
  method: "google_oauth",
  displayName: "",
  clientId: "",
  clientSecret: "",
  discoveryUrl: "",
  issuer: "",
  authorizationUrl: "",
  tokenUrl: "",
  allowedDomains: "",
};

type FieldName = "displayName" | "clientId" | "clientSecret" | "endpoints" | "allowedDomains";

/** Per-field refusal keys — the same vocabulary the BFF answers with. */
const FIELD_ERROR_KEYS: Readonly<Record<FieldName, string>> = {
  displayName: CONSOLE_MESSAGE_KEYS.setup_display_name_required,
  clientId: CONSOLE_MESSAGE_KEYS.setup_client_id_required,
  clientSecret: CONSOLE_MESSAGE_KEYS.setup_client_secret_required,
  endpoints: CONSOLE_MESSAGE_KEYS.setup_issuer_or_discovery_required,
  allowedDomains: CONSOLE_MESSAGE_KEYS.setup_allowed_email_domains_required,
};

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | {
      readonly kind: "failed";
      readonly messageKey: string;
      readonly message?: string;
      readonly messageArgs?: JsonValue;
      readonly canDiscard: boolean;
    };

/** Comma/whitespace-separated domains, trimmed, empties dropped. */
function parseDomains(raw: string): readonly string[] {
  return raw
    .split(/[,\s]+/)
    .map((domain) => domain.trim())
    .filter((domain) => domain !== "");
}

export function AuthSettingsStep({
  methods,
  provisioning,
  refusedDomain,
  onProvisioned,
  fetchImpl,
}: AuthSettingsStepProps) {
  const [form, setForm] = useState<FormEnvelope>(EMPTY_FORM);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [fieldErrors, setFieldErrors] = useState<readonly FieldName[]>([]);
  const [blocked, setBlocked] = useState(false);

  // Stable per SUBMISSION so a retry replays the same idempotency keys.
  const submissionRef = useRef<string | null>(null);
  // The recorded partial state a retry sends back as `resume`.
  const resumeRef = useRef<SetupProvisioningState | null>(null);
  const domainsInputRef = useRef<HTMLInputElement>(null);

  const methodSelectId = useId();
  const domainsFieldId = useId();

  useEffect(() => {
    if (refusedDomain !== null) domainsInputRef.current?.focus();
  }, [refusedDomain]);

  function set<Name extends keyof FormEnvelope>(name: Name, value: FormEnvelope[Name]): void {
    setForm((previous) => ({ ...previous, [name]: value }));
  }

  /**
   * The state a save resumes against: the recorded partial state of a failed
   * attempt first, otherwise the wizard's provisioning state when it already
   * names a provider row — the domain-refusal re-save, or any save after a
   * reload/OAuth round trip. `null` means a genuinely fresh create.
   */
  function effectiveResume(): SetupProvisioningState | null {
    if (resumeRef.current !== null) return resumeRef.current;
    return provisioning.providerId !== null ? provisioning : null;
  }

  function validate(): readonly FieldName[] {
    const missing: FieldName[] = [];
    if (form.displayName.trim() === "") missing.push("displayName");
    if (form.clientId.trim() === "") missing.push("clientId");
    // The secret is required unless the console has ALREADY sealed one for the
    // row being re-saved: the domain-refusal instruction is "add the domain and
    // save again", and the operator may no longer hold a secret the console
    // does. Typing a new one still re-seals it.
    if (form.clientSecret === "" && effectiveResume()?.consoleSecretStored !== true) {
      missing.push("clientSecret");
    }
    if (
      form.discoveryUrl.trim() === "" &&
      (form.issuer.trim() === "" ||
        form.authorizationUrl.trim() === "" ||
        form.tokenUrl.trim() === "")
    ) {
      missing.push("endpoints");
    }
    if (parseDomains(form.allowedDomains).length === 0) missing.push("allowedDomains");
    return missing;
  }

  async function submit(): Promise<void> {
    const missing = validate();
    setFieldErrors(missing);
    if (missing.length > 0) {
      // BLOCKED, not sent: the BFF would refuse the same shapes as keyed 400s
      // one round trip later, and an empty allow-list would provision a
      // deployment nobody can ever become admin of.
      setBlocked(true);
      return;
    }
    setBlocked(false);
    setPhase({ kind: "pending" });

    submissionRef.current ??= crypto.randomUUID();
    const resume = effectiveResume();
    const optional = (value: string): string | null => (value.trim() === "" ? null : value.trim());
    const body: Record<string, unknown> = {
      action: "provision",
      method: form.method,
      display_name: form.displayName.trim(),
      client_id: form.clientId.trim(),
      client_secret: form.clientSecret,
      discovery_url: optional(form.discoveryUrl),
      issuer: optional(form.issuer),
      authorization_url: optional(form.authorizationUrl),
      token_url: optional(form.tokenUrl),
      allowed_email_domains: parseDomains(form.allowedDomains),
      submission_id: submissionRef.current,
      ...(resume === null ? {} : { resume }),
    };

    const send = fetchImpl ?? globalThis.fetch;
    let response: Response;
    try {
      response = await send(SETUP_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
    } catch {
      // The thrown cause is deliberately not read — same rule as SignInPanel.
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_request_unreachable, canDiscard: false });
      return;
    }

    let payload: Record<string, unknown> | null = null;
    try {
      payload = (await response.json()) as Record<string, unknown>;
    } catch {
      payload = null;
    }

    if (response.ok) {
      const state = narrowProvisioningState(payload?.["state"]);
      if (state === null) {
        setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_request_unreachable, canDiscard: false });
        return;
      }
      const providerId = payload?.["provider_id"];
      onProvisioned(state, typeof providerId === "string" ? providerId : null);
      if (isProvisioningComplete(state)) {
        // Done. The secret has been sealed console-side; nothing here needs it
        // any longer, so the envelope drops it. The SUBMISSION id is dropped
        // too: this submission is finished, and a later re-save (the
        // domain-refusal remedy) must not replay its idempotency keys against a
        // changed body — that is `409 idempotency_conflict`, forever.
        resumeRef.current = null;
        submissionRef.current = null;
        setForm((previous) => ({ ...previous, clientSecret: "" }));
        setPhase({ kind: "idle" });
        return;
      }
      // A 2xx whose state still fails a gate condition: refuse to advance.
      resumeRef.current = state;
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_auth_not_complete, canDiscard: false });
      return;
    }

    const error =
      payload !== null && typeof payload["error"] === "object" && payload["error"] !== null
        ? (payload["error"] as Record<string, unknown>)
        : null;

    if (error !== null && error["code"] === "setup_provisioning_failed") {
      const state = narrowProvisioningState(error["state"]);
      if (state !== null) {
        resumeRef.current = state;
        // Report the partial state upward so the wizard's model tracks what was
        // actually written — but NOT a complete one: a failed UPDATE of an
        // already-complete provider carries the untouched complete state, and
        // reporting it would both be redundant and advance the cursor away
        // from the failure the operator is looking at.
        if (!isProvisioningComplete(state)) onProvisioned(state, null);
      }
      setPhase({
        kind: "failed",
        messageKey:
          typeof error["message_key"] === "string"
            ? error["message_key"]
            : CONSOLE_MESSAGE_KEYS.setup_auth_not_complete,
        canDiscard: error["remedy"] === "retry_or_discard_provider",
      });
      return;
    }

    if (error !== null) {
      // A keyed refusal (400/404/409) or a passed-through Moira failure. Both
      // carry a message key; the Moira union nests it under `text`.
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
        canDiscard: false,
      });
      return;
    }

    setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.setup_request_unreachable, canDiscard: false });
  }

  function discard(): void {
    // Abandon the partial attempt: fresh submission, no resume state. The form
    // keeps its values except the secret, which the operator re-enters.
    resumeRef.current = null;
    submissionRef.current = null;
    setForm((previous) => ({ ...previous, clientSecret: "" }));
    setPhase({ kind: "idle" });
  }

  const pending = phase.kind === "pending";
  const errorFor = (name: FieldName): string | undefined =>
    fieldErrors.includes(name) ? t(FIELD_ERROR_KEYS[name]) : undefined;

  return (
    <section className={styles.step} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_auth_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.setup_auth_heading)}</h2>

      {methods.length > 0 && (
        <section className={styles.existing} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_auth_existing_heading)}>
          <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.setup_auth_existing_heading)}</h3>
          <ul className={styles.existinglist}>
            {methods.map((method) => (
              <li key={method.id} className={styles.existingrow}>
                <span className={styles.existingname}>{method.displayName}</span>
                {method.clientIdConfigured && (
                  // A masked value derived from the ROW'S PRESENCE. The console
                  // has no way to read the credential back, and does not try.
                  <span className={styles.masked}>{t(CONSOLE_MESSAGE_KEYS.setup_auth_existing_configured)}</span>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}

      {refusedDomain !== null && (
        <div className={styles.domainrefusal} role="alert">
          <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.setup_domain_not_allowed_title)}</h3>
          <p className={styles.body}>
            {t(CONSOLE_MESSAGE_KEYS.setup_domain_not_allowed_body, { domain: refusedDomain })}
          </p>
          <p className={styles.action}>{t(CONSOLE_MESSAGE_KEYS.setup_domain_not_allowed_action)}</p>
        </div>
      )}

      <div className={styles.form}>
        <div className={styles.field}>
          <Label htmlFor={methodSelectId}>{t(CONSOLE_MESSAGE_KEYS.setup_auth_method_label)}</Label>
          <select
            id={methodSelectId}
            className={styles.select}
            value={form.method}
            onChange={(event) =>
              set("method", event.currentTarget.value === "generic_oidc" ? "generic_oidc" : "google_oauth")
            }
          >
            <option value="google_oauth">{t(CONSOLE_MESSAGE_KEYS.setup_auth_method_google)}</option>
            <option value="generic_oidc">{t(CONSOLE_MESSAGE_KEYS.setup_auth_method_generic)}</option>
          </select>
        </div>

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_display_name_label)}
          required
          {...(errorFor("displayName") === undefined ? {} : { error: errorFor("displayName")! })}
          inputProps={{
            name: "display_name",
            value: form.displayName,
            onChange: (event) => set("displayName", event.currentTarget.value),
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_id_label)}
          required
          {...(errorFor("clientId") === undefined ? {} : { error: errorFor("clientId")! })}
          inputProps={{
            name: "client_id",
            value: form.clientId,
            onChange: (event) => set("clientId", event.currentTarget.value),
            autoComplete: "off",
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_secret_label)}
          hint={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_secret_hint)}
          required
          {...(errorFor("clientSecret") === undefined ? {} : { error: errorFor("clientSecret")! })}
          inputProps={{
            name: "client_secret",
            type: "password",
            // Write-only: the value can only ever be what the operator typed in
            // THIS session. Nothing pre-fills it — there is nothing to pre-fill
            // it from.
            value: form.clientSecret,
            onChange: (event) => set("clientSecret", event.currentTarget.value),
            autoComplete: "new-password",
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_discovery_url_label)}
          {...(errorFor("endpoints") === undefined ? {} : { error: errorFor("endpoints")! })}
          inputProps={{
            name: "discovery_url",
            value: form.discoveryUrl,
            onChange: (event) => set("discoveryUrl", event.currentTarget.value),
            inputMode: "url",
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_issuer_label)}
          inputProps={{
            name: "issuer",
            value: form.issuer,
            onChange: (event) => set("issuer", event.currentTarget.value),
            inputMode: "url",
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_authorization_url_label)}
          inputProps={{
            name: "authorization_url",
            value: form.authorizationUrl,
            onChange: (event) => set("authorizationUrl", event.currentTarget.value),
            inputMode: "url",
          }}
        />

        <FormField
          label={t(CONSOLE_MESSAGE_KEYS.setup_auth_token_url_label)}
          inputProps={{
            name: "token_url",
            value: form.tokenUrl,
            onChange: (event) => set("tokenUrl", event.currentTarget.value),
            inputMode: "url",
          }}
        />

        <div className={styles.field}>
          <Label htmlFor={domainsFieldId} required>
            {t(CONSOLE_MESSAGE_KEYS.setup_auth_allowed_domains_label)}
          </Label>
          <Input
            id={domainsFieldId}
            ref={domainsInputRef}
            name="allowed_email_domains"
            value={form.allowedDomains}
            invalid={fieldErrors.includes("allowedDomains")}
            aria-describedby={`${domainsFieldId}-note`}
            onChange={(event) => set("allowedDomains", event.currentTarget.value)}
          />
          {errorFor("allowedDomains") === undefined ? (
            <p id={`${domainsFieldId}-note`} className={styles.hint}>
              {t(CONSOLE_MESSAGE_KEYS.setup_auth_allowed_domains_hint)}
            </p>
          ) : (
            <p id={`${domainsFieldId}-note`} className={styles.error} role="alert">
              {errorFor("allowedDomains")}
            </p>
          )}
        </div>

        {blocked && (
          <p className={styles.error} role="alert">
            {t(CONSOLE_MESSAGE_KEYS.setup_auth_form_incomplete)}
          </p>
        )}

        <div className={styles.actions}>
          <Button
            type="button"
            variant="primary"
            loading={pending}
            onClick={() => {
              void submit();
            }}
          >
            {t(CONSOLE_MESSAGE_KEYS.setup_auth_submit)}
          </Button>
          {pending && <Spinner label={t(CONSOLE_MESSAGE_KEYS.setup_auth_pending)} />}
        </div>
      </div>

      {phase.kind === "failed" && (
        <div className={styles.failure} role="alert" aria-label={t(CONSOLE_MESSAGE_KEYS.setup_auth_failure_region)}>
          <p className={styles.body}>{t(phase.messageKey, phase.messageArgs, phase.message)}</p>
          <div className={styles.actions}>
            <Button
              type="button"
              variant="primary"
              onClick={() => {
                void submit();
              }}
            >
              {t(CONSOLE_MESSAGE_KEYS.setup_auth_retry)}
            </Button>
            {phase.canDiscard && (
              <Button type="button" variant="secondary" onClick={discard}>
                {t(CONSOLE_MESSAGE_KEYS.setup_auth_discard)}
              </Button>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
