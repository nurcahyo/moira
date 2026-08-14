// @server-only
//
// `/settings/auth` — changing how operators sign in, after setup has closed.
//
// ============================================================================
// WHY THIS EXISTS: THE WINDOW CLOSES AND TAKES THE ONLY FORM WITH IT
// ============================================================================
//
// The OAuth client secret lives in THIS console, not in Moira (decision D7), and
// until issue #185 the only production caller of `ConsoleSecretStore.put()` was
// `app/api/setup/route.ts` — behind `withSetupWindow`, which answers `409
// setup_already_claimed` forever once a deployment has its first admin.
//
// So on a claimed deployment a mistyped or rotated client secret could not be
// corrected by anything: not by this console (the window is shut), and not
// through Moira's admin API (the value is not there). `docs/console-architecture.md`
// documents the recovery as "reload /setup and correct it in the auth-settings
// form", and that instruction stopped being followable the moment the wizard
// finished. This module is the form it names, reachable afterwards.
//
// ============================================================================
// THE ROW IS THE ONE THE CALLER IS STANDING ON
// ============================================================================
//
// Never a row id off a request body, and — unlike the setup path — not derived
// by scanning either. `SessionCheck.moiraProviderId` is the
// `auth_provider_settings` row the caller's own cookie was resolved through, so
// editing it has a property nothing else here can offer: the operator can verify
// the save by signing in again, and they cannot aim this screen at a provider
// they are not currently authenticated by.
//
// That is the same reasoning `assertEnabledProviderMayBeReSaved` encodes for the
// wizard (`lib/setup-flow.ts`), reached by construction rather than by assertion.
//
// ============================================================================
// WHAT THIS MODULE REFUSES TO DO
// ============================================================================
//
// Enable, disable, delete, or create. Each is a documented way to brick the
// deployment — a disable of the only enabled row leaves no sign-in at all, and a
// second enabled row trips `ambiguityGuard` into refusing to resolve EITHER
// provider — and none of them is what "correct the settings" means. They stay
// where they already are: the bootstrap system key against Moira's own API, with
// the sequence written down in `docs/console-architecture.md`.

import "server-only";

import type { AuthSettingsView, AuthProviderSummary } from "./auth-settings-view";
import {
  CONSOLE_SECRET_DRIFT_MESSAGE_KEYS,
  classifySecretDrift,
  type ConsoleSecretStore,
} from "./console-secrets";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import { ifMatchFor, type MoiraClient } from "./moira-client";
import type { AuthProviderSettingsRecord } from "./types";

/** Moira's row, narrowed to what the browser is allowed to render. */
function summarize(record: AuthProviderSettingsRecord): AuthProviderSummary {
  return {
    id: record.id,
    displayName: record.display_name,
    method: record.method,
    clientId: record.client_id ?? null,
    discoveryUrl: record.discovery_url ?? null,
    issuer: record.issuer ?? null,
    authorizationUrl: record.authorization_url ?? null,
    tokenUrl: record.token_url ?? null,
    allowedEmailDomains: [...record.allowed_email_domains],
    enabled: record.enabled,
    version: record.version,
  };
}

/**
 * Everything `/settings/auth` renders.
 *
 * The drift key is computed HERE rather than in the component, because
 * `classifySecretDrift` needs the sealed envelope and the envelope must not
 * cross to the browser.
 */
export async function loadAuthSettings(
  client: MoiraClient,
  store: ConsoleSecretStore,
  providerId: string,
  isOwner: boolean,
): Promise<AuthSettingsView> {
  const record = await client.getAuthProvider(providerId);
  const sealed = await store.read(record.id);
  const drift = classifySecretDrift(record.client_id ?? null, sealed);

  return {
    provider: summarize(record),
    sealedAgainstClientId: sealed?.clientId ?? null,
    sealedAt: sealed?.updatedAt ?? null,
    hasSealedEnvelope: sealed !== null,
    driftKey: drift === "in_sync" ? null : CONSOLE_SECRET_DRIFT_MESSAGE_KEYS[drift],
    isOwner,
  };
}

