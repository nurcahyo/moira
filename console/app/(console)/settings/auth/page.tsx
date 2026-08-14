// `/settings/auth` — how operators sign in, and the one screen that can repair it.
//
// ============================================================================
// GATED TWICE, FOR TWO DIFFERENT QUESTIONS
// ============================================================================
//
// The `(console)` group's layout answers "is there an operator". This page then
// answers "is it the owner", and renders the answer rather than acting on it: a
// non-owner sees the current configuration and a keyed explanation instead of a
// form. Hiding the screen entirely would be worse — an admin who cannot change
// sign-in still benefits from being able to read what it is, and a menu entry
// that vanishes for some people is a support question nobody can answer from a
// screenshot.
//
// Reads only. The mutation is `app/api/settings/auth/route.ts`, which re-checks
// BOTH gates itself because `app/api/**` sits outside every route group.
//
// ============================================================================
// A FAILED READ RENDERS AS A PAGE, NOT AS A 500
// ============================================================================
//
// `force-dynamic`, because the gate above it is per-request. The a11y walker
// asserts `status < 400` on every discovered route, so a backend outage must
// render as keyed copy rather than take the whole gate red.

import { headers } from "next/headers";

import { consoleRuntime, consoleSecretStore } from "@/lib/auth-runtime";
import { loadAuthSettings } from "@/lib/auth-settings";
import type { AuthSettingsView } from "@/lib/auth-settings-view";
import { consoleSessionCheck } from "@/lib/auth";
import { grantIsOwner, lookUpOwnGrant } from "@/lib/console-api";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import { AuthSettingsPanels } from "@/modules/authSettings/AuthSettingsPanels";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * Everything the screen renders, or `null` when Moira could not be reached.
 *
 * The ownership answer is computed here rather than in the organism for the
 * usual reason: it needs a Moira call with the operator's own credential, and a
 * client component has neither.
 */
async function load(): Promise<AuthSettingsView | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const requestHeaders = await headers();
  const check = await consoleSessionCheck(runtimeState.auth, runtimeState.configs, requestHeaders);
  // The layout already redirected a sessionless visitor; this is the narrowing
  // that gives us the provider row and the issuer, not a second gate.
  if (!check.ok) return null;

  const env = consoleEnv();
  const client = moiraClientForSession(env, runtimeState.auth, requestHeaders);

  // The SAME predicate the route handler gates on, not a second one written for
  // rendering. Two answers to "is this the owner" is one more than the number of
  // times `admin_identities.is_primary` can be right.
  const isOwner = grantIsOwner(
    await lookUpOwnGrant(client, check.consoleIssuer, check.identity.idpSubject),
  );

  return loadAuthSettings(client, consoleSecretStore(env), check.moiraProviderId, isOwner);
}

export default async function AuthSettingsPage() {
  let data: AuthSettingsView | null;
  try {
    data = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    data = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.authsettings_page_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.authsettings_page_intro)}</p>

      {data === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.authsettings_load_failed)}
        </p>
      ) : (
        <AuthSettingsPanels view={data} />
      )}
    </main>
  );
}
