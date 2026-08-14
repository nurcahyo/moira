// `/graph` — the derived relationship graph over the agent-platform and provider/model
// registries (plan 12 §4, issue #234).
//
// Thin by design, same posture as `/admins`: guard (inherited from `(console)/layout.tsx`),
// fetch, render. This page performs one read (`GET /api/v1/admin/graph`) and delegates all
// rendering to the `RelationshipGraph` organism — nothing here composes `@xyflow/react`
// directly.
//
// `force-dynamic` and a page-level try/catch for the same reason `/admins` carries both: the
// gate above this route is per-request, and if Moira is unreachable the page still answers
// below 400 with a keyed explanation rather than a 500.

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { GraphResponse } from "@/lib/types";
import { RelationshipGraph } from "@/modules/graph/RelationshipGraph";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

async function load(): Promise<GraphResponse | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const env = consoleEnv();
  const client = moiraClientForSession(env, runtimeState.auth, await headers());
  return client.getGraph();
}

export default async function GraphPage() {
  let graph: GraphResponse | null;
  try {
    graph = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    graph = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_graph_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.graph_intro)}</p>

      {graph === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.graph_request_failed)}
        </p>
      ) : (
        <RelationshipGraph graph={graph} />
      )}
    </main>
  );
}
