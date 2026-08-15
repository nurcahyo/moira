"use client";

// Every registered skill: draft/enabled/disabled, tool/guard, its tags, and the
// four controls a row offers — enable/disable, edit, delete, and (via
// `SkillExecutorPanel`) its HTTP executor.
//
// ============================================================================
// BULK-ENABLE IS A SELECTION OVER THIS LIST, NOT A SEPARATE SCREEN
// ============================================================================
//
// The reason a 40-operation OpenAPI import does not become 40 clicks (plan 12
// §5 decision 22): every row gets a checkbox, and "Enable selected skills"
// calls `POST /api/skills/bulk-enable` once with every checked id. A skill
// already enabled offers no checkbox — there is nothing to add it to.
//
// ============================================================================
// DELETE HAS A CONFIRMATION DIALOG; ENABLE/DISABLE DOES NOT
// ============================================================================
//
// Enable/disable is the reversible toggle (draft/disabled -> enabled and back),
// so a double-click costs nothing. Delete removes the skill and its executor
// with no undo from this screen, which is what `DangerConfirmDialog` exists
// for.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, patchJson, postJson, sendDelete } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { SkillKind, SkillRecord, SkillStatus } from "@/lib/types";

import { SkillExecutorPanel } from "./SkillExecutorPanel";
import styles from "./SkillList.module.css";

export interface SkillListProps {
  readonly skills: readonly SkillRecord[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onChanged?: () => void;
}

function skillUrl(id: string, suffix = ""): string {
  return `/api/skills/${encodeURIComponent(id)}${suffix}`;
}

/** Badge text for a skill's `kind`. */
export function kindKey(kind: SkillKind): string {
  return kind === "tool" ? CONSOLE_MESSAGE_KEYS.skills_kind_tool : CONSOLE_MESSAGE_KEYS.skills_kind_guard;
}

/** Badge text for a skill's `status`. */
export function statusKey(status: SkillStatus): string {
  if (status === "enabled") return CONSOLE_MESSAGE_KEYS.skills_status_enabled;
  if (status === "disabled") return CONSOLE_MESSAGE_KEYS.skills_status_disabled;
  return CONSOLE_MESSAGE_KEYS.skills_status_draft;
}

export function SkillList({ skills, fetchImpl, onChanged }: SkillListProps) {
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [busyId, setBusyId] = useState<string | null>(null);
  const [failure, setFailure] = useState<ConsoleApiFailure | null>(null);

  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDisplayName, setEditDisplayName] = useState("");
  const [editDescription, setEditDescription] = useState("");
  const [editTags, setEditTags] = useState("");
  const [editSaving, setEditSaving] = useState(false);

