"use client";

// The three organisms on `/settings/llm`, wired to ONE thing: re-read the page
// after any of them changes something.
//
// ============================================================================
// WITHOUT THIS THE SCREEN LIED AFTER EVERY SUCCESSFUL MUTATION
// ============================================================================
//
// `page.tsx` is a server component. It reads the whole view from Moira on the
// server and hands it down as a prop, so a `fetch` a client organism makes
// afterwards changes the deployment and nothing on the screen: the badge still
// says Active, the readiness line still says Ready, the trace still describes a
// state that has moved on. Every organism already exposes the callback for it
// (`onConnected` / `onCreated` / `onChanged`) and the page passed none of them,
// which is a seam with nothing plugged into it.
//
// A client component is the only place `useRouter().refresh()` can be called,
// and `refresh()` is the right verb rather than a local `useState` copy: the
// server re-runs `loadLlmSettings` and re-renders with what MOIRA says, not with
// what this component believes it just did. That distinction is the same one
// `ProviderChainPanel`'s header makes about the readiness list, and it matters
// most on the paths where the console and Moira can disagree — a reuse-first
// chain that repaired a disabled row, a bind that enabled an existing policy.
//
// It holds no state and renders no copy of its own; the props are the same
// client-safe view model `lib/llm-view.ts` defines.

import { useRouter } from "next/navigation";

import type { LlmProviderView } from "@/lib/llm-view";

import { ConnectVllmPanel } from "./ConnectVllmPanel";
import { ProviderForm } from "./ProviderForm";
import { ProviderList } from "./ProviderList";

export interface LlmSettingsPanelsProps {
  readonly defaultBaseUrl: string;
  readonly providers: readonly LlmProviderView[];
}

export function LlmSettingsPanels({ defaultBaseUrl, providers }: LlmSettingsPanelsProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  return (
    <>
      <ConnectVllmPanel defaultBaseUrl={defaultBaseUrl} onConnected={reload} />
      <ProviderForm onCreated={reload} />
      <ProviderList providers={providers} onChanged={reload} />
    </>
  );
}
