// `/flows` — multi-agent flow authoring (plan 12 §6).
//
// Thin by design, same posture as `/skills` and `/evals`: guard (inherited
// from `(console)/layout.tsx`), fetch, render. `force-dynamic` and a
// page-level try/catch for the flow read itself, for the same reason every
// screen in this family carries both.
//
// The agent-profile read is a SEPARATE, non-fatal try/catch: it powers the
// step builder's picker rather than the page's own content, and a failure
// there must not take the whole screen down — `FlowStepsBuilder` degrades to a
// free-text agent-profile id field when this is `null`.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { AgentFlowRecord, AgentProfileRecord } from "@/lib/types";
import { FlowsScreen } from "@/modules/flows/FlowsScreen";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

async function loadFlows(): Promise<readonly AgentFlowRecord[] | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  const page = await client.listFlows({ limit: 200 });
  return page.data;
}

async function loadAgentProfiles(): Promise<readonly AgentProfileRecord[] | null> {
  try {
    const runtimeState = await consoleRuntime();
    if (!runtimeState.ok) return null;
    const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
    const page = await client.listAgentProfiles({ limit: 200, status: "active" });
    return page.data;
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    return null;
  }
}

export default async function FlowsPage() {
  let flows: readonly AgentFlowRecord[] | null;
  try {
    flows = await loadFlows();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    flows = null;
  }
  const agentProfiles = await loadAgentProfiles();

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_flows_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.flows_page_intro)}</p>

      {flows === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.flows_load_failed)}
        </p>
      ) : (
        <FlowsScreen flows={flows} agentProfiles={agentProfiles} />
      )}
    </main>
  );
}
