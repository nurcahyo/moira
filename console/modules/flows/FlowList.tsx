"use client";

// Every authored flow: its steps count, edit (including replacing the whole
// step list), delete, and, on expand, its run history.
//
// Delete has a confirmation dialog and edit does not need one: flows have no
// enable/disable on this surface (`app/api/flows/[id]/route.ts`'s header), so a
// flow deletion really is the one-way door `DangerConfirmDialog` exists for.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, patchJson, sendDelete } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AgentFlowRecord, AgentFlowStepCreateRequest, AgentProfileRecord } from "@/lib/types";

import { FlowRunsPanel } from "./FlowRunsPanel";
import { FlowStepsBuilder } from "./FlowStepsBuilder";
import styles from "./FlowList.module.css";

export interface FlowListProps {
  readonly flows: readonly AgentFlowRecord[];
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onChanged?: () => void;
}

function flowUrl(id: string): string {
  return `/api/flows/${encodeURIComponent(id)}`;
}

function toStepCreate(flow: AgentFlowRecord): AgentFlowStepCreateRequest[] {
  return flow.steps.map((step) => ({
    step_key: step.step_key,
    step_order: step.step_order,
    agent_profile_id: step.agent_profile_id,
    on_failure: step.on_failure,
  }));
}

export function FlowList({ flows, agentProfiles, fetchImpl, onChanged }: FlowListProps) {
  const [failure, setFailure] = useState<ConsoleApiFailure | null>(null);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDisplayName, setEditDisplayName] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editSteps, setEditSteps] = useState<readonly AgentFlowStepCreateRequest[]>([]);
  const [editSaving, setEditSaving] = useState(false);
  const [savedId, setSavedId] = useState<string | null>(null);

  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);

  const [expandedId, setExpandedId] = useState<string | null>(null);

  function startEdit(flow: AgentFlowRecord): void {
    setEditingId(flow.id);
    setEditDisplayName(flow.display_name);
    setEditDescription(flow.description ?? "");
    setEditSteps(toStepCreate(flow));
    setFailure(null);
  }

  async function saveEdit(id: string): Promise<void> {
    setEditSaving(true);
    setFailure(null);
    const result = await patchJson<AgentFlowRecord>(
      flowUrl(id),
      {
        display_name: editDisplayName.trim(),
        description: editDescription.trim() === "" ? null : editDescription.trim(),
        steps: editSteps,
      },
      CONSOLE_MESSAGE_KEYS.flows_request_failed,
      fetchImpl,
    );
    setEditSaving(false);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    setEditingId(null);
    setSavedId(id);
    onChanged?.();
  }

  async function confirmDelete(): Promise<void> {
    if (deletingId === null) return;
    setDeleteBusy(true);
    const result = await sendDelete<void>(
      flowUrl(deletingId),
      CONSOLE_MESSAGE_KEYS.flows_request_failed,
      fetchImpl,
    );
    setDeleteBusy(false);
    setDeletingId(null);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    onChanged?.();
  }

  const deletingFlow = flows.find((flow) => flow.id === deletingId) ?? null;

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.flows_list_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.flows_list_heading)}</h2>

      {flows.length === 0 ? (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.flows_list_empty)}</p>
      ) : (
        flows.map((flow) => {
          const editing = editingId === flow.id;
          const expanded = expandedId === flow.id;
          return (
            <article key={flow.id} className={styles.flow}>
              <header className={styles.flowHeader}>
                <h3 className={styles.flowName}>{flow.display_name}</h3>
                <Badge tone={flow.status === "active" ? "success" : "neutral"}>
                  {t(
                    flow.status === "active"
                      ? CONSOLE_MESSAGE_KEYS.flows_status_active
                      : CONSOLE_MESSAGE_KEYS.flows_status_inactive,
                  )}
                </Badge>
              </header>
              <p className={styles.meta}>{flow.flow_key}</p>
              {flow.description !== null && flow.description !== undefined && (
                <p className={styles.meta}>{flow.description}</p>
              )}
              <p className={styles.meta}>{flow.steps.length}</p>

              <div className={styles.actions}>
                <Button type="button" variant="ghost" size="sm" onClick={() => startEdit(flow)}>
                  {t(CONSOLE_MESSAGE_KEYS.flows_edit)}
                </Button>
                <Button type="button" variant="danger" size="sm" onClick={() => setDeletingId(flow.id)}>
                  {t(CONSOLE_MESSAGE_KEYS.flows_delete)}
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => setExpandedId(expanded ? null : flow.id)}
                >
                  {t(expanded ? CONSOLE_MESSAGE_KEYS.flows_collapse : CONSOLE_MESSAGE_KEYS.flows_expand)}
                </Button>
              </div>

              {editing && (
                <div className={styles.editForm}>
                  <FormField
                    label={t(CONSOLE_MESSAGE_KEYS.flows_field_display_name_label)}
                    required
                    inputProps={{
                      value: editDisplayName,
                      disabled: editSaving,
                      onChange: (event) => setEditDisplayName(event.target.value),
                    }}
                  />
                  <FormField
                    label={t(CONSOLE_MESSAGE_KEYS.flows_field_description_label)}
                    inputProps={{
                      value: editDescription,
                      disabled: editSaving,
                      onChange: (event) => setEditDescription(event.target.value),
                    }}
                  />
                  <FlowStepsBuilder
                    steps={editSteps}
                    onStepsChange={setEditSteps}
                    agentProfiles={agentProfiles}
                    disabled={editSaving}
                  />
                  <div className={styles.actions}>
                    <Button
                      type="button"
                      variant="primary"
                      loading={editSaving}
                      disabled={editDisplayName.trim() === ""}
                      onClick={() => {
                        void saveEdit(flow.id);
                      }}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.flows_edit_save)}
                    </Button>
                    <Button
                      type="button"
                      variant="secondary"
                      disabled={editSaving}
                      onClick={() => setEditingId(null)}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.flows_edit_cancel)}
                    </Button>
                  </div>
                </div>
              )}

              {!editing && savedId === flow.id && (
                <p className={styles.status} role="status" aria-live="polite">
                  {t(CONSOLE_MESSAGE_KEYS.flows_edit_saved)}
                </p>
              )}

              {expanded && <FlowRunsPanel flowId={flow.id} {...(fetchImpl === undefined ? {} : { fetchImpl })} />}
            </article>
          );
        })
      )}

      {failure !== null && (
        <p className={styles.problem} role="alert">
          {t(failure.messageKey, failure.messageArgs, failure.message)}
        </p>
      )}

      <DangerConfirmDialog
        open={deletingFlow !== null}
        title={t(CONSOLE_MESSAGE_KEYS.flows_delete_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.flows_delete_confirm_body)}
        confirmLabel={t(CONSOLE_MESSAGE_KEYS.flows_delete_confirm_action)}
        busy={deleteBusy}
        onConfirm={() => {
          void confirmDelete();
        }}
        onCancel={() => setDeletingId(null)}
      />
    </section>
  );
}