/** What the update form may change. Every field optional; absent means "leave it". */
export interface AuthProviderUpdate {
  readonly displayName?: string;
  readonly clientId?: string;
  readonly discoveryUrl?: string;
  readonly issuer?: string;
  readonly authorizationUrl?: string;
  readonly tokenUrl?: string;
  readonly allowedEmailDomains?: readonly string[];
}

export type AuthSettingsWriteFailure =
  /**
   * A secret is needed and none came with the submission. Refused before any
   * write.
   *
   * TWO REASONS, AND THEY NEED DIFFERENT COPY. The client id is MOVING and the
   * seal would be left bound to the old one; or there is no seal at all, so the
   * deployment already cannot complete a code exchange and a save storing a
   * client id without a secret would leave it exactly as broken. Telling the
   * second operator "changing the client ID needs its secret" would be a
   * sentence about a change they did not make.
   */
  | { readonly kind: "secret_required"; readonly reason: "client_id_moving" | "no_secret_stored" }
  /** Moira accepted the row but the console's seal no longer matches it. */
  | { readonly kind: "drift_after_write"; readonly messageKey: string };

export type AuthSettingsWriteResult =
  { readonly ok: true } | { readonly ok: false; readonly failure: AuthSettingsWriteFailure };

/**
 * A `PATCH` body carrying only what the operator actually supplied.
 *
 * TWO PROPERTIES, BOTH LOAD-BEARING:
 *
 *   * Moira's patch columns are written `coalesce($n, column)`, so an absent key
 *     means "leave it" and there is NO way to clear a field through this API.
 *     Sending `""` would not clear it either — it would store an empty string,
 *     which is worse than both.
 *   * `AuthProviderSettingsPatchRequest` is `deny_unknown_fields`. Echoing a
 *     loaded record back would 422 on `method`, `id`, `version` and the rest, so
 *     the body is BUILT rather than round-tripped.
 */
function patchBody(update: AuthProviderUpdate): Record<string, unknown> {
  const body: Record<string, unknown> = {};
  const put = (key: string, value: string | undefined): void => {
    const trimmed = value?.trim();
    if (trimmed !== undefined && trimmed !== "") body[key] = trimmed;
  };
  put("display_name", update.displayName);
  put("client_id", update.clientId);
  put("discovery_url", update.discoveryUrl);
  put("issuer", update.issuer);
  put("authorization_url", update.authorizationUrl);
  put("token_url", update.tokenUrl);
  if (update.allowedEmailDomains !== undefined && update.allowedEmailDomains.length > 0) {
    body["allowed_email_domains"] = [...update.allowedEmailDomains];
  }
  return body;
}

/**
 * Update the provider row and, when a secret was supplied, this console's seal.
 *
 * ============================================================================
 * THE ORDER, AND WHY IT IS THIS ORDER
 * ============================================================================
 *
 * 1. REFUSE FIRST. A client id that differs from the sealed one, with no secret
 *    in the same submission, is refused before a single request leaves this
 *    process. That combination is the most likely lockout on this screen: Moira
 *    would store the new id, the console's seal would still be bound to the old
 *    one, `classifySecretDrift` would report `client_id_mismatch`, and
 *    `resolveAuthConfigs` would then emit no config at all — zero sign-in
 *    buttons, on a deployment whose setup window is shut.
 *
 * 2. MOIRA, with a FRESH `If-Match` read back from the row rather than a version
 *    carried in the form. A stale precondition turns an ordinary save into
 *    `409 resource_version_conflict`, and the version moves on its own (the
 *    column is bumped by a trigger).
 *
 * 3. SEAL AGAINST THE ID MOIRA RETURNED, never the one the form submitted. The
 *    envelope's AAD binds `(providerId, clientId)`, so sealing against an
 *    unnormalised input produces a secret that cannot be opened for the row that
 *    now exists.
 *
 * 4. RE-CLASSIFY, and report failure unless the two agree. A write that leaves
 *    the console and Moira disagreeing has produced the broken state this screen
 *    exists to repair; reporting "saved" would send the operator away from the
 *    only page that can fix it.
 */
