// @server-only
//
// The console's own mutation transport: route handlers under `app/api/**`.
//
// ============================================================================
// WHY ROUTE HANDLERS AND NOT SERVER ACTIONS (plan 09 decision W5-D5)
// ============================================================================
//
// Every mutation this wave adds — create invitation, revoke invitation, transfer
// ownership, revoke a grant, redeem an invitation — is triggered from a
// `"use client"` organism, and `layer-dependencies.test.ts` rule 5 forbids a
// client module from importing any credential-carrying module. `lib/moira-client.ts`
// is in that set by name. So the mutation needs a server-side transport, and
// there were two candidates:
//
//   SERVER ACTIONS — the plan's assumption. There is not one `actions.ts` in
//   this repository, `nextCookies()` is deliberately absent from the Better Auth
//   plugin list (with it installed, the sign-in reply came back with no
//   `Set-Cookie` at all and the callback then failed `state_security_mismatch`),
//   and there is no module-scope `auth` object to import. Adopting them means
//   changing the auth instance, in a wave that should not be touching it.
//
//   ROUTE HANDLERS — the shipped precedent. `SignInPanel` already posts to
//   `POST /api/auth/sign-in/oauth2` with `fetch`, and its header says why.
//
// ============================================================================
// `app/api/**` IS OUTSIDE THE `(console)` SESSION GATE — BY CONSTRUCTION
// ============================================================================
//
// Route groups contribute no URL segment and no layout to `app/api/**`: the
// `(console)` group's `layout.tsx` wraps the pages inside that group and nothing
// else. So a route handler inherits NO session check, and every one of them must
// perform its own.
//
// That is explicit rather than inherited, which is the right direction — but it
// is also exactly the kind of rule that holds until the day somebody adds the
// twelfth handler. `tests/unit/architecture/route-handler-session.test.ts` scans
// `app/api/**` and fails on any handler that neither calls `withConsoleSession`
// nor appears on a named, justified exemption list.
//
// ============================================================================
// WHAT THE CHECK IS AND IS NOT
// ============================================================================
//
// `consoleSessionCheck` runs the same `checkSession` that `jwt.getSubject` runs
// before minting, from the same resolved configuration. **It is not the security
// control.** Moira is: `evaluate_claim_policy` and `admission_policy` apply the
// deployment's `allowed_email_domains` server-side on every claim and every
// redemption, and `require_primary_actor` decides ownership from row state that
// the console cannot see. Deleting this check loses the *explanation* an operator
// gets — a named refusal instead of a bare 403 several screens later — and the
// defence in depth. It does not open anything Moira was relying on it to close.
//
// Finding F25 is the reason that paragraph is written down: `checkSession`
// shipped with eleven green unit assertions and no caller at all, and a reader
// arriving here would reasonably assume the session boundary was already wired.

import "server-only";

import { consoleSessionCheck } from "./auth";
import { consoleRuntime, type ConsoleRuntime } from "./auth-runtime";
import { consoleEnv, type ConsoleEnv } from "./env";
import { isMoiraRequestError, type MoiraError } from "./errors";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import type { MoiraClient } from "./moira-client";
import type { AdminIdentityRecord } from "./types";
import { moiraClientForSession, type ConsoleSessionIdentity } from "./moira-session";

/** What a guarded handler is handed. Never the raw request headers. */
export interface ConsoleApiContext {
  /** The verified session, as `checkSession` resolved it. */
  readonly identity: ConsoleSessionIdentity;
  /**
   * A Moira client authenticating AS THE SIGNED-IN OPERATOR.
   *
   * Built by `moiraClientForSession`, which deliberately passes no `systemKey`:
   * `MoiraClient` prefers the system key over the bearer token when both are
   * present, so including it would authenticate every admin call as the
   * bootstrap credential instead of as the human — defeating the audit trail,
   * and, on the redemption path, throwing outright (`bearer_only`).
   */
  readonly client: MoiraClient;
  readonly env: ConsoleEnv;
  /**
   * The `admin_identities` NAMESPACE this session belongs to —
   * `SessionCheck.consoleIssuer`, resolved from the configuration that actually
   * resolved the cookie, never a caller's string.
   *
   * Forwarded for issue #185's ownership gate: `admin_identities` is keyed by
   * `(issuer, subject)`, so "is this caller the owner" is unanswerable without
   * it. `consoleSessionCheck` has always resolved it; nothing downstream could
   * see it.
   */
  readonly consoleIssuer: string;
  /**
   * The `auth_provider_settings` ROW this session was established through.
   *
   * `consoleIssuer` answers "which namespace"; this answers "which provider",
   * and the two are not interchangeable when the question is whether a caller
   * may REWRITE that row — which is exactly what `/settings/auth` asks.
   */
  readonly moiraProviderId: string;
}

