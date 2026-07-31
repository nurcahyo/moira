// `/login` — the sign-in page.
//
// ============================================================================
// WHY IT SITS OUTSIDE THE `(console)` GROUP
// ============================================================================
//
// It is a SIBLING of the authenticated group, not a member of it. Three shipped
// code paths redirect here — `lib/errors.ts:49` and `:319-323`
// (`isSessionExpired` -> remedy `reauthenticate`) and the `already_complete`
// remedy at `:130` — and a `/login` rendered inside an auth-gated layout would
// make every one of them a redirect loop.
//
// `app/layout.tsx` stays the root layout for the same family of reasons; see its
// header.
//
// ============================================================================
// IT MUST ANSWER < 400 ON A COLD, UNCONFIGURED CONSOLE
// ============================================================================
//
// `e2e/a11y.e2e.ts:47-80` visits every discovered page-level route and fails the
// gate on any status >= 400, then runs axe. The normal first-run state of this
// deployment is "no provider is enabled yet", which is a CONFIGURATION FACT and
// not an error — so it renders as a 200 body carrying a keyed message. Nothing
// on this path throws for a configuration problem: `consoleRuntime()` returns
// `{ ok: false }` rather than raising, deliberately.
//
// `force-dynamic` because the answer depends on Moira and on the process's
// snapshot. It also keeps the page out of the build-time prerender, which is
// where a statically rendered `process.env` read would be resolved — the
// mechanism behind the `E2E_SKIP_BUILD` hole recorded in
// `e2e/secret-leak.e2e.ts:96-102`.

import { CONSOLE_SECRET_DRIFT_MESSAGE_KEYS, type ConsoleSecretDrift } from "@/lib/console-secrets";
import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { MoiraClient } from "@/lib/moira-client";
import { SignInPanel, type SignInPanelState } from "@/modules/signIn/SignInPanel";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/**
 * The provider's human-readable name, from the ANONYMOUS projection.
 *
 * Best-effort by design. By the time this runs the configuration has already
 * resolved, so a failure here costs a NAME, not a button — the caller falls back
 * to `console.signIn.button_generic`. Doing it the other way round (rendering a
 * button from this call alone) is the "button that 503s on click" failure.
 */
async function signInDisplayName(moiraProviderId: string): Promise<string | null> {
  try {
    // No credential: `get_setup_sign_in_methods` declares no `security` block,
    // and the registry in `lib/moira-client.ts` is what makes that true of the
    // request rather than merely believed about the endpoint.
    const client = new MoiraClient({ baseUrl: consoleEnv().moiraBaseUrl });
    const response = await client.getSetupSignInMethods();
    const method = response.methods.find((candidate) => candidate.id === moiraProviderId);
    return method?.display_name ?? null;
  } catch {
    return null;
  }
}

/** The most specific key available for a refusal. */
function refusalKey(messageKey: string, drift: ConsoleSecretDrift | undefined): string {
  // `console_secret_unavailable` collapses three distinct D7 drift states onto
  // one key. When the discriminant is present, the more specific key says which
  // of them it is — "re-enter the secret" and "the Moira row has no client id"
  // are different instructions to a different person.
  if (drift === undefined || drift === "in_sync") return messageKey;
  return CONSOLE_SECRET_DRIFT_MESSAGE_KEYS[drift];
}

async function resolveSignInState(): Promise<SignInPanelState> {
  let runtimeState: Awaited<ReturnType<typeof consoleRuntime>>;
  try {
    runtimeState = await consoleRuntime();
  } catch (error) {
    // Resolving the configuration means calling Moira, so a Moira outage lands
    // here — the same catch `app/api/auth/[...all]/route.ts:50-59` makes, and
    // for the same reason: without it this reads as a console bug rather than
    // as "the backend is down".
    if (isMoiraRequestError(error)) {
      const text = error.moiraError.text;
      return {
        kind: "unavailable",
        messageKey: text.messageKey,
        message: text.message,
        messageArgs: text.messageArgs,
      };
    }
    throw error;
  }

  if (!runtimeState.ok) {
    const resolution = runtimeState.resolution;
    return { kind: "unavailable", messageKey: refusalKey(resolution.messageKey, resolution.drift) };
  }

  const displayName = await signInDisplayName(runtimeState.config.moiraProviderId);
  return { kind: "ready", providerId: runtimeState.config.providerId, displayName };
}

export default async function LoginPage() {
  const state = await resolveSignInState();

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_login_title)}</h1>
      <SignInPanel state={state} />
    </main>
  );
}
