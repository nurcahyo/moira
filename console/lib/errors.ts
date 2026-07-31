// Moira `ErrorResponse` -> client-safe discriminated union.
//
// THE BOUNDARY RULE. Moira's envelope is
// `{ error: { code, message_key, message, message_args, request_id, details } }`.
// `message_key`, `message_args` and `message` cross the server/client boundary.
// `request_id` and `details` DO NOT — `request_id` is logged, `details` is
// server-diagnostic material that has no business in an RSC payload. Nothing in
// this module returns the raw envelope, and `toMoiraError` builds its result
// field by field rather than spreading, so a new field added to Moira's
// `ErrorDetail` cannot leak by default.
//
// THE 401 RULE. `setup_claim_credential_required` is a 401 that means "the
// system key is missing or wrong", not "your session expired". Routing it into
// the session-revoked sign-out flow would sign the operator out of a console
// they are legitimately signed in to, mid-setup, and hide the actual cause. It
// is mapped to its own remedy and `isSessionExpired()` returns false for it.

import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import type { JsonValue, MoiraErrorResponse } from "./types";

/* -------------------------------------------------------------------------- */
/* Client-safe shapes                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The only part of a Moira error permitted to reach the browser.
 *
 * Rendered `t(messageKey, messageArgs, message)` — console catalog first, the
 * server-supplied `message` as fallback, the key itself as last resort.
 * Components receive this object; they never receive pre-formatted prose.
 */
export interface ClientSafeText {
  readonly messageKey: string;
  readonly message: string;
  readonly messageArgs: JsonValue;
}

/**
 * What the operator can actually do about it. The UI branches on this, not on
 * the status code — the mapping from (status, code) to remedy is this module's
 * whole job.
 */
export type MoiraRemedy =
  /** Change something in the form that was just submitted. */
  | "fix_form_input"
  /** Go back to the auth-settings step; the deployment's setup is incomplete. */
  | "fix_setup_configuration"
  /** The `X-Moira-System-Key` is missing or wrong. NOT a session problem. */
  | "fix_system_key"
  /** The console session is stale or revoked: clear it and redirect to /login. */
  | "reauthenticate"
  /** Authentication worked; this identity cannot be used. Sign in as someone else. */
  | "choose_different_identity"
  /** Authenticated and authenticated correctly, but not authorized. No self-service fix. */
  | "denied"
  /** A concurrent write or an in-flight idempotent request. Re-read and retry. */
  | "resolve_conflict"
  /** The thing being addressed does not exist. */
  | "missing_resource"
  /** Setup is already finished; there is nothing to do here. */
  | "already_complete"
  /** Transient. Retrying the same request is reasonable. */
  | "retry"
  /** Moira is not ready (no database, starting up). Wait, then retry. */
  | "wait_for_backend"
  /** The console constructed a request Moira says is impossible. A console bug. */
  | "report_bug";

/** A structured error response from Moira. */
export interface MoiraApiError {
  readonly kind: "api";
  readonly status: number;
  readonly code: string;
  readonly remedy: MoiraRemedy;
  readonly retryable: boolean;
  readonly text: ClientSafeText;
}

/** The request never produced an HTTP response (DNS, TLS, connection reset, abort). */
export interface MoiraTransportError {
  readonly kind: "transport";
  readonly remedy: MoiraRemedy;
  readonly retryable: boolean;
  readonly text: ClientSafeText;
}

/** An HTTP error whose body was not a Moira `ErrorResponse` (proxy HTML, empty body). */
export interface MoiraMalformedError {
  readonly kind: "malformed";
  readonly status: number;
  readonly remedy: MoiraRemedy;
  readonly retryable: boolean;
  readonly text: ClientSafeText;
}

export type MoiraError = MoiraApiError | MoiraTransportError | MoiraMalformedError;

/* -------------------------------------------------------------------------- */
/* Console-originated keys used when the server gave us nothing usable        */
/* -------------------------------------------------------------------------- */

