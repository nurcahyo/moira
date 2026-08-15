"use client";

// The three organisms on `/skills`, wired to ONE thing: re-read the page after
// any of them changes something.
//
// Same reasoning as `LlmSettingsPanels` — `page.tsx` is a server component that
// reads the whole list from Moira once, so a client organism's `fetch`
// afterwards changes the deployment and nothing on the screen unless something
// calls `router.refresh()`.

import { useRouter } from "next/navigation";

import type { SkillRecord } from "@/lib/types";

import { SkillCreateForm } from "./SkillCreateForm";
import { SkillImportPanel } from "./SkillImportPanel";
import { SkillList } from "./SkillList";

export interface SkillsScreenProps {
  readonly skills: readonly SkillRecord[];
}

export function SkillsScreen({ skills }: SkillsScreenProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  return (
    <>
      <SkillImportPanel onImported={reload} />
      <SkillCreateForm onCreated={reload} />
      <SkillList skills={skills} onChanged={reload} />
    </>
  );
}
