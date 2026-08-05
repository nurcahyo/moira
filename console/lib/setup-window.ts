// @server-only
//
// The setup window: the interval in which `app/api/setup/route.ts` may act.
//
// ============================================================================
// WHY THIS IS NOT `withConsoleSession`, AND WHY THAT IS NOT A RELAXATION
// ============================================================================
//
// Every other handler under `app/api/**` re-checks the console session, and
// `tests/unit/architecture/route-handler-session.test.ts` fails on one that does
// not. Setup cannot: it runs BEFORE the first admin exists. There is no
// `admin_identities` grant to check against, no operator to be signed in as, and
// `POST /api/v1/admin/setup/claim` is the request that CREATES the first one.
// Requiring a session there would be requiring the outcome as the precondition.
//
// So the gate is a different one, and it has to be at least as narrow, because
// what it stands in front of is the bootstrap system key:
//
//   1. `env.moiraSystemKey` must be present. Without it there is no credential
//      the wizard could write with, and an operator who has finished setup is
//      EXPECTED to remove it (`lib/auth-runtime.ts`'s header states why). A
//      deployment past that point answers 404 here — the surface is gone, not
//      merely refused.
//   2. `GET /api/v1/admin/setup/claim-status` must say `claimed: false`. That
//      call is the one anonymous operation on Moira's admin surface, it is
//      Moira's own answer rather than the console's belief, and it is re-read on
//      EVERY request rather than cached — a cached "still unclaimed" is exactly
//      the window this guard exists to close.
//
// Both are checked before the handler runs and before any body is interpreted,
// so a malformed payload cannot be the thing that gets a request past them.
//
// ============================================================================
// THE ORDER OF THE TWO CHECKS IS LOAD-BEARING
// ============================================================================
//
// The system-key check runs FIRST and does not touch the network. A console with
// no bootstrap credential cannot build a client for the claim-status call at all
// (`moiraClientForSetup` throws), so checking claim-status first would turn the
// post-setup steady state into a 500 with a stack instead of a 404.
//
// ============================================================================
// WHAT THIS MODULE DELIBERATELY DOES NOT DO
// ============================================================================
//
// It does not read, hold, or name the OAuth client secret. The secret reaches
// `lib/console-secrets.ts` through a closure the ROUTE builds and hands to
// `runSetupProvisioning`, which is what keeps it out of every Moira request body
// by construction rather than by review. This module holds the SYSTEM KEY path —
// `moiraClientForSetup` — which is why it carries the server-only marker.

import "server-only";

import { consoleSessionCheck } from "./auth";
import {
  consoleRuntime,
  consoleSecretStore,
  consoleStorageMode,
  type ConsoleStorageMode,
} from "./auth-runtime";
import type { ConsoleSecretStore } from "./console-secrets";
import { consoleEnv, type ConsoleEnv } from "./env";
import { isMoiraRequestError, type MoiraError } from "./errors";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import type { MoiraClient } from "./moira-client";
import { moiraClientForSetup, rejectedSession, type SessionCheck } from "./moira-session";
import { EMPTY_PROVISIONING_STATE, type SetupProvisioningState } from "./setup-flow";

/* -------------------------------------------------------------------------- */
/* Responses                                                                  */
/* -------------------------------------------------------------------------- */

const NO_STORE = { "cache-control": "no-store" } as const;

/**
 * A keyed JSON refusal. Never English prose, never a Moira `request_id`.
 *
 * The same shape `lib/console-api.ts` emits, deliberately: a wizard rendering a
 * refusal from this route and a screen rendering one from a session-guarded
 * route must not need two branches to read the same envelope.
 */
export function setupError(
  status: number,
  code: string,
  messageKey: string,
  messageArgs: Record<string, string | number> | null = null,
): Response {
  return Response.json(
    { error: { code, message_key: messageKey, message_args: messageArgs } },
    { status, headers: NO_STORE },
  );
}

/** A 400 the console itself decided, before any request left the process. */
export function setupBadRequest(
  messageKey: string,
  messageArgs: Record<string, string | number> | null = null,
): Response {
  return setupError(400, "invalid_request", messageKey, messageArgs);
}

/**
 * A Moira failure, rendered from the already-narrowed client-safe union.
 *
 * `MoiraError` is what `lib/errors.ts` produces field by field; it carries no
 * `request_id` and no server diagnostics, so serialising it whole is safe and is
 * what lets the wizard branch on `remedy` rather than on a status code.
 */
export function setupMoiraFailure(error: MoiraError): Response {
  return Response.json(
    { error },
    { status: error.kind === "api" ? error.status : 502, headers: NO_STORE },
  );
}

