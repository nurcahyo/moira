"use client";

// The two organisms on `/runners`, wired to ONE thing: after a successful
// provision, navigate to the new runner's detail page. Same shape as
// `modules/llm/LlmSettingsPanels.tsx` — a client component is the only place
// `useRouter()` can be called, and a server component cannot navigate.

import { useRouter } from "next/navigation";

import type { ClaudeRunnerRecord } from "@/lib/types";

import { RunnerList } from "./RunnerList";
import { RunnerProvisionForm } from "./RunnerProvisionForm";

export interface RunnerPanelsProps {
  readonly runners: readonly ClaudeRunnerRecord[];
}

export function RunnerPanels({ runners }: RunnerPanelsProps) {
  const router = useRouter();

  return (
    <>
      <RunnerProvisionForm onProvisioned={(id) => router.push(`/runners/${id}`)} />
      <RunnerList runners={runners} />
    </>
  );
}
