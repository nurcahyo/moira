// @server-only
//
// Parses the `steps` array both `POST /api/flows` and `PATCH /api/flows/{id}`
// accept, shared by both route handlers rather than exported from one `route.ts`
// and imported by the other.
//
// Next.js's route handler compiler recognises only the HTTP-method exports
// (`GET`/`POST`/…) and a small set of route config constants
// (`runtime`/`dynamic`/…) from a `route.ts` module — an arbitrary extra named
// export is outside that contract, so shared logic between two route files
// belongs in `lib/`, not in one route importing from its sibling.

import "server-only";

import type { AgentFlowStepCreateRequest, FlowStepOnFailure } from "./types";

const ON_FAILURE_VALUES: readonly FlowStepOnFailure[] = ["abort", "continue"];

/** `null` means the caller sent something that is not a valid step list. */
export function parseFlowSteps(value: unknown): AgentFlowStepCreateRequest[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;

  const steps: AgentFlowStepCreateRequest[] = [];
  for (const entry of value) {
    if (typeof entry !== "object" || entry === null) return null;
    const record = entry as Record<string, unknown>;

    const stepKey = typeof record["step_key"] === "string" ? record["step_key"].trim() : "";
    const stepOrder = record["step_order"];
    const agentProfileId =
      typeof record["agent_profile_id"] === "string" ? record["agent_profile_id"].trim() : "";
    if (stepKey === "" || typeof stepOrder !== "number" || agentProfileId === "") return null;

    const onFailure = record["on_failure"];
    const step: AgentFlowStepCreateRequest = {
      step_key: stepKey,
      step_order: stepOrder,
      agent_profile_id: agentProfileId,
    };
    if (typeof onFailure === "string" && ON_FAILURE_VALUES.includes(onFailure as FlowStepOnFailure)) {
      step.on_failure = onFailure as FlowStepOnFailure;
    }
    steps.push(step);
  }
  return steps;
}
