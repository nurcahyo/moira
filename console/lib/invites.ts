// @server-only
//
// Everything the console does with a raw invitation token.
//
// ============================================================================
// WHY THE TOKEN IS CONFINED TO THIS MODULE AND `OnceOnlySecretModal`
// ============================================================================
//
// There are exactly two moments a raw invitation token exists inside the
// console:
//
//   1. CREATION — Moira returns it once, in `AdminInviteSecretResponse.secret`,
//      and `modules/secrets/OnceOnlySecretModal.tsx` renders it once. That file
//      is the single entry on `no-secret-props.test.ts`'s allow-list.
//   2. REDEMPTION — the invitee arrives holding it. It enters through the URL
//      path of `/invite/[token]`, is exchanged **server-side** here, and never
//      becomes a prop, a client payload, or a query string.
//
// This module owns (2). Its two exchange functions take the token and return
// something that is not the token: `previewInvite` returns the constraint and
// the expiry, `redeemInvite` returns the granted `AdminIdentityRecord`. Neither
// echoes it back, neither logs it, and neither stores it.
//
// ============================================================================
// THE THREE PROPERTIES, AND WHERE EACH IS ENFORCED
// ============================================================================
//
//   NEVER LOGGED           this module contains no logging call at all, and
//                          `invite-token-containment.test.ts` asserts that by
//                          source scan across the whole console — a `console.*`
//                          logging call in any file that names a token variable
//                          is the failure it looks for.
//   NEVER IN A QUERY       `previewAdminInvite` / `redeemAdminInvite` put the
//                          token in the request BODY; the registry's `POST`
//                          shape is what makes that structural, and
//                          `moira-client.test.ts` asserts the URL does not
//                          contain it.
//   NEVER A CLIENT PAYLOAD `InviteAcceptPanel` receives the PREVIEW, not the
//                          token. The redemption POST is made by the browser to
//                          a console route handler that reads the token back out
//                          of the URL path server-side, so the token is never
//                          serialised into a props object or an RSC payload
//                          beyond the router state the browser already holds.
//
// ============================================================================
// WHAT THE URL PATH DOES AND DOES NOT PROTECT
// ============================================================================
//
// `/invite/[token]` puts a secret in a URL path. That is deliberate — it is the
// only shape in which a link can be shared out of band — and it is bounded on
// both sides:
//
//   * Moira's side, already shipped: prefix lookup before any Argon2 work, so it
//     is not a CPU-exhaustion oracle, and an identical `invite_not_found` for a
//     wrong prefix and a wrong hash, so it is not a guessing oracle.
//   * The console's side, this module plus the page: the token is exchanged
//     server-side on first load, and the page contacts no foreign origin, so it
//     never reaches a `Referer` chain or an analytics call.
//     `e2e/smoke.e2e.ts` asserts the no-foreign-origin half on the real route.
//
// What it does NOT protect against is the browser's own history and the URL bar.
// That is inherent to a shareable link and is why the token is single-use,
// time-capped at 72 hours, and Argon2id-hashed at rest.

import "server-only";

import type { MoiraClient } from "./moira-client";
import type {
  AdminIdentityRecord,
  AdminInviteConstraint,
  AdminInviteCreateRequest,
  AdminInvitePreviewResponse,
  AdminInviteSecretResponse,
} from "./types";
import type { ConsoleSessionIdentity } from "./moira-session";

/* -------------------------------------------------------------------------- */
/* The two bounds Moira enforces, mirrored so the form can respect them        */
/* -------------------------------------------------------------------------- */

/**
 * `MIN_INVITE_EXPIRY_SECONDS` in `src/domain/identity.rs`.
 *
 * Below it, `validated_invite_lifetime` returns `422 invalid_request` — NOT
 * `admin_invite_expiry_too_long`, which is the cap's code. Two bounds, two
 * codes; a form that only knew about the cap would render the floor's refusal as
 * a generic validation failure.
 */
export const MIN_INVITE_EXPIRY_SECONDS = 60;

/**
 * `MAX_INVITE_EXPIRY_SECONDS` in `src/domain/identity.rs` — 72 hours.
 *
 * A **hard cap, refused rather than clamped**: an operator who believes they
 * issued a 30-day invitation and silently received a 3-day one discovers the
 * difference at the worst possible moment. Mirrored here so `ExpiryPicker` can
 * refuse locally with the same number, and asserted against Moira's own constant
 * in `tests/unit/lib/invites.test.ts`.
 */
