"use client";

// The thin `useRouter()` wrapper around `RunnerDetail` — same split as
// `modules/llm/LlmSettingsPanels.tsx` around `ProviderList`, and for the same
// reason: a server component (the page) cannot call `useRouter()`, and
// `RunnerDetail` deliberately does not either, so it stays renderable in a
// unit test with no App Router context. See `RunnerDetail.tsx`'s header.

import { useRouter } from "next/navigation";

import type { ClaudeRunnerRecord, ProviderRecord } from "@/lib/types";

import { RunnerDetail } from "./RunnerDetail";

export interface RunnerDetailPanelProps {
  readonly runner: ClaudeRunnerRecord;
  readonly providers: readonly ProviderRecord[];
}

export function RunnerDetailPanel({ runner, providers }: RunnerDetailPanelProps) {
  const router = useRouter();

  return (
    <RunnerDetail
      runner={runner}
      providers={providers}
      onRefresh={() => router.refresh()}
      onDeleted={() => router.push("/runners")}
    />
  );
}
