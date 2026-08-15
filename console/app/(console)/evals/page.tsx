// `/evals` — eval suites, cases and runs (plan 12 §3).
//
// Thin by design, same posture as `/skills`: guard (inherited from
// `(console)/layout.tsx`), fetch, render. `force-dynamic` and a page-level
// try/catch for the same reason every screen in this family carries both.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { EvalSuiteRecord } from "@/lib/types";
import { EvalsScreen } from "@/modules/evals/EvalsScreen";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

async function load(): Promise<readonly EvalSuiteRecord[] | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  const page = await client.listEvalSuites({ limit: 200 });
  return page.data;
}

export default async function EvalsPage() {
  let suites: readonly EvalSuiteRecord[] | null;
  try {
    suites = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    suites = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_evals_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.evals_page_intro)}</p>

      {suites === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.evals_load_failed)}
        </p>
      ) : (
        <EvalsScreen suites={suites} />
      )}
    </main>
  );
}
