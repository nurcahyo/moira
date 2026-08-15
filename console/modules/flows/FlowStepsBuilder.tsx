"use client";

// The ordered step list, shared by the create-flow form and a flow's inline
// edit form — both send the WHOLE list on submit (`AgentFlowCreateRequest.steps`
// / `AgentFlowPatchRequest.steps`, decision 13), so one controlled component is
// the source of truth for it rather than two copies that could drift.
//
// A step row's agent-profile picker is a `<select>` over `agentProfiles` when
// the caller could load that list, and degrades to a free-text id field when it
// could not — `flows_step_agent_profiles_load_failed` names the degradation
// rather than blocking flow authoring on a read this component does not own.

import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AgentFlowStepCreateRequest, AgentProfileRecord, FlowStepOnFailure } from "@/lib/types";

import styles from "./FlowStepsBuilder.module.css";

export interface FlowStepsBuilderProps {
  readonly steps: readonly AgentFlowStepCreateRequest[];
  readonly onStepsChange: (steps: readonly AgentFlowStepCreateRequest[]) => void;
  /** `null` when the server-side read of the agent profile list failed. */
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  readonly disabled?: boolean;
}

function blankStep(order: number): AgentFlowStepCreateRequest {
  return { step_key: "", step_order: order, agent_profile_id: "", on_failure: "abort" };
}

export function FlowStepsBuilder({
  steps,
  onStepsChange,
  agentProfiles,
  disabled = false,
}: FlowStepsBuilderProps) {
  function update(index: number, patch: Partial<AgentFlowStepCreateRequest>): void {
    onStepsChange(steps.map((step, i) => (i === index ? { ...step, ...patch } : step)));
  }

  function remove(index: number): void {
    onStepsChange(steps.filter((_, i) => i !== index));
  }

  function add(): void {
    onStepsChange([...steps, blankStep(steps.length)]);
  }

  return (
    <div className={styles.builder}>
      <h4 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.flows_steps_heading)}</h4>

      {agentProfiles === null && (
        <p className={styles.notice} role="status">
          {t(CONSOLE_MESSAGE_KEYS.flows_step_agent_profiles_load_failed)}
        </p>
      )}

      {steps.length === 0 && <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.flows_steps_empty)}</p>}

      {steps.map((step, index) => (
        // Steps have no stable id before creation, so the array index is the
        // only key available — acceptable here because rows are never
        // reordered independently of the list itself.
        <div key={index} className={styles.step}>
          <label>
            {t(CONSOLE_MESSAGE_KEYS.flows_step_key_label)}
            <input
              className={styles.input}
              value={step.step_key}
              disabled={disabled}
              onChange={(event) => update(index, { step_key: event.target.value })}
            />
          </label>

          <label>
            {t(CONSOLE_MESSAGE_KEYS.flows_step_order_label)}
            <input
              type="number"
              className={`${styles.input} ${styles.orderInput}`}
              value={step.step_order}
              disabled={disabled}
              onChange={(event) => update(index, { step_order: Number(event.target.value) })}
            />
          </label>

          <label>
            {t(CONSOLE_MESSAGE_KEYS.flows_step_agent_profile_label)}
            {agentProfiles === null ? (
              <input
                className={styles.input}
                value={step.agent_profile_id}
                disabled={disabled}
                onChange={(event) => update(index, { agent_profile_id: event.target.value })}
              />
            ) : (
              <select
                className={styles.select}
                value={step.agent_profile_id}
                disabled={disabled}
                onChange={(event) => update(index, { agent_profile_id: event.target.value })}
              >
                <option value="">{t(CONSOLE_MESSAGE_KEYS.flows_step_agent_profile_none)}</option>
                {agentProfiles.map((profile) => (
                  <option key={profile.id} value={profile.id}>
                    {profile.display_name}
                  </option>
                ))}
              </select>
            )}
          </label>

          <label>
            {t(CONSOLE_MESSAGE_KEYS.flows_step_on_failure_label)}
            <select
              className={styles.select}
              value={step.on_failure ?? "abort"}
              disabled={disabled}
              onChange={(event) => update(index, { on_failure: event.target.value as FlowStepOnFailure })}
            >
              <option value="abort">{t(CONSOLE_MESSAGE_KEYS.flows_on_failure_abort)}</option>
              <option value="continue">{t(CONSOLE_MESSAGE_KEYS.flows_on_failure_continue)}</option>
            </select>
          </label>

          <Button type="button" variant="ghost" size="sm" disabled={disabled} onClick={() => remove(index)}>
            {t(CONSOLE_MESSAGE_KEYS.flows_step_remove)}
          </Button>
        </div>
      ))}

      <Button type="button" variant="secondary" size="sm" disabled={disabled} onClick={add}>
        {t(CONSOLE_MESSAGE_KEYS.flows_step_add)}
      </Button>
    </div>
  );
}
