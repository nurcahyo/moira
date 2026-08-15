// `/runners` — containerised Claude runners: provision one, see every one
// already provisioned. Issue #275/#272 workstream R3.
//
// Thin by design, exactly like `/admins` and `/settings/llm`: gated by
// position (inherited from `(console)/layout.tsx`), fetches, renders. Every
// mutation belongs to a route handler under `app/api/runners/**`, each of
// which re-checks the session itself because `app/api/**` sits outside every
// route group.
//
// `force-dynamic` for the same reason every gated screen here is: the session
// gate is per-request, and a failed read renders as a page (below 400) rather
// than a 500 — the a11y walker asserts `status < 400` on every discovered
// route, and this page's own gate entry
// (`e2e/a11y.e2e.ts`'s `ROUTES_NOT_AUDITED_PENDING_AUTHENTICATED_E2E`) only
// covers the unauthenticated redirect, not a crash.

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { ClaudeRunnerRecord } from "@/lib/types";
import { RunnerPanels } from "@/modules/runners/RunnerPanels";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const LIST_LIMIT = 100;

async function load(): Promise<readonly ClaudeRunnerRecord[] | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  const page = await client.listRunners({ limit: LIST_LIMIT });
  return page.data;
}

export default async function RunnersPage() {
  let runners: readonly ClaudeRunnerRecord[] | null;
  try {
    runners = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    runners = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_runners_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.runners_intro)}</p>

      {runners === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.runners_load_failed)}
        </p>
      ) : (
        <RunnerPanels runners={runners} />
      )}
    </main>
  );
}