export async function updateAuthProvider(
  client: MoiraClient,
  store: ConsoleSecretStore,
  providerId: string,
  update: AuthProviderUpdate,
  clientSecret: string | null,
): Promise<AuthSettingsWriteResult> {
  const current = await client.getAuthProvider(providerId);
  const sealed = await store.read(current.id);

  const nextClientId = update.clientId?.trim();
  const submittingClientId = nextClientId !== undefined && nextClientId !== "";
  const secretSupplied = clientSecret !== null && clientSecret !== "";
  if (submittingClientId && !secretSupplied) {
    // No seal at all is a DIFFERENT situation from a seal bound to another id,
    // even though both require a secret here. The form pre-fills the client id,
    // so an operator on a deployment with no stored secret submits one on every
    // save without changing anything — and telling them their change needs a
    // secret would describe a change they did not make.
    if (sealed === null) {
      return { ok: false, failure: { kind: "secret_required", reason: "no_secret_stored" } };
    }
    if (nextClientId !== sealed.clientId) {
      return { ok: false, failure: { kind: "secret_required", reason: "client_id_moving" } };
    }
  }

  const body = patchBody(update);
  const written =
    Object.keys(body).length === 0
      ? current
      : await client.patchAuthProvider(current.id, body, ifMatchFor(current));

  if (clientSecret !== null && clientSecret !== "") {
    // Read back off Moira's response, per step 3. `null` here would seal against
    // a client id that does not exist, so it is a refusal rather than a skip.
    const storedClientId = written.client_id ?? null;
    if (storedClientId === null || storedClientId === "") {
      return {
        ok: false,
        failure: {
          kind: "drift_after_write",
          messageKey: CONSOLE_MESSAGE_KEYS.moira_provider_client_id_missing,
        },
      };
    }
    await store.put(written.id, storedClientId, clientSecret);
  }

  const drift = classifySecretDrift(written.client_id ?? null, await store.read(written.id));
  if (drift !== "in_sync") {
    return {
      ok: false,
      failure: { kind: "drift_after_write", messageKey: CONSOLE_SECRET_DRIFT_MESSAGE_KEYS[drift] },
    };
  }
  return { ok: true };
}

/**
 * Replace the stored client secret and nothing else.
 *
 * NO MOIRA REQUEST AT ALL — not a patch, not an `If-Match`, not a version bump.
 * The secret is console-owned, so when the identity provider issues a new one
 * for the SAME client id, Moira's row is already correct and touching it would
 * spend a version on nothing.
 *
 * Sealed against the id read from MOIRA, not from the console's own store: if
 * the two have already drifted, sealing against the stored id would preserve the
 * drift while reporting success.
 */
export async function replaceStoredClientSecret(
  client: MoiraClient,
  store: ConsoleSecretStore,
  providerId: string,
  clientSecret: string,
): Promise<AuthSettingsWriteResult> {
  const current = await client.getAuthProvider(providerId);
  const storedClientId = current.client_id ?? null;
  if (storedClientId === null || storedClientId === "") {
    return {
      ok: false,
      failure: {
        kind: "drift_after_write",
        messageKey: CONSOLE_MESSAGE_KEYS.moira_provider_client_id_missing,
      },
    };
  }
  await store.put(current.id, storedClientId, clientSecret);

  const drift = classifySecretDrift(storedClientId, await store.read(current.id));
  if (drift !== "in_sync") {
    return {
      ok: false,
      failure: { kind: "drift_after_write", messageKey: CONSOLE_SECRET_DRIFT_MESSAGE_KEYS[drift] },
    };
  }
  return { ok: true };
}