// Re-exported, not redefined: `tests/unit/lib/errors.test.ts:184,197` imports
// these two names from THIS module, and `lib/i18n/keys.ts` is the single place a
// `console.*` key is spelled. Both halves matter — see the header of that file.
export const CONSOLE_TRANSPORT_ERROR_KEY = CONSOLE_MESSAGE_KEYS.moira_unreachable;
export const CONSOLE_MALFORMED_ERROR_KEY = CONSOLE_MESSAGE_KEYS.moira_response_unreadable;

/* -------------------------------------------------------------------------- */
/* The (code -> remedy) table                                                 */
/* -------------------------------------------------------------------------- */

/**
 * Code-specific remedies. A code listed here ALWAYS wins over the status-code
 * default below — that is the mechanism by which `setup_claim_credential_required`
 * escapes the generic 401 handling.
 */
export const MOIRA_CODE_REMEDIES: Readonly<Record<string, MoiraRemedy>> = {
  // --- claim path ---------------------------------------------------------
  // 400. The console must never send `setup_token`; if this fires, the console
  // built a body it is forbidden to build.
  setup_token_not_supported: "report_bug",
  // 401, and deliberately NOT `reauthenticate`. Bad or missing system key.
  setup_claim_credential_required: "fix_system_key",
  // 400. The claim body carried no email — a console bug, but the operator's
  // practical route out is to sign in again with an account that has one.
  admin_claim_email_required: "choose_different_identity",
  // 403. Distinct from domain_not_allowed: different cause, different remedy.
  admin_claim_email_not_verified: "choose_different_identity",
  // 403. The expected first-run response; an actionable setup instruction, never
  // a generic failure banner.
  admin_claim_domain_not_allowed: "fix_setup_configuration",
  // 400. The claim named an issuer with no active `trusted_jwt_issuers` row.
  unregistered_trusted_issuer: "fix_setup_configuration",
  // 409. Setup is done. Route to /login, do not offer a retry.
  admin_identity_already_claimed: "already_complete",
  // 422 — NOT 400. `normalize_scopes` runs before the issuer is even resolved.
  // The console omits `scopes` entirely, so this firing means it did not.
  scope_invalid: "report_bug",

  // --- auth-provider path -------------------------------------------------
  // 400. Reachable only once the provider row actually carries
  // `trusted_jwt_issuer_id` (the B1 fix) — before that the check short-circuits.
  console_issuer_must_not_assert_scopes: "fix_setup_configuration",
  // 400. Fires on create AND on enable: the method's required non-secret
  // configuration is missing.
  auth_provider_method_config_incomplete: "fix_form_input",
  // 400. https-only scheme/host check, no allow_http escape hatch on this path.
  auth_provider_url_not_allowed: "fix_form_input",
  duplicate_auth_provider: "fix_form_input",
  auth_provider_not_found: "missing_resource",
  // 409, finding F23. NOT `fix_form_input`: nothing in the submitted body is wrong. Another
  // row is already the enabled provider on this trusted issuer, and the operator has to
  // decide which one governs admission — a configuration decision, not a typo.
  duplicate_enabled_provider_for_issuer: "fix_setup_configuration",
  // 409, F23 shape (b). The `issuer` field names a registered trusted JWT issuer the row is
  // not bound to. That IS a field the operator can correct in the form — either by removing
  // the issuer or by binding the row — so it is form input rather than configuration.
  auth_provider_issuer_shadows_trusted_issuer: "fix_form_input",

  // --- invitation redemption (plan 09 wave 5) ------------------------------
  // 404. The token names no live invite. `missing_resource`, NOT `retry`: the
  // same code is returned for a wrong prefix and for a wrong hash, deliberately,
  // so that retrying is never how you learn anything.
  invite_not_found: "missing_resource",
  // 403. Nothing the holder can do with THIS link; a new invitation is the only
  // route forward, and that is somebody else's action.
  invite_expired: "denied",
  // 409. Someone already redeemed it — possibly this same person on another
  // device. `resolve_conflict` sends the reader to re-read state rather than to
  // retry a request that will keep failing.
  invite_already_consumed: "resolve_conflict",
  invite_revoked: "denied", // 403
  // 403 both. `choose_different_identity`, and NOT `fix_setup_configuration`:
  // the deployment is fine, the invitation was issued for a different address or
  // domain than the one that just signed in. Merging these into
  // `admin_claim_domain_not_allowed`'s remedy would send an invitee to an
  // auth-settings screen they cannot reach and must not change.
  invite_email_mismatch: "choose_different_identity",
  invite_domain_mismatch: "choose_different_identity",

  // --- invitation creation --------------------------------------------------
  // 422, a HARD CAP refused rather than clamped. The field is in the form the
  // operator just submitted, so it is form input.
  admin_invite_expiry_too_long: "fix_form_input",

  // --- grant administration -------------------------------------------------
  admin_identity_not_found: "missing_resource", // 404
  admin_identity_already_revoked: "resolve_conflict", // 409
  // 409, decision D-F20. NOT `resolve_conflict`: re-reading and retrying will
  // fail identically forever, because the row IS the last primary. The remedy is
  // "transfer ownership first", which is a different operation — `denied` is the
  // honest classification, and the ownership screen states the rule in its own
  // copy rather than surfacing this as a failed request.
  admin_identity_last_primary: "denied",
  // 403. The caller's own grant is not primary. THE constructible
  // authorization-denial case — `moira:admins:manage` does not exist and could
  // not be withheld if it did.
  admin_identity_not_primary: "denied",

  // --- jwt-issuer path ----------------------------------------------------
  jwks_url_rejected: "fix_form_input",
  // 409. Live admin grants were made through this issuer; retiring it would revoke every
  // one of them. Revoke the grants first — a configuration sequence, not a bad field.
  trusted_issuer_has_active_grants: "fix_setup_configuration",

  // --- shared -------------------------------------------------------------
  if_match_required: "report_bug",
  resource_version_conflict: "resolve_conflict",
  idempotency_conflict: "resolve_conflict",
  idempotency_in_progress: "resolve_conflict",
  invalid_request: "fix_form_input",
  bad_request: "fix_form_input",
  validation_failed: "fix_form_input",
  unauthorized: "reauthenticate",
  forbidden: "denied",
  not_found: "missing_resource",
  rate_limited: "retry",
  database_unavailable: "wait_for_backend",
  // An unmapped Postgres unique violation surfaces as an opaque 500 with this
  // code — notably what a naive re-`POST` of an existing trusted JWT issuer
  // produces, since `trusted_jwt_issuers_issuer_active_unique` is not mapped to
  // a 409. Reuse-first provisioning is what avoids it; see `setup-flow.ts`.
  database_error: "retry",
  internal_error: "retry",
};

