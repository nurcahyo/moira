// Invitation lifetimes and the public invitation path. CLIENT-SAFE, deliberately.
//
// ============================================================================
// WHY THIS IS A SEPARATE MODULE FROM `lib/invites.ts`
// ============================================================================
//
// `lib/invites.ts` carries `import "server-only"` and is in the CONTAINED set:
// no `"use client"` module may import it, and `components/**` may not either.
// That is right — it holds the token exchanges.
//
// But two of its facts are needed by a MOLECULE. `ExpiryPicker` has to know the
// bounds in order to offer only lifetimes Moira will accept, and it renders
// client-side. Re-declaring `60` and `259200` inside the component would be two
// copies of a server constant, which is exactly how a UI drifts from the rule it
// claims to respect.
//
// So the constants live here, in a module with no credential, no `process.env`,
// no transport and no server marker, and `lib/invites.ts` imports them rather
// than owning them. `server-only-guards.test.ts` asserts this module is
// genuinely client-safe rather than merely believed to be.
//
// ============================================================================
// THE TWO BOUNDS ARE DIFFERENT KINDS OF BOUND
// ============================================================================
//
// `validated_invite_lifetime` in `src/application/identity.rs`:
//
//   ABOVE MAX  `422 admin_invite_expiry_too_long`, a HARD CAP — refused rather
//              than clamped, so an operator who asks for a month is told no
//              instead of silently receiving three days.
//   BELOW MIN  `422 invalid_request`, a different code entirely.
//
// Plan 09's body names the cap and never mentions the floor. Both are mirrored
// here, and `tests/unit/lib/invites.test.ts` pins them against Moira's own
// `assert_eq!(MAX_INVITE_EXPIRY_SECONDS, 259_200)`.

/** `MIN_INVITE_EXPIRY_SECONDS` in `src/domain/identity.rs`. */
export const MIN_INVITE_EXPIRY_SECONDS = 60;

/** `MAX_INVITE_EXPIRY_SECONDS` in `src/domain/identity.rs` — 72 hours. */
export const MAX_INVITE_EXPIRY_SECONDS = 72 * 60 * 60;

/** The invitation form's default. Well inside both bounds; not a Moira value. */
export const DEFAULT_INVITE_EXPIRY_SECONDS = 24 * 60 * 60;

/** The public path an invitee lands on. One spelling, used by page and link. */
export const INVITE_PATH_PREFIX = "/invite";

/** Is this lifetime one Moira will accept? Both bounds, inclusive. */
export function isAcceptableInviteLifetime(seconds: number): boolean {
  return (
    Number.isInteger(seconds) &&
    seconds >= MIN_INVITE_EXPIRY_SECONDS &&
    seconds <= MAX_INVITE_EXPIRY_SECONDS
  );
}

/**
 * Where invitation links point, e.g. `https://console.example/invite`.
 *
 * `OnceOnlySecretModal` appends the token to this inside its own render, so the
 * console's origin travels to the browser and the token never leaves that one
 * expression. Passing a prebuilt link instead would create a second string
 * holding the token, in a component that is not allow-listed by
 * `no-secret-props.test.ts`.
 */
export function inviteBaseUrl(consoleOrigin: string): string {
  return `${consoleOrigin.replace(/\/+$/, "")}${INVITE_PATH_PREFIX}`;
}

/**
 * The console's own redemption endpoint for a token.
 *
 * Built from a path the CALLER already holds — `InviteAcceptPanel` reads it out
 * of `location.pathname` at click time rather than receiving the token as a
 * prop, which is the same design `CopyButton` uses for the once-only token: the
 * value stays in the one place it already is, and adding a holder is what a
 * prop would do.
 */
export function inviteRedeemPath(token: string): string {
  return `/api/invite/${encodeURIComponent(token)}/redeem`;
}
