"use client";

// Every authored eval suite, with edit/delete and, on expand, its cases and
// run history.
//
// Delete has a confirmation dialog and edit does not need one: eval suites
// have no enable/disable on this surface (`app/api/evals/suites/[id]/route.ts`'s
// header), so a suite deletion really is the one-way door
// `DangerConfirmDialog` exists for — unlike the skills family, there is no undo.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, patchJson, sendDelete } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { EvalSuiteRecord } from "@/lib/types";

import { EvalCasesPanel } from "./EvalCasesPanel";
import { EvalRunsPanel } from "./EvalRunsPanel";
import styles from "./EvalSuiteList.module.css";

export interface EvalSuiteListProps {
  readonly suites: readonly EvalSuiteRecord[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onChanged?: () => void;
}

function suiteUrl(id: string): string {
  return `/api/evals/suites/${encodeURIComponent(id)}`;
}

export function EvalSuiteList({ suites, fetchImpl, onChanged }: EvalSuiteListProps) {
  const [failure, setFailure] = useState<ConsoleApiFailure | null>(null);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDisplayName, setEditDisplayName] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editSaving, setEditSaving] = useState(false);
  const [savedId, setSavedId] = useState<string | null>(null);

  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);

  const [expandedId, setExpandedId] = useState<string | null>(null);

  function startEdit(suite: EvalSuiteRecord): void {
    setEditingId(suite.id);
    setEditDisplayName(suite.display_name);
    setEditDescription(suite.description ?? "");
    setFailure(null);
  }

  async function saveEdit(id: string): Promise<void> {
    setEditSaving(true);
    setFailure(null);
    const result = await patchJson<EvalSuiteRecord>(
      suiteUrl(id),
      {
        display_name: editDisplayName.trim(),
        description: editDescription.trim() === "" ? null : editDescription.trim(),
      },
      CONSOLE_MESSAGE_KEYS.evals_request_failed,
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
      suiteUrl(deletingId),
      CONSOLE_MESSAGE_KEYS.evals_request_failed,
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

  const deletingSuite = suites.find((suite) => suite.id === deletingId) ?? null;

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.evals_suites_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.evals_suites_heading)}</h2>

      {suites.length === 0 ? (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.evals_suites_empty)}</p>
      ) : (
        suites.map((suite) => {
          const editing = editingId === suite.id;
          const expanded = expandedId === suite.id;
          return (
            <article key={suite.id} className={styles.suite}>
              <header className={styles.suiteHeader}>
                <h3 className={styles.suiteName}>{suite.display_name}</h3>
                <Badge tone={suite.status === "active" ? "success" : "neutral"}>
                  {t(
                    suite.status === "active"
                      ? CONSOLE_MESSAGE_KEYS.evals_status_active
                      : CONSOLE_MESSAGE_KEYS.evals_status_inactive,
                  )}
                </Badge>
              </header>
              <p className={styles.meta}>{suite.suite_key}</p>
              {suite.description !== null && suite.description !== undefined && (
                <p className={styles.meta}>{suite.description}</p>
              )}

              <div className={styles.actions}>
                <Button type="button" variant="ghost" size="sm" onClick={() => startEdit(suite)}>
                  {t(CONSOLE_MESSAGE_KEYS.evals_edit)}
                </Button>
                <Button type="button" variant="danger" size="sm" onClick={() => setDeletingId(suite.id)}>
                  {t(CONSOLE_MESSAGE_KEYS.evals_delete)}
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => setExpandedId(expanded ? null : suite.id)}
                >
                  {t(expanded ? CONSOLE_MESSAGE_KEYS.evals_collapse : CONSOLE_MESSAGE_KEYS.evals_expand)}
                </Button>
              </div>

              {editing && (
                <div className={styles.editForm}>
                  <FormField
                    label={t(CONSOLE_MESSAGE_KEYS.evals_field_display_name_label)}
                    required
                    inputProps={{
                      value: editDisplayName,
                      disabled: editSaving,
                      onChange: (event) => setEditDisplayName(event.target.value),
                    }}
                  />
                  <FormField
                    label={t(CONSOLE_MESSAGE_KEYS.evals_field_description_label)}
                    inputProps={{
                      value: editDescription,
                      disabled: editSaving,
                      onChange: (event) => setEditDescription(event.target.value),
                    }}
                  />
                  <div className={styles.actions}>
                    <Button
                      type="button"
                      variant="primary"
                      loading={editSaving}
                      disabled={editDisplayName.trim() === ""}
                      onClick={() => {
                        void saveEdit(suite.id);
                      }}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.evals_edit_save)}
                    </Button>
                    <Button
                      type="button"
                      variant="secondary"
                      disabled={editSaving}
                      onClick={() => setEditingId(null)}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.evals_edit_cancel)}
                    </Button>
                  </div>
                </div>
              )}

              {!editing && savedId === suite.id && (
                <p className={styles.status} role="status" aria-live="polite">
                  {t(CONSOLE_MESSAGE_KEYS.evals_edit_saved)}
                </p>
              )}

              {expanded && (
                <>
                  <EvalCasesPanel suiteId={suite.id} {...(fetchImpl === undefined ? {} : { fetchImpl })} />
                  <EvalRunsPanel suiteId={suite.id} {...(fetchImpl === undefined ? {} : { fetchImpl })} />
                </>
              )}
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
        open={deletingSuite !== null}
        title={t(CONSOLE_MESSAGE_KEYS.evals_delete_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.evals_delete_confirm_body)}
        confirmLabel={t(CONSOLE_MESSAGE_KEYS.evals_delete_confirm_action)}
        busy={deleteBusy}
        onConfirm={() => {
          void confirmDelete();
        }}
        onCancel={() => setDeletingId(null)}
      />
    </section>
  );
}
