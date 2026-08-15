"use client";

// The two top-level organisms on `/evals`, wired to re-read the page after
// either of them changes something. Same reasoning as `LlmSettingsPanels` and
// `SkillsScreen`.

import { useRouter } from "next/navigation";

import type { EvalSuiteRecord } from "@/lib/types";

import { EvalSuiteCreateForm } from "./EvalSuiteCreateForm";
import { EvalSuiteList } from "./EvalSuiteList";

export interface EvalsScreenProps {
  readonly suites: readonly EvalSuiteRecord[];
}

export function EvalsScreen({ suites }: EvalsScreenProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  return (
    <>
      <EvalSuiteCreateForm onCreated={reload} />
      <EvalSuiteList suites={suites} onChanged={reload} />
    </>
  );
}
