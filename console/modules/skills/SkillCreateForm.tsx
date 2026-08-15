"use client";

// Author one skill by hand.
//
// The long way round the OpenAPI import shortcut, kept because an operator
// wiring up a single guard or a small tool does not want to hand-craft a
// one-operation OpenAPI document to get it. `params_schema` is deliberately
// not a field here — see `app/api/skills/route.ts`'s header for why a
// JSON-Schema authoring UI is out of scope for this path.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { type ConsoleApiFailure, postJson } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { SkillKind, SkillRecord } from "@/lib/types";

import styles from "./SkillCreateForm.module.css";

export interface SkillCreateFormProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onCreated?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "created" }
  | { readonly kind: "failed"; readonly failure: ConsoleApiFailure };

export function SkillCreateForm({ fetchImpl, onCreated }: SkillCreateFormProps) {
  const [skillKey, setSkillKey] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [kind, setKind] = useState<SkillKind>("tool");
  const [description, setDescription] = useState("");
  const [tags, setTags] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    setPhase({ kind: "pending" });
    const result = await postJson<SkillRecord>(
      "/api/skills",
      {
        skill_key: skillKey.trim(),
        display_name: displayName.trim(),
        kind,
        description: description.trim() === "" ? null : description.trim(),
        tags: tags
          .split(",")
          .map((tag) => tag.trim())
          .filter((tag) => tag !== ""),
      },
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setSkillKey("");
    setDisplayName("");
    setDescription("");
    setTags("");
    setPhase({ kind: "created" });
    onCreated?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.skills_create_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.skills_create_heading)}</h2>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_field_skill_key_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.skills_field_skill_key_hint)}
        required
        inputProps={{
          value: skillKey,
          disabled: pending,
          onChange: (event) => setSkillKey(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_field_display_name_label)}
        required
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <label className={styles.selectRow}>
        {t(CONSOLE_MESSAGE_KEYS.skills_field_kind_label)}
        <select
          className={styles.select}
          value={kind}
          disabled={pending}
          onChange={(event) => setKind(event.target.value as SkillKind)}
        >
          <option value="tool">{t(CONSOLE_MESSAGE_KEYS.skills_kind_tool)}</option>
          <option value="guard">{t(CONSOLE_MESSAGE_KEYS.skills_kind_guard)}</option>
        </select>
      </label>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_field_description_label)}
        inputProps={{
          value: description,
          disabled: pending,
          onChange: (event) => setDescription(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.skills_field_tags_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.skills_field_tags_hint)}
        inputProps={{
          value: tags,
          disabled: pending,
          onChange: (event) => setTags(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={skillKey.trim() === "" || displayName.trim() === ""}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.skills_create_submit)}
      </Button>

      {phase.kind === "created" && (
        <p className={styles.status} role="status" aria-live="polite">
          {t(CONSOLE_MESSAGE_KEYS.skills_create_success)}
        </p>
      )}
      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
    </section>
  );
}