/** Fallback when the code is unknown to this console. */
export function remedyForStatus(status: number): MoiraRemedy {
  if (status === 401) return "reauthenticate";
  if (status === 403) return "denied";
  if (status === 404) return "missing_resource";
  if (status === 409 || status === 412 || status === 428) return "resolve_conflict";
  if (status === 429) return "retry";
  if (status === 503) return "wait_for_backend";
  if (status >= 500) return "retry";
  if (status >= 400) return "fix_form_input";
  return "report_bug";
}

const RETRYABLE_REMEDIES: ReadonlySet<MoiraRemedy> = new Set<MoiraRemedy>([
  "retry",
  "wait_for_backend",
  "resolve_conflict",
]);

/* -------------------------------------------------------------------------- */
/* Parsing                                                                    */
/* -------------------------------------------------------------------------- */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Narrow an arbitrary parsed body to Moira's error envelope.
 *
 * Deliberately permissive on `message_args`/`details` (they are free-form JSON)
 * and strict on the three string fields the console actually renders.
 */
export function isMoiraErrorResponse(body: unknown): body is MoiraErrorResponse {
  if (!isRecord(body)) return false;
  const error = body["error"];
  if (!isRecord(error)) return false;
  return (
    typeof error["code"] === "string" &&
    typeof error["message_key"] === "string" &&
    typeof error["message"] === "string"
  );
}

/**
 * Build the client-safe text. Field by field, never by spread: a field Moira
 * adds to `ErrorDetail` tomorrow does not become a field the browser sees today.
 */
