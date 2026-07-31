"use client";

// Create an admin invitation, and show its token exactly once.
//
// ============================================================================
// THE PRE-SUBMIT GATE **WARNS** ABOVE ONE PROVIDER (W5-B11 / decision W5-D11)
// ============================================================================
//
// Plan 09's body states the gate as a guarantee: it blocks any invitation that
// "would be refused at redemption", and "the client-side check is never more
// permissive than" Moira's. **That is unsatisfiable for more than one enabled
// provider, and the projection cannot be made to satisfy it.**
//
// Redemption applies EXACTLY ONE provider row. `admission_policy` resolves the
// row bound to the caller's trusted issuer, and falls back to an unbound row
// whose own `issuer` matches only when nothing is bound. The console reads
// `PublicAuthMethod`, which carries `id, method, display_name, issuer,
// discovery_url, authorization_url, client_id, jwks_url, requested_scopes,
// allowed_email_domains` — and **no `trusted_jwt_issuer_id`, no `created_at`,
// no enabled/ordering data**. So it cannot tell which rows are candidates, nor
// which one wins. All it can compute is a UNION.
//
// Three ways to respond, and only one is honest:
//
//   BLOCK ON THE UNION      under-strict: with two enabled rows the union is
//                           strictly wider than the governing row, so it passes
//                           invitations redemption refuses — the exact stranding
//                           the gate exists to prevent.
//   SILENTLY BEST-EFFORT    a stated invariant nothing exercises. That is the
//                           plan-08 B1 signature this project keeps paying for.
//   BLOCK WHERE IT IS EXACT, WARN WHERE IT IS NOT, AND SAY WHICH  <- taken.
//
// So:
//   0 providers enabled  -> BLOCK. Unambiguous: nobody can sign in at all.
//   1 provider enabled   -> BLOCK on an uncovered domain. The union provably
//                           equals the governing row, so the gate is exact.
//   N providers enabled  -> WARN, with the reason, and allow submission.
//
// The real safety net is shipped and stronger than the gate anyway: a
// policy-denied redemption does **not** consume the invitation
// (`a_policy_denied_redemption_leaves_the_invite_pending_and_the_same_link_still_works`),
// so the same link works once the allow-list widens. The warning copy says so.
//
// UI GATING ONLY. Moira's redeem-time check is the authority, on every branch.
//
// ============================================================================
// THE ONCE-ONLY TOKEN IS HELD AS AN ENVELOPE, NEVER AS A BINDING
// ============================================================================
//
// This file mounts `OnceOnlySecretModal`, which is the single entry on
// `no-secret-props.test.ts`'s allow-list. Mounting it means the response has to
// live in state between the fetch and the render — and the shape of that state
// is the whole question.
//
//   `const [secret, setSecret] = useState<string | null>(null)` would create a
//   second standalone binding of the plaintext token, in a file that is NOT
//   allow-listed, and it is the obvious way to write this.
//
// So the state holds the WHOLE `AdminInviteSecretResponse` and the token is read
// as a property at the JSX site. One binding, at the modal, which is what that
// component's header claims and what `no-secret-props.test.ts` rule (c) now
// asserts: this file must not declare a standalone `secret` identifier, and it
// must be the only file that mounts the modal.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { ExpiryPicker } from "@/components/molecules/ExpiryPicker";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { DEFAULT_INVITE_EXPIRY_SECONDS } from "@/lib/invite-bounds";
import type { AdminInviteConstraint, AdminInviteSecretResponse, ResponseText } from "@/lib/types";
import { OnceOnlySecretModal } from "@/modules/secrets/OnceOnlySecretModal";

import styles from "./InviteAdminForm.module.css";

/** The console's own create endpoint. Not Moira's. */
const CREATE_ENDPOINT = "/api/admins/invites";

/** What the server resolved about this deployment's sign-in providers. */
export interface InviteProviderPolicy {
  /** How many `auth_provider_settings` rows are enabled and resolvable. */
  readonly enabledProviderCount: number;
  /** The UNION of `allowed_email_domains` across them. Lower-cased. */
  readonly allowedEmailDomains: readonly string[];
}

export interface InviteAdminFormProps {
  readonly policy: InviteProviderPolicy;
  /** e.g. `https://console.example/invite`. The modal appends the token. */
  readonly inviteBaseUrl: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites reload the server data. */
  readonly onCreated?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly messageKey: string; readonly text: ResponseText | null };

/** The domain half of an address, or the value itself in domain mode. */
export function invitedDomain(constraint: AdminInviteConstraint, value: string): string | null {
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "") return null;
  if (constraint === "domain") return trimmed;
  const at = trimmed.lastIndexOf("@");
  if (at <= 0 || at === trimmed.length - 1) return null;
  return trimmed.slice(at + 1);
}

/**
 * The pre-submit verdict. Exported so its three branches can be tested
 * directly rather than through the DOM.
 *
 * `"warn"` is a first-class outcome, not an absence of a verdict — that is the
 * whole of W5-D11.
 */
