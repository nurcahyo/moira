"use client";

// One skill's HTTP executor: view, edit, delete.
//
// ============================================================================
// FETCHED ON DEMAND, NOT EMBEDDED IN THE SKILL LIST
// ============================================================================
//
// `SkillRecord` carries no executor reference — the two are separate resources
// joined only by `skill_id` — and most skills on a real deployment have none at
// all (only the OpenAPI import pipeline creates one; there is no
// `POST .../executor`). Fetching every executor up front to answer "does this
// row have one" would be an N+1 read for a fact the row's own expand toggle
// already exists to defer. `SkillList` mounts this panel only when its row is
// expanded, and this panel fetches exactly once on mount.
//
// ============================================================================
// A 404 HERE IS THE NORMAL CASE, NOT A FAILURE
// ============================================================================
//
// A hand-authored skill has no executor and cannot get one from this console —
// see `app/api/skills/[id]/executor/route.ts`'s header. So `notFound` is its
// own phase with calm copy, distinct from `loadFailed`, which is a genuine
// problem (network, 5xx, an unexpected 4xx).

import { useEffect, useState } from "react";

import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, getJson, patchJson, sendDelete } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { SkillExecutorMethod, SkillHttpExecutorRecord } from "@/lib/types";

import styles from "./SkillExecutorPanel.module.css";

export interface SkillExecutorPanelProps {
  readonly skillId: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

type Phase =
  | { readonly kind: "loading" }
  | { readonly kind: "not_found" }
  | { readonly kind: "load_failed"; readonly failure: ConsoleApiFailure }
  | { readonly kind: "loaded"; readonly executor: SkillHttpExecutorRecord };

const METHODS: readonly SkillExecutorMethod[] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

function executorUrl(skillId: string): string {
  return `/api/skills/${encodeURIComponent(skillId)}/executor`;
}

export function SkillExecutorPanel({ skillId, fetchImpl }: SkillExecutorPanelProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [method, setMethod] = useState<SkillExecutorMethod>("GET");
  const [urlTemplate, setUrlTemplate] = useState("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [credentialId, setCredentialId] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveFailure, setSaveFailure] = useState<ConsoleApiFailure | null>(null);
  const [saved, setSaved] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleted, setDeleted] = useState(false);

  useEffect(() => {
    let cancelled = false;
    async function load(): Promise<void> {
      const result = await getJson<SkillHttpExecutorRecord>(
        executorUrl(skillId),
        CONSOLE_MESSAGE_KEYS.skills_executor_load_failed,
        fetchImpl,
      );
      if (cancelled) return;
      if (!result.ok) {
        if (result.failure.status === 404) {
          setPhase({ kind: "not_found" });
          return;
        }
        setPhase({ kind: "load_failed", failure: result.failure });
        return;
      }
      setPhase({ kind: "loaded", executor: result.data });
      setMethod(result.data.method);
      setUrlTemplate(result.data.url_template);
      setTimeoutMs(String(result.data.timeout_ms));
      setCredentialId(result.data.credential_id ?? "");
    }
    void load();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [skillId]);

  async function save(): Promise<void> {
    setSaving(true);
    setSaveFailure(null);
    setSaved(false);
    const timeout = Number(timeoutMs);
    const result = await patchJson<SkillHttpExecutorRecord>(
      executorUrl(skillId),
      {
        method,
        url_template: urlTemplate.trim(),
        timeout_ms: Number.isFinite(timeout) ? timeout : undefined,
        credential_id: credentialId.trim() === "" ? null : credentialId.trim(),
      },
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    setSaving(false);
    if (!result.ok) {
      setSaveFailure(result.failure);
      return;
    }
    setPhase({ kind: "loaded", executor: result.data });
    setSaved(true);
  }

  async function confirmDelete(): Promise<void> {
    setDeleting(true);
    const result = await sendDelete<void>(
      executorUrl(skillId),
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    setDeleting(false);
    setConfirmingDelete(false);
    if (!result.ok) {
      setSaveFailure(result.failure);
      return;
    }
    setDeleted(true);
    setPhase({ kind: "not_found" });
  }

  if (phase.kind === "loading") {
    return (
      <div className={styles.panel}>
        <p className={styles.status} role="status">
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_loading)}
        </p>
      </div>
    );
  }

  if (phase.kind === "not_found") {
    return (
      <div className={styles.panel}>
        <p className={styles.status}>
          {deleted
            ? t(CONSOLE_MESSAGE_KEYS.skills_executor_deleted)
            : t(CONSOLE_MESSAGE_KEYS.skills_executor_none)}
        </p>
      </div>
    );
  }

  if (phase.kind === "load_failed") {
    return (
      <div className={styles.panel}>
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      </div>
    );
  }

  return (
    <div className={styles.panel}>
      <h4 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.skills_executor_heading)}</h4>

      <p className={styles.readOnlyRow}>
        <span className={styles.readOnlyLabel}>
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_allowed_host_label)}:
        </span>
        <span className={styles.readOnlyValue}>{phase.executor.allowed_host}</span>
      </p>

      <label className={styles.readOnlyRow}>
        <span className={styles.readOnlyLabel}>
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_method_label)}
        </span>
        <select
          className={styles.select}
          value={method}
          disabled={saving}
          onChange={(event) => setMethod(event.target.value as SkillExecutorMethod)}
        >
          {METHODS.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
      </label>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_executor_url_label)}
        inputProps={{
          value: urlTemplate,
          disabled: saving,
          onChange: (event) => setUrlTemplate(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_executor_timeout_label)}
        inputProps={{
          value: timeoutMs,
          type: "number",
          min: 1,
          disabled: saving,
          onChange: (event) => setTimeoutMs(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_executor_key_row_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.skills_executor_key_row_hint)}
        inputProps={{
          value: credentialId,
          disabled: saving,
          onChange: (event) => setCredentialId(event.target.value),
        }}
      />

      <div className={styles.actions}>
        <Button
          type="button"
          variant="secondary"
          loading={saving}
          disabled={urlTemplate.trim() === "" || timeoutMs.trim() === ""}
          onClick={() => {
            void save();
          }}
        >
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_save)}
        </Button>
        <Button
          type="button"
          variant="danger"
          size="sm"
          onClick={() => setConfirmingDelete(true)}
        >
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_delete)}
        </Button>
      </div>

      {saved && (
        <p className={styles.status} role="status" aria-live="polite">
          {t(CONSOLE_MESSAGE_KEYS.skills_executor_saved)}
        </p>
      )}
      {saveFailure !== null && (
        <p className={styles.problem} role="alert">
          {t(saveFailure.messageKey, saveFailure.messageArgs, saveFailure.message)}
        </p>
      )}

      <DangerConfirmDialog
        open={confirmingDelete}
        title={t(CONSOLE_MESSAGE_KEYS.skills_executor_delete_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.skills_executor_delete_confirm_body)}
        confirmLabel={t(CONSOLE_MESSAGE_KEYS.skills_executor_delete_confirm_action)}
        busy={deleting}
        onConfirm={() => {
          void confirmDelete();
        }}
        onCancel={() => setConfirmingDelete(false)}
      />
    </div>
  );
}
