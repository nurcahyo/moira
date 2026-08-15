// `/providers/health` — the rolling reachability window for every enabled
// provider (issue #83).
//
// Thin by design, same posture as `/graph`: guard (inherited from
// `(console)/layout.tsx`), fetch, render. `force-dynamic` and a page-level
// try/catch for the same reason every other screen in this family carries
// both — the gate above this route is per-request, and if Moira is
// unreachable the page still answers below 400 with a keyed explanation
// rather than a 500.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { ProviderHealthResponse } from "@/lib/types";
import { ProviderHealthTable } from "@/modules/providerHealth/ProviderHealthTable";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

async function load(): Promise<ProviderHealthResponse | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  return client.getProviderHealth();
}

export default async function ProviderHealthPage() {
  let health: ProviderHealthResponse | null;
  try {
    health = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    health = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_provider_health_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.providerhealth_page_intro)}</p>

      {health === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.providerhealth_load_failed)}
        </p>
      ) : (
        <ProviderHealthTable health={health} />
      )}
    </main>
  );
}