/** A 2xx body. Always `no-store`: none of this is cacheable, and some of it is identity configuration. */
export function setupJson(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: NO_STORE });
}

/* -------------------------------------------------------------------------- */
/* The context a guarded handler receives                                     */
/* -------------------------------------------------------------------------- */

export interface SetupWindowContext {
  readonly env: ConsoleEnv;
  /**
   * Moira's own answer on this request, not the console's belief.
   *
   * Always `false` here — the guard refuses otherwise — and carried anyway so a
   * view model reports what was READ rather than what the guard implies.
   */
  readonly claimed: boolean;
  /**
   * A Moira client carrying the BOOTSTRAP SYSTEM KEY.
   *
   * The only client that can run `POST /api/v1/admin/setup/claim`, which
   * declares `systemKeyAuth` and nothing else — a bearer token is refused there
   * even if it verifies.
   */
  readonly client: MoiraClient;
  /** The console's own OAuth client-secret store (D7). Never Moira's. */
  readonly store: ConsoleSecretStore;
  /** Whether the console's secrets survive a restart. Display-safe. */
  readonly storageMode: ConsoleStorageMode;
  /**
   * The Better Auth session on this request, as `checkSession` resolved it.
   *
   * A function rather than a value because only the claim step needs it, and
   * resolving it means asking Moira for the auth configuration — work the
   * provisioning step must not pay for and, before the first provider exists,
   * work that cannot succeed.
   */
  readSession(headers: Headers): Promise<SessionCheck>;
}

export type SetupWindowHandler = (context: SetupWindowContext) => Promise<Response>;

/* -------------------------------------------------------------------------- */
/* The test seam                                                              */
/* -------------------------------------------------------------------------- */

/**
 * Substitutions for the process-wide wiring, so the route can be driven without
 * an environment, a database, or a network.
 *
 * The same device `resetConsoleRuntime` already provides for the secret store,
 * and for the same reason: a handler whose dependencies cannot be substituted is
 * a handler whose tests have to re-implement it, and a re-implementation is what
 * finding F25 was.
 */
export interface SetupWindowDependencies {
  readonly env?: ConsoleEnv;
  readonly client?: MoiraClient;
  readonly store?: ConsoleSecretStore;
  readonly storageMode?: ConsoleStorageMode;
  readonly readSession?: (headers: Headers) => Promise<SessionCheck>;
}

let injected: SetupWindowDependencies | null = null;

/** Test seam. Never called from a shipped code path. */
export function setSetupWindowDependenciesForTests(
  dependencies: SetupWindowDependencies | null,
): void {
  injected = dependencies;
}

/* -------------------------------------------------------------------------- */
/* Session resolution                                                         */
/* -------------------------------------------------------------------------- */

/**
 * The shipped session read.
 *
 * `consoleRuntime` is what resolves the provider configuration the session was
 * established against; before the wizard has provisioned one there is nothing to
 * resolve, and the honest answer is `no_session` rather than a 503 — nobody can
 * be signed in to a console with no enabled provider.
 */
async function readConsoleSession(env: ConsoleEnv, headers: Headers): Promise<SessionCheck> {
  const runtime = await consoleRuntime(env);
  if (!runtime.ok) return rejectedSession("no_session");
  return consoleSessionCheck(runtime.auth, runtime.configs, headers);
}

/* -------------------------------------------------------------------------- */
/* The guard                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * Run `handler` only while this deployment is still in its setup window.
 *
 * The two refusals, both keyed and distinguishable by a client:
 *
 *   404  no bootstrap system key. The setup surface does not exist on this
 *        deployment — either it never did, or the operator removed the key after
 *        finishing, which is what they are told to do. NOT a 401: there is no
 *        credential the caller could supply to change the answer.
 *   409  Moira reports the deployment already claimed. Setup is over; the caller
 *        belongs at /login, and a retry will never succeed.
 *
 * Takes no `Request`, deliberately, where `withConsoleSession` takes one: this
 * gate reads nothing off the request, and the only thing that does — the session
 * on the claim step — receives the headers explicitly through `readSession`. A
 * request parameter here would be a request this guard is not entitled to read.
 */