  const [savedId, setSavedId] = useState<string | null>(null);

  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);

  const [expandedId, setExpandedId] = useState<string | null>(null);

  const [bulkBusy, setBulkBusy] = useState(false);
  const [bulkDone, setBulkDone] = useState(false);

  function toggleSelected(id: string): void {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function toggleEnabled(skill: SkillRecord): Promise<void> {
    setBusyId(skill.id);
    setFailure(null);
    const suffix = skill.status === "enabled" ? "/disable" : "/enable";
    const result = await postJson<SkillRecord>(
      skillUrl(skill.id, suffix),
      {},
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    setBusyId(null);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    onChanged?.();
  }

  function startEdit(skill: SkillRecord): void {
    setEditingId(skill.id);
    setEditDisplayName(skill.display_name);
    setEditDescription(skill.description ?? "");
    setEditTags(skill.tags.join(", "));
    setFailure(null);
  }

  async function saveEdit(id: string): Promise<void> {
    setEditSaving(true);
    setFailure(null);
    const result = await patchJson<SkillRecord>(
      skillUrl(id),
      {
        display_name: editDisplayName.trim(),
        description: editDescription.trim() === "" ? null : editDescription.trim(),
        tags: editTags
          .split(",")
          .map((tag) => tag.trim())
          .filter((tag) => tag !== ""),
      },
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
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
      skillUrl(deletingId),
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
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

  async function bulkEnable(): Promise<void> {
    setBulkBusy(true);
    setFailure(null);
    setBulkDone(false);
    const result = await postJson<{ data: SkillRecord[] }>(
      "/api/skills/bulk-enable",
      { skill_ids: [...selected] },
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    setBulkBusy(false);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    setSelected(new Set());
    setBulkDone(true);
    onChanged?.();
  }

  const deletingSkill = skills.find((skill) => skill.id === deletingId) ?? null;

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.skills_list_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.skills_list_heading)}</h2>

      {skills.length === 0 ? (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.skills_list_empty)}</p>
      ) : (
        <>
          <div className={styles.bulkBar}>
            <Button
              type="button"
              variant="secondary"
              loading={bulkBusy}
              disabled={selected.size === 0}
              onClick={() => {
                void bulkEnable();
              }}
            >
              {t(CONSOLE_MESSAGE_KEYS.skills_bulk_enable_button)}
            </Button>
            {selected.size === 0 && (
              <span className={styles.status}>
                {t(CONSOLE_MESSAGE_KEYS.skills_bulk_enable_none_selected)}
              </span>
            )}
            {bulkDone && (
              <span className={styles.status} role="status" aria-live="polite">
                {t(CONSOLE_MESSAGE_KEYS.skills_bulk_enable_done)}
              </span>
            )}
          </div>

          {skills.map((skill) => {
            const editing = editingId === skill.id;
            const expanded = expandedId === skill.id;
            return (
              <article key={skill.id} className={styles.skill}>
                <header className={styles.skillHeader}>
                  {skill.status !== "enabled" && (
                    <input
                      type="checkbox"
                      checked={selected.has(skill.id)}
                      aria-label={t(CONSOLE_MESSAGE_KEYS.skills_select_for_bulk_enable)}
                      onChange={() => toggleSelected(skill.id)}
                    />
                  )}
                  <h3 className={styles.skillKey}>{skill.display_name}</h3>
                  <Badge tone="neutral">{t(kindKey(skill.kind))}</Badge>
                  <Badge tone={skill.status === "enabled" ? "success" : "neutral"}>
                    {t(statusKey(skill.status))}
                  </Badge>
                </header>

                <p className={styles.meta}>{skill.skill_key}</p>
                <p className={styles.meta}>
                  {skill.description ?? t(CONSOLE_MESSAGE_KEYS.skills_no_description)}
                </p>
                <div className={styles.tags}>
                  {skill.tags.length === 0 ? (
                    <span className={styles.meta}>{t(CONSOLE_MESSAGE_KEYS.skills_tags_none)}</span>
                  ) : (
                    skill.tags.map((tag) => (
                      <Badge key={tag} tone="info">
                        {tag}
                      </Badge>
                    ))
                  )}
                </div>

                <div className={styles.actions}>
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    loading={busyId === skill.id}
                    onClick={() => {
                      void toggleEnabled(skill);
                    }}
                  >
                    {t(skill.status === "enabled" ? CONSOLE_MESSAGE_KEYS.skills_disable : CONSOLE_MESSAGE_KEYS.skills_enable)}
                  </Button>
                  <Button type="button" variant="ghost" size="sm" onClick={() => startEdit(skill)}>
                    {t(CONSOLE_MESSAGE_KEYS.skills_edit)}
                  </Button>
                  <Button
                    type="button"
                    variant="danger"
                    size="sm"
                    onClick={() => setDeletingId(skill.id)}
                  >
                    {t(CONSOLE_MESSAGE_KEYS.skills_delete)}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setExpandedId(expanded ? null : skill.id)}
                  >
                    {t(expanded ? CONSOLE_MESSAGE_KEYS.skills_executor_hide : CONSOLE_MESSAGE_KEYS.skills_executor_show)}
                  </Button>
                </div>

                {editing && (
                  <div className={styles.editForm}>
                    <FormField
                      label={t(CONSOLE_MESSAGE_KEYS.skills_field_display_name_label)}
                      required
                      inputProps={{
                        value: editDisplayName,
                        disabled: editSaving,
                        onChange: (event) => setEditDisplayName(event.target.value),
                      }}
                    />
                    <FormField
                      label={t(CONSOLE_MESSAGE_KEYS.skills_field_description_label)}
                      inputProps={{
                        value: editDescription,
                        disabled: editSaving,
                        onChange: (event) => setEditDescription(event.target.value),
                      }}
                    />
                    <FormField
                      label={t(CONSOLE_MESSAGE_KEYS.skills_field_tags_label)}
                      hint={t(CONSOLE_MESSAGE_KEYS.skills_field_tags_hint)}
                      inputProps={{
                        value: editTags,
                        disabled: editSaving,
                        onChange: (event) => setEditTags(event.target.value),
                      }}
                    />
                    <div className={styles.actions}>
                      <Button
                        type="button"
                        variant="primary"
                        loading={editSaving}
                        disabled={editDisplayName.trim() === ""}
                        onClick={() => {
                          void saveEdit(skill.id);
                        }}
                      >
                        {t(CONSOLE_MESSAGE_KEYS.skills_edit_save)}
                      </Button>
                      <Button
                        type="button"
                        variant="secondary"
                        disabled={editSaving}
                        onClick={() => setEditingId(null)}
                      >
                        {t(CONSOLE_MESSAGE_KEYS.skills_edit_cancel)}
                      </Button>
                    </div>
                  </div>
                )}

                {!editing && savedId === skill.id && (
                  <p className={styles.status} role="status" aria-live="polite">
                    {t(CONSOLE_MESSAGE_KEYS.skills_edit_saved)}
                  </p>
                )}

                {expanded && <SkillExecutorPanel skillId={skill.id} {...(fetchImpl === undefined ? {} : { fetchImpl })} />}
              </article>
            );
          })}
        </>
      )}

      {failure !== null && (
        <p className={styles.problem} role="alert">
          {t(failure.messageKey, failure.messageArgs, failure.message)}
        </p>
      )}

      <DangerConfirmDialog
        open={deletingSkill !== null}
        title={t(CONSOLE_MESSAGE_KEYS.skills_delete_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.skills_delete_confirm_body)}
        confirmLabel={t(CONSOLE_MESSAGE_KEYS.skills_delete_confirm_action)}
        busy={deleteBusy}
        onConfirm={() => {
          void confirmDelete();
        }}
        onCancel={() => setDeletingId(null)}
      />
    </section>
  );
}
