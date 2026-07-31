// The authenticated console chrome.
//
// ============================================================================
// WHAT A ROUTE GROUP BUYS HERE
// ============================================================================
//
// `(console)` contributes no URL segment, so `app/(console)/page.tsx` is still
// `/`. What it contributes is a LAYOUT BOUNDARY: everything inside this group
// passes the session gate below, and everything outside it — `/login`, and every
// route handler under `app/api/**` — does not.
//
// That split is the whole point. `/login` must not be gated (three shipped code
// paths redirect there, and a gated `/login` makes each one a loop), and
// `app/api/**` sits outside every group by construction, which is also why
// `app/layout.tsx` stays the ROOT layout rather than moving in here: Next
// requires a root layout for the whole segment tree.
//
// `e2e/support/routes.ts:46-48,104` already strips route-group segments when it
// discovers routes, so both e2e gates picked this up with no edit.
//
// ============================================================================
// WHY THE GATE FAILS *CLOSED* ON A CONFIGURATION PROBLEM
// ============================================================================
//
// `consoleRuntime()` returns `{ ok: false }` for the normal first-run state
// ("no provider is enabled yet") and throws `MoiraRequestError` when Moira is
// unreachable. Neither is a session, and neither should render the console
// shell — so both land on `/login`, which is the page that knows how to explain
// them. Rendering the chrome and letting the first data fetch fail would show an
// operator a working-looking console over a backend that cannot authorise them.
//
// The chrome itself is deliberately thin at this wave: navigation, a header and
// a sign-out control arrive with the surfaces they navigate to. What ships here
// is the boundary, because the boundary is what the later surfaces depend on.

import { redirect } from "next/navigation";
import { headers } from "next/headers";
import type { ReactNode } from "react";

import { consoleSessionCheck } from "@/lib/auth";
import { consoleRuntime } from "@/lib/auth-runtime";
import { isMoiraRequestError } from "@/lib/errors";

import styles from "./layout.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** Where an ungated visitor is sent. Must stay outside this group. */
const SIGN_IN_PATH = "/login";

/**
 * Is there a console session right now, **and may it act**?
 *
 * Never throws for a configuration problem — see the header. A `MoiraRequestError`
 * is caught here rather than at the call site so that `redirect()`'s own control-
 * flow throw is never inside a `catch`.
 *
 * ============================================================================
 * WHY THIS ASKS THE CREDENTIAL BOUNDARY RATHER THAN COUNTING COOKIES (F25)
 * ============================================================================
 *
 * It used to be `session !== null`, which admits anyone the IdP authenticated —
 * including an address outside `allowed_email_domains`. That human got the whole
 * console shell and then a bare `403` from Moira on the first data fetch, with
 * nothing on screen explaining which policy refused them.
 *
 * `consoleSessionCheck` runs the *same* `checkSession` that `jwt.getSubject` runs
 * before minting, from the same resolved config, so the page and the token cannot
 * disagree. **This gate is not the security control** — `getSubject` and the token
 * route are, and Moira is the enforcer behind both. Deleting this line loses the
 * explanation, not the enforcement; deleting `getSubject`'s loses the enforcement.
 * That asymmetry is why a page-level-only wiring was rejected.
 */
async function hasConsoleSession(): Promise<boolean> {
  try {
    const runtimeState = await consoleRuntime();
    if (!runtimeState.ok) return false;
    const check = await consoleSessionCheck(
      runtimeState.auth,
      runtimeState.config,
      await headers(),
    );
    return check.ok;
  } catch (error) {
    if (isMoiraRequestError(error)) return false;
    throw error;
  }
}

export default async function ConsoleLayout({ children }: { children: ReactNode }) {
  const signedIn = await hasConsoleSession();
  // Outside the `try` above, deliberately: `redirect()` signals by throwing, and
  // a `catch` around it would swallow the redirect and render the chrome.
  if (!signedIn) redirect(SIGN_IN_PATH);

  return <div className={styles.shell}>{children}</div>;
}
