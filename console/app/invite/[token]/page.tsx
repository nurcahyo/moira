// `/invite/[token]` — the public redemption page.
//
// ============================================================================
// IT IS OUTSIDE `(console)`, AND IT HAS TO BE
// ============================================================================
//
// The invitee has no admin grant when they arrive — that is what they are here
// to obtain. A page inside the authenticated group would redirect them to
// `/login` and lose the token from the URL on the way, which is the one thing
// the link exists to carry.
//
// ============================================================================
// AN UNUSABLE INVITATION RENDERS AS A PAGE, NOT AS A 404
// ============================================================================
//
// `e2e/a11y.e2e.ts` asserts `status < 400` on every discovered route, and this
// route's fixture URL holds a token that matches nothing. A `notFound()` here
// would take the a11y gate red for the ordinary case of an expired link.
//
// It is also the better design independently: an expired, revoked or already-
// redeemed invitation is a CONDITION the holder needs explained, not a missing
// document. The explanation is Moira's own keyed refusal, rendered through
// `t()`, with the console supplying nothing beyond a heading.
//
// ============================================================================
// THE TOKEN IS EXCHANGED SERVER-SIDE AND NEVER SENT DOWN
// ============================================================================
//
// `previewInvite` runs here, on the server, with the token in the request BODY.
// What crosses to the browser is the PREVIEW — `constraint`, `value`,
// `expires_at` — which is exactly what Moira is willing to tell an
// unauthenticated holder. `InviteAcceptPanel` receives that and no token; when
// the invitee accepts, it reads the token back out of `location.pathname`, which
// is where it already was.
//
// The one place the token does exist client-side is the browser's own URL and
// the router state Next derives from it. That is inherent to a shareable link
// and is bounded on Moira's side (prefix lookup before any Argon2 work, so no
// CPU oracle; identical `invite_not_found` for a wrong prefix and a wrong hash,
// so no guessing oracle) and on the console's (`smoke.e2e.ts` asserts this route
// contacts no foreign origin, so the token reaches no `Referer` chain and no
// analytics call — there is no analytics in this console).

import { consoleSessionCheck } from "@/lib/auth";
import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { previewInvite } from "@/lib/invites";
import { MoiraClient } from "@/lib/moira-client";
import type { ClientSafeText } from "@/lib/errors";
import type { AdminInvitePreviewResponse } from "@/lib/types";
import { InviteAcceptPanel } from "@/modules/invite/InviteAcceptPanel";
import { SignInPanel, type SignInPanelState } from "@/modules/signIn/SignInPanel";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type PreviewOutcome =
  | { readonly kind: "ok"; readonly preview: AdminInvitePreviewResponse }
  | { readonly kind: "unusable"; readonly text: ClientSafeText | null };

/**
 * Exchange the token for what an anonymous holder may be told.
 *
 * ANONYMOUS on purpose: `preview_admin_invite` declares no `security` block at
 * all, so this client carries no credential. Building it from
 * `moiraClientForSetup` would put the bootstrap system key on a request made for
 * whoever opened the link.
 */
async function preview(token: string): Promise<PreviewOutcome> {
  try {
    const client = new MoiraClient({ baseUrl: consoleEnv().moiraBaseUrl });
    return { kind: "ok", preview: await previewInvite(client, token) };
  } catch (error) {
    if (isMoiraRequestError(error)) return { kind: "unusable", text: error.moiraError.text };
    throw error;
  }
}

/**
 * Human-readable names by Moira row id, from the ANONYMOUS projection.
 *
 * `get_setup_sign_in_methods` declares no `security` block, so this client
 * carries no credential — which is what makes it usable on a page reachable by
 * anyone holding a link.
 */
async function signInDisplayNames(): Promise<ReadonlyMap<string, string>> {
  try {
    const client = new MoiraClient({ baseUrl: consoleEnv().moiraBaseUrl });
    const response = await client.getSetupSignInMethods();
    return new Map(response.methods.map((method) => [method.id, method.display_name]));
  } catch {
    return new Map();
  }
}

/** Does the visitor already have a console session that may act? */
async function hasSession(): Promise<boolean> {
  try {
    const runtimeState = await consoleRuntime();
    if (!runtimeState.ok) return false;
    const check = await consoleSessionCheck(runtimeState.auth, runtimeState.configs, await headers());
    return check.ok;
  } catch (error) {
    if (isMoiraRequestError(error)) return false;
    throw error;
  }
}

/**
 * What to render in place of a sign-in panel when the visitor has no session.
 *
 * The same resolution `/login` performs, for the same reason: `PublicSignInMethod`
 * is enough to RENDER a button and not enough to RESOLVE the configuration
 * behind one, so buttons come from `consoleRuntime()`'s resolved configs and
 * never from the anonymous projection alone. A button rendered from the
 * projection alone is a working-looking button that 503s on click.
 */
async function signInState(): Promise<SignInPanelState> {
  try {
    const runtimeState = await consoleRuntime();
    if (!runtimeState.ok) {
      return { kind: "unavailable", messageKey: runtimeState.resolution.messageKey };
    }
    // Names, best-effort, from the ANONYMOUS projection — the same call `/login`
    // makes and for the same reason: by the time this runs the configuration has
    // already resolved, so a failure here costs NAMES, not buttons. Without it an
    // invitee is offered "Sign in with moira-console-idp-github" instead of
    // "Sign in with GitHub".
    const names = await signInDisplayNames();
    return {
      kind: "ready",
      providers: runtimeState.configs.map((config) => ({
        providerId: config.providerId,
        displayName: names.get(config.moiraProviderId) ?? null,
      })),
    };
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    const text = error.moiraError.text;
    return { kind: "unavailable", messageKey: text.messageKey, message: text.message };
  }
}

export default async function InvitePage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = await params;
  const outcome = await preview(token);

  if (outcome.kind === "unusable") {
    return (
      <main className={styles.main}>
        <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_invite_title)}</h1>
        <h2 className={styles.unusable}>{t(CONSOLE_MESSAGE_KEYS.invite_unusable_heading)}</h2>
        <p className={styles.problem} role="alert">
          {outcome.text === null
            ? t(CONSOLE_MESSAGE_KEYS.invite_request_failed)
            : t(outcome.text.messageKey, outcome.text.messageArgs, outcome.text.message)}
        </p>
      </main>
    );
  }

  const signedIn = await hasSession();

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_invite_title)}</h1>
      <InviteAcceptPanel
        constraint={outcome.preview.constraint}
        value={outcome.preview.value}
        expiresAt={outcome.preview.expires_at}
        signedIn={signedIn}
      />
      {!signedIn && <SignInPanel state={await signInState()} />}
    </main>
  );
}
