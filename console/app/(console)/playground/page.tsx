// `/playground` — a test-chat page that drives Moira's real execution path
// (issue #261).
//
// Thin by design, same posture as `/skills`, `/flows`, `/evals`: guard
// (inherited from `(console)/layout.tsx`), fetch, render. `force-dynamic` for
// the same reason every screen in this family carries it.
//
// UNLIKE those screens, none of the three server-side reads below is this
// page's PRIMARY resource — that role belongs to the live execution itself,
// which happens client-side through `app/api/playground/**` on Send, not on
// page load. All three reads here exist only to power `PlaygroundControls`'
// pickers, so all three are NON-FATAL: any of them failing degrades the
// controls to their "no override" options plus one inline notice, and the
// page still renders and is still usable for sending a prompt.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { readListOrNull } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { AgentProfileRecord, ProviderRecord, RouteDefinitionRecord } from "@/lib/types";
import { PlaygroundScreen } from "@/modules/playground/PlaygroundScreen";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

interface Pickers {
  readonly routes: readonly RouteDefinitionRecord[] | null;
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  readonly providers: readonly ProviderRecord[] | null;
  readonly anyFailed: boolean;
}

const EMPTY_PICKERS: Pickers = { routes: null, agentProfiles: null, providers: null, anyFailed: true };

async function loadPickers(): Promise<Pickers> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return EMPTY_PICKERS;
  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());

  const [routes, agentProfiles, providers] = await Promise.all([
    readListOrNull(() => client.listRoutes({ limit: 200 })),
    readListOrNull(() => client.listAgentProfiles({ limit: 200, status: "active" })),
    readListOrNull(() => client.listProviders({ limit: 200, status: "active" })),
  ]);

  return {
    routes,
    agentProfiles,
    providers,
    anyFailed: routes === null || agentProfiles === null || providers === null,
  };
}

export default async function PlaygroundPage() {
  const pickers = await loadPickers();

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_playground_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.playground_page_intro)}</p>

      {pickers.anyFailed && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.playground_pickers_load_failed)}
        </p>
      )}

      <PlaygroundScreen
        routes={pickers.routes}
        agentProfiles={pickers.agentProfiles}
        providers={pickers.providers}
      />
    </main>
  );
}
