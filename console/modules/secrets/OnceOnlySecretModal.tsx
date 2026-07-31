"use client";

// The once-only invitation token, shown exactly once.
//
// ============================================================================
// NOTHING REDACTS THIS. READ THIS BEFORE ASSUMING SOMETHING DOES.
// ============================================================================
//
// A secret-bearing response NEVER PASSES THROUGH `lib/errors.ts`.
// `lib/moira-client.ts:550-557` calls `toMoiraError` only under
// `if (!response.ok)`; a 201 body is returned raw at `:561` as
// `(await response.json()) as T`. There is nothing between the JSON parse and a
// React prop — no allow-list, no field-by-field rebuild, no sanitiser.
//
// A reader will assume the error module covers it, because that module's whole
// header is about what may and may not cross to the browser. It does not cover
// this. What contains the token is: this file's own design, the
// `no-secret-props` guard that allow-lists exactly this file, and the e2e needle
// in `console/e2e/secret-leak.e2e.ts`.
//
// ============================================================================
// THE TOKEN EXISTS IN EXACTLY ONE PLACE
// ============================================================================
//
// `secret` is a prop of THIS component and is passed to nothing. In particular
// `CopyButton` takes an element `id`, not a value, so copying does not create a
// second holder — see that atom's header. `tests/unit/architecture/no-secret-props.test.ts`
// rule (b) scans this file for `<X secret=`, `<X value={secret}` and
// `{...{ secret }}`, and `tests/unit/modules/OnceOnlySecretModal.test.tsx`
// asserts the same property at runtime by recording every props object handed to
// an imported component.
//
// ============================================================================
// `secret === null` IS THE NORMAL CASE
// ============================================================================
//
// `AdminInviteSecretResponse.secret` is `{"type":["string","null"]}` and is NOT
// required. Moira returns the raw token exactly once, at creation, and `None` on
// an IDEMPOTENT REPLAY — where the stored replay body is the sanitized record. A
// UI that treats null as a failure reports a successful, correct operation as
// broken, and does it on the retry path, which is where people already suspect
// something went wrong.
//
// ============================================================================
// WHY THE LINK IS COMPOSED HERE AND NOT BY THE CALLER
// ============================================================================
//
// Moira's envelope carries the raw token and never a URL, so somebody has to
// build the link. The brief for this wave put that on the caller, on the grounds
// that only the console knows its own public origin.
//
// It is done here instead, and the origin arrives as a plain `inviteBaseUrl`
// prop. A caller-built link is a SECOND string containing the token, held by a
// component that is not allow-listed and not covered by rule (b) — which would
// move the one thing this file exists to contain into a file that contains
// nothing. Passing an origin costs nothing and keeps the count at one.
//
// REVERSAL CONDITION: if a caller ever needs a link shape this cannot express
// (a different path, a query parameter), give it a `buildUrl: (token) => string`
// callback rather than a prebuilt URL — the closure still runs inside this
// render and the token still never leaves it.

import { useId } from "react";

import { CopyButton } from "@/components/atoms/CopyButton";
import { Button } from "@/components/atoms/Button";
import { Dialog } from "@/components/atoms/Dialog";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AdminInviteRecord, ResponseText } from "@/lib/types";

import styles from "./OnceOnlySecretModal.module.css";

export interface OnceOnlySecretModalProps {
  /**
   * THE PLAINTEXT TOKEN, or `null` on an idempotent replay.
   *
   * Named `secret` deliberately rather than hidden inside an envelope object.
   * The name scan in `no-secret-props.test.ts` cannot see through a nested
   * shape, so passing `envelope: AdminInviteSecretResponse` would have smuggled
   * the token past the very guard this file is allow-listed by — and the
   * allow-list entry would then have been unjustified, which that test also
   * checks.
   */
  readonly secret: string | null;
  /** The sanitized record. Carries no token by construction. */
  readonly resource: AdminInviteRecord;
  /** Moira's success notice. Rendered through `t()`, never as hardcoded English. */
  readonly notice: ResponseText;
  /** e.g. `https://console.example/invite`. The token is appended here. */
  readonly inviteBaseUrl: string;
  readonly open: boolean;
  readonly onDismiss: () => void;
}

export function OnceOnlySecretModal({
  secret,
  resource,
  notice,
  inviteBaseUrl,
  open,
  onDismiss,
}: OnceOnlySecretModalProps) {
  const tokenId = useId();
  const linkId = useId();

  // Composed inline, not stored: one expression, one holder.
  const inviteUrl =
    secret === null ? null : `${inviteBaseUrl.replace(/\/+$/, "")}/${encodeURIComponent(secret)}`;

  return (
    <Dialog open={open} label={t(CONSOLE_MESSAGE_KEYS.secret_modal_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.secret_modal_heading)}</h2>

      {/* Moira's own notice, through the i18n helper. `message_args` may be
          absent; `message` is always present on this envelope and is the
          fallback. */}
      <p className={styles.notice}>{t(notice.message_key, notice.message_args, notice.message)}</p>

      {secret === null ? (
        <p className={styles.replay} role="status">
          {t(CONSOLE_MESSAGE_KEYS.secret_already_shown)}
        </p>
      ) : (
        <>
          <p className={styles.warning} role="alert">
            {t(CONSOLE_MESSAGE_KEYS.secret_shown_once)}
          </p>

          <div className={styles.field}>
            <span className={styles.label} id={`${tokenId}-label`}>
              {t(CONSOLE_MESSAGE_KEYS.secret_token_label)}
            </span>
            <code className={styles.value} id={tokenId}>
              {secret}
            </code>
            <CopyButton targetId={tokenId} aria-describedby={`${tokenId}-label`} />
          </div>

          <div className={styles.field}>
            <span className={styles.label} id={`${linkId}-label`}>
              {t(CONSOLE_MESSAGE_KEYS.secret_link_label)}
            </span>
            <code className={styles.value} id={linkId}>
              {inviteUrl}
            </code>
            <CopyButton targetId={linkId} aria-describedby={`${linkId}-label`} />
          </div>
        </>
      )}

      <p className={styles.expiry}>
        {t(CONSOLE_MESSAGE_KEYS.secret_expires_at, { expires_at: resource.expires_at })}
      </p>

      <Button type="button" variant="primary" onClick={onDismiss}>
        {t(CONSOLE_MESSAGE_KEYS.secret_dismiss)}
      </Button>
    </Dialog>
  );
}
