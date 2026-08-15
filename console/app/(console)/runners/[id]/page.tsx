// `/runners/[id]` — one runner's lifecycle. Issue #275/#272 workstream R3.
//
// Reading the runner here is what advances it: `GET /api/v1/admin/runners/{id}`
// refreshes the row from the runner service before answering (that is what
// surfaces `authorization_url`), so simply loading this page is part of the
// provisioning walkthrough, not a side effect of it. `RunnerDetail` polls the
// same way on an interval while the state is not yet settled — see that
// component's header.
//
// Providers are read alongside the runner (bounded to one page) so the
// finalize form can offer a real choice without a second round trip once the
// runner reaches `ready`. Fetching them even when the runner is nowhere near
// `ready` is deliberate: the page has one `force-dynamic` gate, and a
// conditional second fetch keyed off state would make this page's data
// shape depend on when the runner service happens to be polled, which is
// exactly the kind of drift `RunnerDetail`'s own header names for the
// `runner_token_unavailable` flag.
//
// `force-dynamic` and the below-400 rendering discipline are the same as
// `/runners` — see that page's header.

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { ClaudeRunnerRecord, ProviderRecord } from "@/lib/types";
import { RunnerDetailPanel } from "@/modules/runners/RunnerDetailPanel";
import { headers } from "next/headers";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const PROVIDER_LIST_LIMIT = 200;

interface ScreenData {
  readonly runner: ClaudeRunnerRecord;
  readonly providers: readonly ProviderRecord[];
}

type LoadResult =
  | { readonly kind: "ok"; readonly data: ScreenData }
  | { readonly kind: "not_found" }
  | { readonly kind: "failed" };

async function load(id: string): Promise<LoadResult> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return { kind: "failed" };

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  try {
    const [runner, providerPage] = await Promise.all([
      client.getRunner(id),
      client.listProviders({ limit: PROVIDER_LIST_LIMIT }),
    ]);
    return { kind: "ok", data: { runner, providers: providerPage.data } };
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    if (error.moiraError.kind === "api" && error.moiraError.code === "runner_not_found") {
      return { kind: "not_found" };
    }
    return { kind: "failed" };
  }
}

export default async function RunnerDetailPage({
  params,
}: {
  readonly params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  const result = await load(id);

  return (
    <main className={styles.main}>
      {result.kind === "ok" && (
        <RunnerDetailPanel runner={result.data.runner} providers={result.data.providers} />
      )}
      {result.kind === "not_found" && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.runners_detail_not_found)}
        </p>
      )}
      {result.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.runners_detail_load_failed)}
        </p>
      )}
    </main>
  );
}