export const MAX_INVITE_EXPIRY_SECONDS = 72 * 60 * 60;

/** The form's default. Well inside both bounds; not a Moira value. */
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
 * holding the token, in a component that is not allow-listed.
 */
export function inviteBaseUrl(consoleOrigin: string): string {
  return `${consoleOrigin.replace(/\/+$/, "")}${INVITE_PATH_PREFIX}`;
}

/* -------------------------------------------------------------------------- */
/* Idempotency keys                                                           */
/* -------------------------------------------------------------------------- */

/**
 * A deterministic `Idempotency-Key` for creating an invitation.
 *
 * Derived from the invite's own identity `(constraint, value)` plus a caller
 * nonce, so a double-submit of the SAME form replays rather than minting a
 * second token, while a deliberate re-invitation of the same address after the
 * first expired is a different request.
 *
 * The replay returns `secret: null` with the sanitized record — the NORMAL case,
 * not a failure. `OnceOnlySecretModal` renders it as
 * `console.secret.already_shown`.
 */
export function inviteCreateIdempotencyKey(
  constraint: AdminInviteConstraint,
  value: string,
  nonce: string,
): string {
  return `admin-invite:${constraint}:${value.trim().toLowerCase()}:${nonce}`;
}

/* -------------------------------------------------------------------------- */
/* Exchanges — token in, something that is not the token out                  */
/* -------------------------------------------------------------------------- */

/**
 * Create an invitation. Returns the once-only envelope, unmodified.
 *
 * Deliberately a thin pass-through rather than a place that reshapes the
 * response: `AdminInviteSecretResponse` carries a REQUIRED `notice` that
 * `ApiKeySecretResponse` does not, and every rebuild of that object is a chance
 * to drop it. The caller hands the whole envelope to the modal.
 */
export async function createInvite(
  client: MoiraClient,
  body: AdminInviteCreateRequest,
  idempotencyKey: string,
): Promise<AdminInviteSecretResponse> {
  return client.createAdminInvite(body, { idempotencyKey });
}

/**
 * Exchange a raw token for what an anonymous holder may be told about it.
 *
 * Returns `constraint`, `value` and `expires_at` — nothing about the inviter,
 * the deployment, or the policy. The token is not part of the return value and
 * is not retained.
 */
export async function previewInvite(
  client: MoiraClient,
  token: string,
): Promise<AdminInvitePreviewResponse> {
  return client.previewAdminInvite(token);
}

/**
 * Redeem a raw token as the signed-in invitee.
 *
 * `email` and `email_verified` come from the JUST-VERIFIED SESSION and from
 * nowhere else — this signature takes a `ConsoleSessionIdentity`, which is what
 * `consoleSessionCheck` produces, so a caller cannot pass an address the browser
 * supplied.
 *
 * `client` must be built from the invitee's own session
 * (`moiraClientForSession`). One built by `moiraClientForSetup` carries the
 * bootstrap system key and `MoiraClient` throws on this operation rather than
 * sending it — see `bearer_only` in `lib/moira-client.ts`.
 *
 * `email_verified` is passed through rather than forced to `true`: Moira refuses
 * an unverified address with `403 admin_claim_email_not_verified`, and a console
 * that asserted `true` here would be lying to the server about the one claim the
 * grant's whole policy rests on.
 */
export async function redeemInvite(
  client: MoiraClient,
  token: string,
  identity: ConsoleSessionIdentity,
  idempotencyKey: string,
): Promise<AdminIdentityRecord> {
  return client.redeemAdminInvite(
    { token, email: identity.email, email_verified: identity.emailVerified },
    { idempotencyKey },
  );
}

/**
 * The `Idempotency-Key` for a redemption.
 *
 * Derived from the redeeming identity rather than from the token, because the
 * token is the thing that must not spread: a key echoing it would put it into a
 * header, an idempotency ledger row, and every access log along the way. The
 * IdP subject is stable, is already in the JWT, and is exactly as unique as the
 * grant this request creates.
 */
export function redeemIdempotencyKey(identity: ConsoleSessionIdentity): string {
  return `admin-invite-redeem:${identity.idpSubject}`;
}
