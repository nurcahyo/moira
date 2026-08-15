// `/skills` — the tool/guard skill registry, and the OpenAPI import pipeline
// (plan 12 §5, issue #237).
//
// Thin by design, same posture as `/settings/llm` and `/graph`: guard
// (inherited from `(console)/layout.tsx`), fetch, render. `force-dynamic` and a
// page-level try/catch for the same reason those two carry both — the gate
// above this route is per-request, and if Moira is unreachable the page still
// answers below 400 with a keyed explanation rather than a 500.

import { headers } from "next/headers";

import { consoleRuntime } from "@/lib/auth-runtime";
import { consoleEnv } from "@/lib/env";
import { isMoiraRequestError } from "@/lib/errors";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { moiraClientForSession } from "@/lib/moira-session";
import type { SkillRecord } from "@/lib/types";
import { SkillsScreen } from "@/modules/skills/SkillsScreen";

import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

async function load(): Promise<readonly SkillRecord[] | null> {
  const runtimeState = await consoleRuntime();
  if (!runtimeState.ok) return null;

  const client = moiraClientForSession(consoleEnv(), runtimeState.auth, await headers());
  const page = await client.listSkills({ limit: 200 });
  return page.data;
}

export default async function SkillsPage() {
  let skills: readonly SkillRecord[] | null;
  try {
    skills = await load();
  } catch (error) {
    if (!isMoiraRequestError(error)) throw error;
    skills = null;
  }

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.page_skills_title)}</h1>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.skills_page_intro)}</p>

      {skills === null ? (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.skills_load_failed)}
        </p>
      ) : (
        <SkillsScreen skills={skills} />
      )}
    </main>
  );
}
