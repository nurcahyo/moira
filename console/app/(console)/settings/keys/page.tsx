// `/settings/keys` — the credential an application presents to Moira.
//
// ============================================================================
// GATED BY POSITION, LIKE EVERY OTHER CONSOLE SCREEN
// ============================================================================
//
// Inside the `(console)` route group, so it inherits that layout's session gate:
// an unauthenticated visitor is redirected to `/login` before this module runs.
// This file therefore performs READS only — every mutation belongs to a route
// handler under `app/api/keys/**`, each of which re-checks the session itself
// because `app/api/**` sits outside every route group.
//
// Ordinary administration, not setup. Minting a credential that can send prompts
// must not be reachable before the first admin is claimed.
//
// ============================================================================
// A FAILED READ RENDERS AS A PAGE, NOT AS A 500
// ============================================================================
//
// `force-dynamic`, because the gate above it is per-request. If Moira is
// unreachable the page still answers below 400 with a keyed explanation: the
// a11y walker asserts `status < 400` on every discovered route, and a 500 here
// would take the whole gate red for a backend outage. Same reasoning as
// `/settings/llm` and `/admins`.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { loadConsumerKeys } from "@/lib/consumer-keys";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ConsumerKeysView } from "@/lib/keys-view";
import { moiraClientForSession } from "@/lib/moira-session";
import { ConsumerKeyPanels } from "@/modules/keys/ConsumerKeyPanels";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** Everything the screen renders, or `null` when Moira could not be reached. */
async function load(): Promise<ConsumerKeysView | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  return loadConsumerKeys(client);
}

export default async function ConsumerKeysPage() {
  let data: ConsumerKeysView | null;
  try {
    data = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    data = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.keys_page_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.keys_page_intro)}</p>

      {data === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.keys_load_failed)}
        </p>
      ) : (
        <ConsumerKeyPanels view={data} />
      )}
    </main>
  );
}