export type ConsoleApiHandler = (context: ConsoleApiContext) => Promise<Response>;

/* -------------------------------------------------------------------------- */
/* Test seam                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * The three process-wide things a route handler cannot be tested without.
 *
 * ============================================================================
 * WHAT THIS DELIBERATELY DOES **NOT** SUBSTITUTE
 * ============================================================================
 *
 * `consoleSessionCheck`. That is the gate, and a seam that replaced it would
 * turn every "the handler refuses without a session" test into an assertion
 * about the stub — green whether or not the handler calls `withConsoleSession`
 * at all, which is finding F25's exact shape.
 *
 * So a test supplies a real `ConsoleAuth` (the same `createConsoleAuth` the
 * process builds) plus real resolved configs, and the check runs for real
 * against the request's own headers. A request with no session cookie is a 401
 * because the shipped code decided so; a request carrying one minted by the
 * shipped sign-in flow is admitted for the same reason.
 *
 * The seam exists because the other three are unreachable in a unit test:
 * `consoleRuntime()` calls Moira to resolve the configuration, `consoleEnv()`
 * reads `process.env` and refuses to boot without a database in production, and
 * `moiraClientForSession` mints a JWT against a live signing key.
 *
 * Modelled on `setSetupWindowDependenciesForTests` — same shape, same
 * `null`-restores-production contract.
 */
export interface ConsoleApiDependencies {
  readonly runtime: () => Promise<ConsoleRuntime>;
  readonly env: () => ConsoleEnv;
  readonly clientFor: (
    env: ConsoleEnv,
    runtime: Extract<ConsoleRuntime, { ok: true }>,
    headers: Headers,
  ) => MoiraClient;
}

let dependencies: ConsoleApiDependencies | null = null;

/** Install substitutes, or pass `null` to restore the shipped wiring. */
export function setConsoleApiDependenciesForTests(overrides: ConsoleApiDependencies | null): void {
  dependencies = overrides;
}

const NO_STORE = { "cache-control": "no-store" } as const;

/** A keyed JSON body. Never English prose, never a Moira `request_id`. */
function keyed(status: number, code: string, messageKey: string): Response {
  return Response.json({ error: { code, message_key: messageKey } }, { status, headers: NO_STORE });
}

/**
 * A client-safe rendering of a Moira failure.
 *
 * `MoiraError` is already the narrowed, client-safe union — `lib/errors.ts` is
 * the only module that reads `request_id` and `details`, and it does not put
 * either into this shape. Serialising it whole is therefore safe AND is the
 * reason the organisms can render a remedy: the mapping from `(status, code)` to
 * `remedy` happens once, on the server, rather than being re-derived in three
 * components.
 */
export function moiraErrorBody(error: MoiraError): { readonly error: MoiraError } {
  return { error };
}

/** Status for a `MoiraError`, defaulting to 502 for transport-level failures. */
function statusFor(error: MoiraError): number {
  return error.kind === "api" ? error.status : 502;
}

/**
 * Run `handler` only for a session that exists and may act.
 *
 * The three refusal shapes, all keyed and all distinguishable by a client:
 *
 *   503  the deployment has no resolvable auth configuration, or Moira is
 *        unreachable. Not a session problem, and specifically not a 401 — the
 *        caller must not be signed out because the backend is down.
 *   401  no session.
 *   403  a session that may not act: unverified email, a domain outside the
 *        allow-list, no IdP subject, or a provider this configuration does not
 *        contain.
 */
