// `/settings/llm` — which language model providers exist, and whether a prompt
// can actually reach them.
//
// ============================================================================
// GATED BY POSITION, LIKE EVERY OTHER CONSOLE SCREEN
// ============================================================================
//
// It sits inside the `(console)` route group, so it inherits that layout's
// session gate: an unauthenticated visitor is redirected to `/login` before this
// module runs at all. That is the same gate `/admins` inherits, and it is the
// reason this file performs reads only — every mutation belongs to a route
// handler under `app/api/llm/**`, each of which re-checks the session itself
// because `app/api/**` sits outside every route group.
//
// This is ORDINARY ADMINISTRATION and not setup. Nothing here is reachable
// before the first admin is claimed.
//
// ============================================================================
// A FAILED READ RENDERS AS A PAGE, NOT AS A 500
// ============================================================================
//
// `force-dynamic`, because the gate above it is per-request. If Moira is
// unreachable the page still answers below 400 with a keyed explanation: the
// a11y walker asserts `status < 400` on every discovered route, and a 500 here
// would take the whole gate red for a backend outage. The same reasoning as
// `/admins`.
//
// ============================================================================
// THE `general` ROUTE'S ABSENCE IS STATED, NOT WORKED AROUND
// ============================================================================
//
// Migration `0005` seeds it. If it is missing, no routing policy can be created
// and the console does NOT create one — `POST /api/v1/admin/routes` documents no
// 409 for a duplicate `route_key`, so a console that helpfully created a second
// `general` would leave routing with two candidate definitions and no rule for
// choosing. The page says so instead.

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { loadLlmSettings } from "@/lib/llm-settings";
import { LOCAL_VLLM_BASE_URL, type LlmSettingsView } from "@/lib/llm-view";
import { moiraClientForSession } from "@/lib/moira-session";
import { ConnectVllmPanel } from "@/modules/llm/ConnectVllmPanel";
import { ProviderForm } from "@/modules/llm/ProviderForm";
import { ProviderList } from "@/modules/llm/ProviderList";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** Everything the screen renders, or `null` when Moira could not be reached. */
async function load(): Promise<LlmSettingsView | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  return loadLlmSettings(client);
}

export default async function LlmSettingsPage() {
  let data: LlmSettingsView | null;
  try {
    data = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    data = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.llm_page_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.llm_page_intro)}</p>

      {data === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.llm_load_failed)}
        </p>
      ) : (
        <>
          {data.generalRouteId === null && (
            <p className={styles.problem} role="alert">
              {t(CONSOLE_MESSAGE_KEYS.llm_general_route_missing)}
            </p>
          )}
          <ConnectVllmPanel defaultBaseUrl={LOCAL_VLLM_BASE_URL} />
          <ProviderForm />
          <ProviderList providers={data.providers} />
        </>
      )}
    </main>
  );
}