export type GateVerdict =
  | { readonly kind: "allow" }
  | { readonly kind: "warn"; readonly messageKey: string }
  | { readonly kind: "block"; readonly messageKey: string };

export function gateVerdict(
  policy: InviteProviderPolicy,
  constraint: AdminInviteConstraint,
  value: string,
): GateVerdict {
  if (policy.enabledProviderCount === 0) {
    return { kind: "block", messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_no_enabled_provider };
  }
  if (policy.enabledProviderCount > 1) {
    // The union is strictly wider than the governing row, and under F23 it can
    // also be narrower in the other direction. Neither a block nor a silent pass
    // would be honest; a warning that names the limit is.
    return { kind: "warn", messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_multi_provider_warning };
  }
  const domain = invitedDomain(constraint, value);
  if (domain === null) return { kind: "allow" };
  const allowed = policy.allowedEmailDomains.some((entry) => entry.toLowerCase() === domain);
  return allowed
    ? { kind: "allow" }
    : { kind: "block", messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_domain_not_in_allow_list };
}

export function InviteAdminForm({
  policy,
  inviteBaseUrl,
  fetchImpl,
  onCreated,
}: InviteAdminFormProps) {
  const [constraint, setConstraint] = useState<AdminInviteConstraint>("email");
  const [value, setValue] = useState("");
  const [expiresInSeconds, setExpiresInSeconds] = useState(DEFAULT_INVITE_EXPIRY_SECONDS);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  // THE WHOLE ENVELOPE, never the token on its own — see the header.
  const [created, setCreated] = useState<AdminInviteSecretResponse | null>(null);

  const verdict = gateVerdict(policy, constraint, value);
  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    if (value.trim() === "") {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.admins_invite_value_required,
        text: null,
      });
      return;
    }
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(CREATE_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          constraint,
          value: value.trim(),
          expires_in_seconds: expiresInSeconds,
        }),
      });
    } catch {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.admins_request_failed,
        text: null,
      });
      return;
    }

    let body: unknown;
    try {
      body = await response.json();
    } catch {
      body = undefined;
    }

    if (!response.ok) {
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setPhase({
        kind: "failed",
        messageKey:
          text?.message_key ?? error?.message_key ?? CONSOLE_MESSAGE_KEYS.admins_request_failed,
        text,
      });
      return;
    }

    setPhase({ kind: "idle" });
    // `secret === null` here is the NORMAL idempotent-replay case, not a
    // failure: the stored replay body is the sanitized record. The modal renders
    // it as `console.secret.already_shown`.
    setCreated(body as AdminInviteSecretResponse);
    onCreated?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.admins_invite_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.admins_invite_heading)}</h2>

      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>
          {t(CONSOLE_MESSAGE_KEYS.admins_invite_constraint_label)}
        </legend>
        {(["email", "domain"] as const).map((option) => (
          <label key={option} className={styles.choice}>
            <input
              type="radio"
              name="admin-invite-constraint"
              value={option}
              checked={constraint === option}
              disabled={pending}
              onChange={() => setConstraint(option)}
            />
            {option === "email"
              ? t(CONSOLE_MESSAGE_KEYS.admins_invite_constraint_email)
              : t(CONSOLE_MESSAGE_KEYS.admins_invite_constraint_domain)}
          </label>
        ))}
      </fieldset>

      <FormField
        label={
          constraint === "email"
            ? t(CONSOLE_MESSAGE_KEYS.admins_invite_value_label_email)
            : t(CONSOLE_MESSAGE_KEYS.admins_invite_value_label_domain)
        }
        hint={
          constraint === "email"
            ? t(CONSOLE_MESSAGE_KEYS.admins_invite_value_hint_email)
            : t(CONSOLE_MESSAGE_KEYS.admins_invite_value_hint_domain)
        }
        required
        {...(verdict.kind === "block" && value.trim() !== ""
          ? { error: t(verdict.messageKey) }
          : {})}
        inputProps={{
          value,
          disabled: pending,
          onChange: (event) => setValue(event.target.value),
        }}
      />

      <ExpiryPicker seconds={expiresInSeconds} onChange={setExpiresInSeconds} disabled={pending} />

      {/* The gate's own verdict, always rendered in the same place so that the
          difference between "blocked" and "warned" is visible rather than
          inferred from whether the button is enabled. */}
      {verdict.kind !== "allow" && (
        <p
          className={verdict.kind === "block" ? styles.blocked : styles.warned}
          role={verdict.kind === "block" ? "alert" : "status"}
        >
          {t(verdict.messageKey)}
        </p>
      )}

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={verdict.kind === "block"}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.admins_invite_submit)}
      </Button>

      <p className={styles.activity} role="status" aria-live="polite">
        {pending && t(CONSOLE_MESSAGE_KEYS.admins_invite_pending)}
      </p>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}

      {created !== null && (
        <OnceOnlySecretModal
          open
          secret={created.secret ?? null}
          resource={created.resource}
          notice={created.notice}
          inviteBaseUrl={inviteBaseUrl}
          onDismiss={() => setCreated(null)}
        />
      )}
    </section>
  );
}
