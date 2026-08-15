"use client";

// The two top-level organisms on `/flows`, wired to re-read the page after
// either of them changes something. Same reasoning as `LlmSettingsPanels`,
// `SkillsScreen` and `EvalsScreen`.

import { useRouter } from "next/navigation";

import type { AgentFlowRecord, AgentProfileRecord } from "@/lib/types";

import { FlowCreateForm } from "./FlowCreateForm";
import { FlowList } from "./FlowList";

export interface FlowsScreenProps {
  readonly flows: readonly AgentFlowRecord[];
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
}

export function FlowsScreen({ flows, agentProfiles }: FlowsScreenProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  return (
    <>
      <FlowCreateForm agentProfiles={agentProfiles} onCreated={reload} />
      <FlowList flows={flows} agentProfiles={agentProfiles} onChanged={reload} />
    </>
  );
}
