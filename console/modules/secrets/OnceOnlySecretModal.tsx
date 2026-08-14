"use client";

// The once-only credential, shown exactly once.
//
// ============================================================================
// TWO CALLERS, ONE COMPONENT, AND THAT IS THE RULE RATHER THAN AN ACCIDENT
// ============================================================================
//
// This started as the invitation-token modal. Issue #180 needed the same thing
// for a minted consumer key, and the obvious move — a second modal beside this
// one — is exactly what `no-secret-props.test.ts` forbids: its allow-list is
// asserted to hold AT MOST ONE entry, because "which component may hold the
// plaintext" is a one-answer question. So this component was generalised
// instead, and the generalisation is deliberately thin: two optional props and
// a catalog key, no discriminated union, no envelope-shaped parameter.
//
//   `inviteBaseUrl`   present only for an invitation, where a link exists to
//                     compose. A consumer key has no redemption URL.
//   `notice`          Moira's own `ResponseText`, present only on the invitation
//                     envelope. `ApiKeySecretResponse` carries none, so the
//                     consumer-key caller passes `noticeKey` and the console's
//                     own copy is rendered rather than a fabricated
//                     server-shaped message.
//   `secretLabelKey`  "Invitation token" or "Consumer key". A label that said
//                     the wrong one is a small lie about what the operator is
//                     holding.
//   `headingKey`      the same problem one line higher up. The default reads
//                     "Invitation created", which above a minted API key is
//                     wrong in the first words an operator reads.
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
import type { ResponseText } from "@/lib/types";

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
  /**
   * The sanitized record. Carries no plaintext by construction.
   *
   * Structural rather than one named DTO: an invitation always has an
   * `expires_at` and a consumer key need not, and the expiry line is the only
   * thing this component reads off it. Widening it to a union of two records
   * would let this file reach for fields neither caller intends it to render.
   */
  readonly resource: { readonly expires_at?: string | null };
  /**
   * Moira's own success notice, when the envelope carries one.
   *
   * `AdminInviteSecretResponse` requires it; `ApiKeySecretResponse` has no such
   * field, so that caller passes `noticeKey` instead. Fabricating a
   * `ResponseText` for it would put console copy behind a shape that claims to
   * be Moira's.
   */
  readonly notice?: ResponseText;
  /** The console's own notice copy, for an envelope that carries none. */
  readonly noticeKey?: string;
  /**
   * The dialog's heading and accessible name.
   *
   * Defaults to the invitation wording this component shipped with. A consumer
   * key passes its own: the default reads "Invitation created", which is a
   * plainly wrong label to put above a minted API key — and it is the FIRST
   * thing an operator reads, so getting it wrong is not cosmetic.
   */
  readonly headingKey?: string;
  /** Which credential this is, for the plaintext field's label. */
  readonly secretLabelKey?: string;
  /**
   * e.g. `https://console.example/invite`. The token is appended here.
   *
   * Omitted when there is no link to compose — a consumer key is presented on a
   * header, not redeemed at a URL.
   */
  readonly inviteBaseUrl?: string;
  readonly open: boolean;
  readonly onDismiss: () => void;
}

export function OnceOnlySecretModal({
  secret,
  resource,
  notice,
  noticeKey,
  headingKey,
  secretLabelKey,
  inviteBaseUrl,
  open,
  onDismiss,
}: OnceOnlySecretModalProps) {
  const tokenId = useId();
  const linkId = useId();

  // Composed inline, not stored: one expression, one holder.
  const inviteUrl =
    secret === null || inviteBaseUrl === undefined
      ? null
      : `${inviteBaseUrl.replace(/\/+$/, "")}/${encodeURIComponent(secret)}`;

  return (
    <Dialog open={open} label={t(headingKey ?? CONSOLE_MESSAGE_KEYS.secret_modal_heading)}>
      <h2 className={styles.heading}>
        {t(headingKey ?? CONSOLE_MESSAGE_KEYS.secret_modal_heading)}
      </h2>

      {/* Moira's own notice when the envelope carries one, through the i18n
          helper — `message_args` may be absent, `message` is the fallback — and
          the console's own catalogued copy when it does not. */}
      <p className={styles.notice}>
        {notice === undefined
          ? t(noticeKey ?? CONSOLE_MESSAGE_KEYS.secret_shown_once)
          : t(notice.message_key, notice.message_args, notice.message)}
      </p>

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
              {t(secretLabelKey ?? CONSOLE_MESSAGE_KEYS.secret_token_label)}
            </span>
            <code className={styles.value} id={tokenId}>
              {secret}
            </code>
            <CopyButton targetId={tokenId} aria-describedby={`${tokenId}-label`} />
          </div>

          {/* Only where there is a link to compose. A consumer key is presented
              on a request header and has no redemption URL, and a field showing
              `null` would invite somebody to make one up. */}
          {inviteUrl !== null && (
            <div className={styles.field}>
              <span className={styles.label} id={`${linkId}-label`}>
                {t(CONSOLE_MESSAGE_KEYS.secret_link_label)}
              </span>
              <code className={styles.value} id={linkId}>
                {inviteUrl}
              </code>
              <CopyButton targetId={linkId} aria-describedby={`${linkId}-label`} />
            </div>
          )}
        </>
      )}

      {/* An unbounded credential is stated as such. A blank expiry line reads as
          missing data, which is the wrong impression to leave about a key that
          works until somebody revokes it. */}
      <p className={styles.expiry}>
        {resource.expires_at === undefined || resource.expires_at === null
          ? t(CONSOLE_MESSAGE_KEYS.secret_no_expiry)
          : t(CONSOLE_MESSAGE_KEYS.secret_expires_at, { expires_at: resource.expires_at })}
      </p>

      <Button type="button" variant="primary" onClick={onDismiss}>
        {t(CONSOLE_MESSAGE_KEYS.secret_dismiss)}
      </Button>
    </Dialog>
  );
}
