"use client";

// The invitee's side of the invitation flow.
//
// ============================================================================
// IT NEVER RECEIVES THE TOKEN AS A PROP
// ============================================================================
//
// The obvious API is `<InviteAcceptPanel token={token} />`, and it is the exact
// thing `no-secret-props.test.ts` rule (a) forbids: `token` matches
// `SECRET_PROP_PATTERN`, and a secret on a rendering layer's props object is one
// careless `console.log`, one error boundary, or one dev-tools screenshot away
// from somewhere nobody meant.
//
// Instead this component reads the token out of `location.pathname` at CLICK
// TIME — which is precisely what `CopyButton` does with the once-only token, and
// for the same reason: the value is already in exactly one place the browser
// owns, and a prop would add a second holder. The page is `/invite/<token>`, so
// the last path segment IS the token and no additional channel is needed.
//
// The consequence for the server: nothing about the token is serialised into the
// RSC payload by this component. What the page DOES send down is the anonymous
// PREVIEW — `constraint`, `value`, `expires_at` — which is what Moira is willing
// to tell an unauthenticated holder and nothing more.
//
// ============================================================================
// THE THREE STATES, AND WHY `admin_identity_already_claimed` IS WORDED THAT WAY
// ============================================================================
//
//   no session   -> the page renders `SignInPanel` above this component; here we
//                   say why signing in is required.
//   session      -> one control, one POST, one outcome.
//   refused      -> Moira's key, rendered through `t()`.
//
// Two refusals get console-owned copy rather than the server's English, because
// their remedies are things only the console can phrase:
//
//   `admin_claim_domain_not_allowed` — decision D3. An invitation is a scoping
//   token, never a policy exemption, so an invitee at an unlisted domain is
//   refused even holding a valid link. It is rendered as an ACTIONABLE
//   INSTRUCTION ("ask whoever invited you to add your domain, then use this link
//   again") and never as a generic error banner — and the instruction is true:
//   a policy-denied redemption does not consume the invitation.
//
//   `admin_identity_already_claimed` — finding F24. It must NOT say "you already
//   have admin". `admin_identities` is keyed on `(issuer, subject)` with the
//   console's own issuer on every row, so under two providers minting one issuer
//   the holder of that grant may be a different human entirely. The copy says an
//   identity exists, not that it is yours.
//
// Neither is conflated with `invite_email_mismatch` / `invite_domain_mismatch`,
// whose remedy is a NEW invitation rather than a configuration change. Moira
// separates the codes on exactly those grounds and this panel keeps them apart.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { inviteRedeemPath } from "@/lib/invite-bounds";
import type { AdminInviteConstraint, ResponseText } from "@/lib/types";

import styles from "./InviteAcceptPanel.module.css";

/** Moira codes this panel answers with console-owned copy. */
const CONSOLE_OWNED_REFUSALS: Readonly<Record<string, string>> = {
  admin_claim_domain_not_allowed: CONSOLE_MESSAGE_KEYS.invite_domain_not_allowed,
  admin_identity_already_claimed: CONSOLE_MESSAGE_KEYS.invite_already_claimed,
};

export interface InviteAcceptPanelProps {
  /** From the ANONYMOUS preview. Never the token. */
  readonly constraint: AdminInviteConstraint;
  /** The invited address or bare domain. */
  readonly value: string;
  readonly expiresAt: string;
  /** Whether the visitor already has a console session. */
  readonly signedIn: boolean;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /**
   * Injected by the unit test. Shipped call sites read the browser's own URL.
   *
   * The seam exists because `location` is the SOURCE of the token, not a
   * convenience: a test that passed the token in would be exercising a different
   * component from the one that ships.
   */
  readonly resolvePathname?: () => string;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "done"; readonly notice: ResponseText | null }
  | { readonly kind: "refused"; readonly messageKey: string; readonly text: ResponseText | null };

/** The last path segment of `/invite/<token>`, or null. */
export function tokenFromPathname(pathname: string): string | null {
  const segments = pathname.split("/").filter((segment) => segment !== "");
  if (segments.length < 2 || segments[0] !== "invite") return null;
  const last = segments[segments.length - 1] ?? "";
  return last === "" ? null : decodeURIComponent(last);
}

export function InviteAcceptPanel({
  constraint,
  value,
  expiresAt,
  signedIn,
  fetchImpl,
  resolvePathname,
}: InviteAcceptPanelProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  async function accept(): Promise<void> {
    const pathname =
      resolvePathname?.() ?? (typeof globalThis.location === "undefined" ? "" : globalThis.location.pathname);
    const fromUrl = tokenFromPathname(pathname);
    if (fromUrl === null) {
      setPhase({
        kind: "refused",
        messageKey: CONSOLE_MESSAGE_KEYS.invite_request_failed,
        text: null,
      });
      return;
    }

    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      // No body at all. The token is in the path the handler already has, and
      // duplicating it into a payload would be a second copy travelling a second
      // route.
      response = await send(inviteRedeemPath(fromUrl), { method: "POST" });
    } catch {
      setPhase({
        kind: "refused",
        messageKey: CONSOLE_MESSAGE_KEYS.invite_request_failed,
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
      const error = (
        body as
          | { error?: { code?: string; text?: ResponseText; message_key?: string } }
          | undefined
      )?.error;
      const code = error?.code ?? "";
      const owned = CONSOLE_OWNED_REFUSALS[code];
      const text = error?.text ?? null;
      setPhase({
        kind: "refused",
        messageKey:
          owned ??
          text?.message_key ??
          error?.message_key ??
          CONSOLE_MESSAGE_KEYS.invite_request_failed,
        // The server's English is suppressed for the two console-owned
        // refusals: `t()` would otherwise fall back to it, and the whole point
        // of owning them is that the console's wording is the correct one.
        text: owned === undefined ? text : null,
      });
      return;
    }

    const record = body as { notice?: ResponseText } | undefined;
    setPhase({ kind: "done", notice: record?.notice ?? null });
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.invite_panel_label)}>
      <p className={styles.heading}>
        {constraint === "email"
          ? t(CONSOLE_MESSAGE_KEYS.invite_heading_email, { value })
          : t(CONSOLE_MESSAGE_KEYS.invite_heading_domain, { value })}
      </p>
      <p className={styles.expiry}>
        {t(CONSOLE_MESSAGE_KEYS.invite_expires_at, { expires_at: expiresAt })}
      </p>

      {!signedIn && <p className={styles.instruction}>{t(CONSOLE_MESSAGE_KEYS.invite_sign_in_first)}</p>}

      {signedIn && phase.kind !== "done" && (
        <Button
          type="button"
          variant="primary"
          loading={phase.kind === "pending"}
          onClick={() => {
            void accept();
          }}
        >
          {t(CONSOLE_MESSAGE_KEYS.invite_accept)}
        </Button>
      )}

      {/* Present before it is populated, so the announcement is not made by a
          region that was created in the same tick. */}
      <p className={styles.activity} role="status" aria-live="polite">
        {phase.kind === "pending" && t(CONSOLE_MESSAGE_KEYS.invite_accept_pending)}
        {phase.kind === "done" && t(CONSOLE_MESSAGE_KEYS.invite_accepted)}
      </p>

      {phase.kind === "done" && phase.notice !== null && (
        <p className={styles.notice}>
          {t(phase.notice.message_key, phase.notice.message_args, phase.notice.message)}
        </p>
      )}

      {phase.kind === "refused" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}
    </section>
  );
}