export async function withSetupWindow(handler: SetupWindowHandler): Promise<Response> {
  const overrides = injected ?? {};
  const env = overrides.env ?? consoleEnv();

  if (env.moiraSystemKey === undefined || env.moiraSystemKey === "") {
    return setupError(404, "setup_unavailable", CONSOLE_MESSAGE_KEYS.setup_system_key_absent);
  }

  const client = overrides.client ?? moiraClientForSetup(env);

  try {
    const status = await client.getSetupClaimStatus();
    if (status.claimed) {
      return setupError(409, "setup_already_claimed", CONSOLE_MESSAGE_KEYS.setup_already_claimed);
    }

    const store = overrides.store ?? consoleSecretStore(env);
    const storageMode = overrides.storageMode ?? consoleStorageMode(env);
    const readSession =
      overrides.readSession ?? ((headers: Headers) => readConsoleSession(env, headers));

    return await handler({ env, claimed: status.claimed, client, store, storageMode, readSession });
  } catch (error) {
    // A Moira failure anywhere under the window — the claim-status read included —
    // comes back as the client-safe union rather than as an unhandled rejection
    // Next would render as a 500 with a stack.
    if (isMoiraRequestError(error)) return setupMoiraFailure(error.moiraError);
    throw error;
  }
}

/* -------------------------------------------------------------------------- */
/* Idempotency keys for one wizard submission                                 */
/* -------------------------------------------------------------------------- */

/**
 * The two `Idempotency-Key` values for one submission, derived rather than drawn.
 *
 * Derived from a caller-supplied submission id so that a RESUME replays the same
 * keys without the console having to hand them back and take them again. Two
 * distinct keys from one id because Moira scopes idempotency per operation and
 * reusing one string across two operations is a conflict waiting for the first
 * retry.
 *
 * The value is a key, not a secret: the input is a UUID the wizard generated for
 * this form submission and nothing else.
 */
export async function setupIdempotencyKeys(submissionId: string): Promise<{
  readonly trustedJwtIssuer: string;
  readonly authProvider: string;
}> {
  return {
    trustedJwtIssuer: await derivedKey("trusted-jwt-issuer", submissionId),
    authProvider: await derivedKey("auth-provider", submissionId),
  };
}

/** SHA-256 folded into a v5-shaped UUID, the same construction `claimIdempotencyKey` uses. */
async function derivedKey(scope: string, submissionId: string): Promise<string> {
  const encoded = new TextEncoder().encode(`moira-console/setup/${scope}/${submissionId}`);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", encoded));
  const bytes = digest.slice(0, 16);
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x50;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return [
    hex.slice(0, 8),
    hex.slice(8, 12),
    hex.slice(12, 16),
    hex.slice(16, 20),
    hex.slice(20, 32),
  ].join("-");
}

/* -------------------------------------------------------------------------- */
/* Resume state                                                               */
/* -------------------------------------------------------------------------- */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nullableString(value: unknown): string | null | undefined {
  if (value === null) return null;
  if (typeof value === "string") return value;
  return undefined;
}

function nullableNumber(value: unknown): number | null | undefined {
  if (value === null) return null;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return undefined;
}

/**
 * Narrow a caller-supplied resume payload back to `SetupProvisioningState`.
 *
 * `null` is a refusal, not a fallback to "start over". Restarting is what
 * produces the orphan-issuer uniqueness collision `ensureTrustedJwtIssuer`'s
 * reuse-first lookup exists to avoid, so a resume the console cannot read is a
 * 400 the operator sees rather than a silent second attempt from scratch.
 */
export function parseProvisioningState(value: unknown): SetupProvisioningState | null {
  if (!isRecord(value)) return null;

  const trustedJwtIssuerId = nullableString(value["trustedJwtIssuerId"]);
  const trustedJwtIssuerVersion = nullableNumber(value["trustedJwtIssuerVersion"]);
  const providerId = nullableString(value["providerId"]);
  const providerVersion = nullableNumber(value["providerVersion"]);
  const providerTrustedJwtIssuerId = nullableString(value["providerTrustedJwtIssuerId"]);
  const providerEnabled = value["providerEnabled"];
  const allowedEmailDomainCount = value["allowedEmailDomainCount"];
  const consoleSecretStored = value["consoleSecretStored"];

  if (
    trustedJwtIssuerId === undefined ||
    trustedJwtIssuerVersion === undefined ||
    providerId === undefined ||
    providerVersion === undefined ||
    providerTrustedJwtIssuerId === undefined ||
    typeof providerEnabled !== "boolean" ||
    typeof consoleSecretStored !== "boolean" ||
    typeof allowedEmailDomainCount !== "number" ||
    !Number.isFinite(allowedEmailDomainCount) ||
    allowedEmailDomainCount < 0
  ) {
    return null;
  }

  return {
    ...EMPTY_PROVISIONING_STATE,
    trustedJwtIssuerId,
    trustedJwtIssuerVersion,
    providerId,
    providerVersion,
    providerTrustedJwtIssuerId,
    providerEnabled,
    allowedEmailDomainCount,
    consoleSecretStored,
  };
}