function clientSafeText(error: MoiraErrorResponse["error"]): ClientSafeText {
  return {
    messageKey: error.message_key,
    message: error.message,
    messageArgs: error.message_args ?? null,
  };
}

/**
 * Map an HTTP status plus a parsed body onto the client-safe union.
 *
 * `body` is `unknown` on purpose: it is whatever `response.json()` produced, or
 * `undefined` when the body was unreadable.
 */
export function toMoiraError(status: number, body: unknown): MoiraError {
  if (!isMoiraErrorResponse(body)) {
    const remedy = remedyForStatus(status);
    return {
      kind: "malformed",
      status,
      remedy,
      retryable: RETRYABLE_REMEDIES.has(remedy),
      text: {
        messageKey: CONSOLE_MALFORMED_ERROR_KEY,
        message: `Moira returned HTTP ${status} with an unreadable body.`,
        messageArgs: { status },
      },
    };
  }

  const code = body.error.code;
  const remedy = MOIRA_CODE_REMEDIES[code] ?? remedyForStatus(status);
  return {
    kind: "api",
    status,
    code,
    remedy,
    retryable: RETRYABLE_REMEDIES.has(remedy),
    text: clientSafeText(body.error),
  };
}

/** The no-HTTP-response case. */
export function toTransportError(cause: unknown): MoiraTransportError {
  return {
    kind: "transport",
    remedy: "retry",
    retryable: true,
    text: {
      messageKey: CONSOLE_TRANSPORT_ERROR_KEY,
      // Deliberately not `String(cause)`: a thrown fetch error can carry a URL
      // with credentials in it. The reason is logged server-side instead.
      message: "The console could not reach Moira.",
      messageArgs: { reason: transportReason(cause) },
    },
  };
}

function transportReason(cause: unknown): string {
  if (cause instanceof Error && cause.name === "AbortError") return "aborted";
  if (cause instanceof Error && cause.name === "TimeoutError") return "timeout";
  return "network";
}

/* -------------------------------------------------------------------------- */
/* Server-side-only diagnostics — MOVED OUT OF THIS MODULE                    */
/* -------------------------------------------------------------------------- */
//
// `MoiraServerDiagnostics` and `serverDiagnostics()` now live in
// `lib/errors-server.ts`, which carries `import "server-only"`.
//
// They were here, and that was a hole with only a doc comment holding it shut.
// This module deliberately carries NO `import "server-only"` — `MoiraError` has
// to cross to the browser — so a client component could import
// `serverDiagnostics` from here and Next would compile it happily. And that
// function is the one thing in this file that returns UNFILTERED pass-through
// data: `details: error.details ?? null`, typed `JsonValue`, with no allow-list.
//
// It was dead code in application paths (its only callers were two lines of
// `errors.test.ts`), so nothing noticed. Wave 3 adds the first real caller, and
// a one-line import edit turned the doc comment into a build failure.

/* -------------------------------------------------------------------------- */
/* Predicates the UI uses                                                     */
/* -------------------------------------------------------------------------- */

/**
 * Should this error clear the console session and redirect to /login?
 *
 * ONLY when the remedy is `reauthenticate`. A 401 alone is not sufficient:
 * `setup_claim_credential_required` is a 401 about the system key and must not
 * sign anybody out.
 */
export function isSessionExpired(error: MoiraError): boolean {
  return error.remedy === "reauthenticate";
}

/**
 * Is this the expected, actionable first-run setup condition rather than a
 * failure? These render as guidance with a route back to the auth-settings
 * step, never as a generic error banner.
 */
export function isActionableSetupCondition(error: MoiraError): boolean {
  return error.remedy === "fix_setup_configuration";
}

/** A thrown `MoiraError`, so async call sites can `throw`/`catch` it. */
export class MoiraRequestError extends Error {
  readonly moiraError: MoiraError;

  constructor(moiraError: MoiraError) {
    super(moiraError.text.message);
    this.name = "MoiraRequestError";
    this.moiraError = moiraError;
  }
}

/** Type guard for `catch` blocks. */
export function isMoiraRequestError(value: unknown): value is MoiraRequestError {
  return value instanceof MoiraRequestError;
}
