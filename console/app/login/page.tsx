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
 * Human-readable names by Moira row id, from the ANONYMOUS projection.
 *
 * Best-effort by design. By the time this runs the configuration has already
 * resolved, so a failure here costs NAMES, not buttons — `SignInPanel` falls
 * back to `console.signIn.button_generic` for a lone provider and to the
 * provider id when there are several (identical accessible names on several
 * buttons would be worse than an ugly one). Doing it the other way round —
 * rendering buttons from this call alone — is the "button that 503s on click"
 * failure this whole page exists to avoid.
 *
 * ONE call for N providers, not one per provider: `getSetupSignInMethods`
 * returns the whole list, and calling it per button would multiply an anonymous
 * request by the provider count on every render of the sign-in page.
 */
async function signInDisplayNames(): Promise<ReadonlyMap<string, string>> {
  try {
    // No credential: `get_setup_sign_in_methods` declares no `security` block,
    // and the registry in `lib/moira-client.ts` is what makes that true of the
    // request rather than merely believed about the endpoint.
    const client = new MoiraClient({ baseUrl: consoleEnv().moiraBaseUrl });
    const response = await client.getSetupSignInMethods();
    return new Map(response.methods.map((method) => [method.id, method.display_name]));
  } catch {
    return new Map();
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

  // Per-provider problems are deliberately NOT rendered here. A resolved
  // deployment with one drifted extra row must still show the working buttons,
  // and the remedy for the drifted row belongs on the auth-settings screen where
  // the operator can act on it — not on the page a locked-out human is looking
  // at. `consoleRuntime()` carries them so that screen can, when it ships.
  const names = await signInDisplayNames();
  return {
    kind: "ready",
    providers: runtimeState.configs.map((config) => ({
      providerId: config.providerId,
      displayName: names.get(config.moiraProviderId) ?? null,
    })),
    // ISSUE #152. The console is past its snapshot TTL and could not re-read the
    // configuration — no bootstrap credential, or Moira is unreachable. The
    // buttons stay, because this configuration is the only one anybody could
    // sign in with; what changes is that the operator is TOLD, rather than
    // discovering it as a fetch error against an endpoint that moved.
    ...(runtimeState.stale ? { noticeKey: CONSOLE_MESSAGE_KEYS.auth_config_stale } : {}),
  };
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