export async function withConsoleSession(
  request: Request,
  handler: ConsoleApiHandler,
): Promise<Response> {
  const wiring = dependencies;
  let runtimeState: ConsoleRuntime;
  try {
    runtimeState = await (wiring === null ? consoleRuntime() : wiring.runtime());
  } catch (error) {
    // A Moira outage lands here, because resolving the configuration means
    // calling Moira. Without this catch it escapes as an unhandled rejection and
    // Next renders a 500 with a stack, which reads as a console bug.
    if (isMoiraRequestError(error)) {
      return keyed(503, "moira_unreachable", error.moiraError.text.messageKey);
    }
    throw error;
  }

  if (!runtimeState.ok) {
    return keyed(503, runtimeState.resolution.problem, runtimeState.resolution.messageKey);
  }

  // THE SESSION CHECK ITSELF IS NEVER SUBSTITUTED. `setConsoleApiDependenciesForTests`
  // replaces the environment, the runtime and the Moira transport — the three
  // things a test cannot supply — and deliberately not this line, so a test that
  // exercises a handler exercises the REAL gate. See that function's header.
  const check = await consoleSessionCheck(runtimeState.auth, runtimeState.configs, request.headers);
  if (!check.ok) {
    const status = check.rejection === "no_session" ? 401 : 403;
    return keyed(status, check.rejection, check.messageKey);
  }

  const env = wiring === null ? consoleEnv() : wiring.env();
  const client =
    wiring === null
      ? moiraClientForSession(env, runtimeState.auth, request.headers)
      : wiring.clientFor(env, runtimeState, request.headers);

  try {
    return await handler({
      identity: check.identity,
      client,
      env,
      consoleIssuer: check.consoleIssuer,
      moiraProviderId: check.moiraProviderId,
    });
  } catch (error) {
    if (isMoiraRequestError(error)) {
      return Response.json(moiraErrorBody(error.moiraError), {
        status: statusFor(error.moiraError),
        headers: NO_STORE,
      });
    }
    throw error;
  }
}

/**
 * Read a JSON body, or `null` when it is absent or unparseable.
 *
 * Returning `null` rather than throwing keeps a malformed body a 400 the handler
 * writes with its own key, instead of a 500 with a stack.
 */
export async function readJsonBody(request: Request): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await request.json();
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    return parsed as Record<string, unknown>;
  } catch {
    return null;
  }
}

/** A 400 for a body the console itself rejected, keyed like every other refusal. */
export function badRequest(messageKey: string): Response {
  return keyed(400, "invalid_request", messageKey);
}

/**
 * A 404 the CONSOLE decided, for a nested resource that does not belong to the
 * parent named in the path.
 *
 * Distinct from Moira's own 404 on purpose. "This model id exists, but not under
 * this provider" is a refusal only the console can make — Moira's flat
 * `POST /provider-models/{id}/disable` would happily act on it — and it is the
 * check that stops a request body choosing which row a privileged call touches.
 */
export function notFound(messageKey: string): Response {
  return keyed(404, "not_found", messageKey);
}

/* -------------------------------------------------------------------------- */
/* "NOT ON PAGE ONE" IS NOT "DOES NOT EXIST" (issue #117)                      */
/* -------------------------------------------------------------------------- */

/** The shape of any Moira list response, structurally — no import edge needed. */
interface PagedResponse<T> {
  readonly data: readonly T[];
  readonly pagination: { readonly has_more: boolean };
}

/**
 * The outcome of looking one row up in a single listed page.
 *
 * Three cases, because two of them are not the same answer. `absent` means the
 * whole list was seen and the row is not in it; `truncated` means the page ran
 * out first and NOTHING IS KNOWN about the row.
 */
export type PageLookup<T> =
  | { readonly kind: "found"; readonly row: T }
  | { readonly kind: "absent" }
  | { readonly kind: "truncated" };

/**
 * The OWNER gate (issue #185). Ownership is row state, not a scope.
 *
 * ============================================================================
 * WHY THIS EXISTS WHEN MOIRA ALREADY REFUSES
 * ============================================================================
 *
 * Moira gates the auth-provider WRITE surface on `require_primary_actor`, so a
 * non-owner's request is refused at the source whatever this console believes.
 * That refusal is the enforcement; this function is not.
 *
 * What this buys is the thing a 403 arriving mid-form cannot: a screen that does
 * not offer a control the caller may not use. The alternative — render the form
 * to every admin and let Moira answer — teaches operators that the console shows
 * them buttons that fail, which is how people learn to retry destructive things.
 *
 * It is therefore a PRE-CHECK and is described as one. It must never become the
 * only check: a console-side gate is bypassable by anyone who can call Moira
 * directly with their own bearer token, which every admin can.
 *
 * ============================================================================
 * `truncated` IS ITS OWN ANSWER, NOT "not the owner"
 * ============================================================================
 *
 * Issue #117's lesson, applied to the one lookup where getting it wrong is worst:
 * an owner whose grant sits on page two would be told they are not the owner of
 * a deployment they own, on the screen that exists to let them fix their sign-in.
 * `lookupOnPage` distinguishes the three cases and each gets its own keyed
 * refusal.
 */
export async function requireConsoleOwner(
  context: ConsoleApiContext,
): Promise<{ readonly ok: true } | { readonly ok: false; readonly response: Response }> {
  const lookup = await lookUpOwnGrant(
    context.client,
    context.consoleIssuer,
    context.identity.idpSubject,
  );

  if (lookup.kind === "truncated") {
    return {
      ok: false,
      response: keyed(
        409,
        "admin_identity_lookup_truncated",
        CONSOLE_MESSAGE_KEYS.authsettings_owner_lookup_truncated,
      ),
    };
  }
  if (lookup.kind === "absent") {
    // A signed-in operator with no grant in this namespace. Reachable: the
    // session resolved, so the domain allow-list admitted them, but nobody has
    // granted them admin here.
    return {
      ok: false,
      response: keyed(
        403,
        "admin_identity_not_primary",
        CONSOLE_MESSAGE_KEYS.authsettings_owner_grant_absent,
      ),
    };
  }
  if (!grantIsOwner(lookup)) {
    return {
      ok: false,
      response: keyed(
        403,
        "admin_identity_not_primary",
        CONSOLE_MESSAGE_KEYS.authsettings_not_owner,
      ),
    };
  }
  return { ok: true };
}

/**
 * One page is enough to find one grant on any deployment a flat admin list
 * serves, and `truncated` is answered rather than guessed when it is not.
 */
const OWNER_LOOKUP_PAGE_LIMIT = 200;

/**
 * The caller's OWN `admin_identities` grant, if this page holds it.
 *
 * Exported because two surfaces ask the same question and must not answer it
 * twice: the route handler (which owes a keyed refusal) and the PAGE (which owes
 * a rendered explanation and cannot use a `Response`). A second lookup written
 * for the page is a second predicate to keep in step with
 * `admin_identities.is_primary`, and the first time they disagreed one of the
 * two would be showing a form it should not.
 *
 * Matched on BOTH halves of the key. A subject is unique only within its issuer,
 * and this console can hold several namespaces (wave 4B slugs), so a
 * subject-only match could find a grant from a different namespace entirely.
 */
export async function lookUpOwnGrant(
  client: MoiraClient,
  consoleIssuer: string,
  idpSubject: string,
): Promise<PageLookup<AdminIdentityRecord>> {
  const page = await client.listAdminIdentities({ limit: OWNER_LOOKUP_PAGE_LIMIT });
  return lookupOnPage(page, (row) => row.issuer === consoleIssuer && row.subject === idpSubject);
}

/** Whether that grant is one that may rewrite the sign-in configuration. */
export function grantIsOwner(lookup: PageLookup<AdminIdentityRecord>): boolean {
  // `status` as well as `is_primary`: a revoked row keeps its flag, and a
  // revoked owner is not an owner.
  return lookup.kind === "found" && lookup.row.status === "active" && lookup.row.is_primary;
}

/**
 * Find one row on a single page, distinguishing a truncated page from an absent
 * row.
 *
 * ============================================================================
 * WHY THE THIRD CASE EXISTS (issue #117)
 * ============================================================================
 *
 * The ownership lookups behind enable/disable each listed with `LIST_PAGE_LIMIT`
 * and took `.find(...) ?? null`, so a row on page two was reported as
 * `not found` — a 404 saying "this model does not belong to this provider" about
 * a model that does. Fail-closed, and the operator is told something untrue
 * about their own deployment, on the screen whose entire job is to explain it.
 *
 * `lib/llm-settings.ts`'s `findOnPage` already draws this distinction for the
 * connect chain, and throws to carry the partial progress a chain has made. A
 * route handler has no progress to carry, so this returns the distinction
 * instead and each handler chooses its own keyed refusal.
 *
 * The right answer to `truncated` is never "act anyway" and never "say it does
 * not exist" — it is `llm_list_truncated`, which tells the operator the list
 * outgrew one page.
 */
export function lookupOnPage<T>(page: PagedResponse<T>, match: (row: T) => boolean): PageLookup<T> {
  const hit = page.data.find(match);
  if (hit !== undefined) return { kind: "found", row: hit };
  return page.pagination.has_more ? { kind: "truncated" } : { kind: "absent" };
}
