"use client";

// The OpenAPI import pipeline's console side (plan 12 §5, issue #237).
//
// ============================================================================
// JSON PARSING HAPPENS HERE, NOT ON THE SERVER
// ============================================================================
//
// The pasted text is parsed client-side before anything is sent. A parse
// failure is therefore a local, instant refusal (`skills_import_invalid_json`)
// rather than a round trip to the console's own route handler for a mistake
// the browser could already see. Once it parses, the PARSED VALUE is what gets
// sent as `document` — never the raw string — so the server never re-parses
// text it has to trust came from JSON in the first place.
//
// ============================================================================
// EVERYTHING ELSE — THE 300-OPERATION CAP, AN SSRF-BLOCKED HOST — IS MOIRA'S
// ============================================================================
//
// Those refusals arrive as Moira's own coded error, carried through
// `readConsoleApiFailure` like any other, and rendered the same way. This
// panel adds no second cap and no second host check of its own.

import { useId, useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Label } from "@/components/atoms/Label";
import { type ConsoleApiFailure, postJson } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { SkillImportResponse } from "@/lib/types";

import styles from "./SkillImportPanel.module.css";

export interface SkillImportPanelProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onImported?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "invalid_json" }
  | { readonly kind: "pending" }
  | { readonly kind: "imported"; readonly result: SkillImportResponse }
  | { readonly kind: "failed"; readonly failure: ConsoleApiFailure };

export function SkillImportPanel({ fetchImpl, onImported }: SkillImportPanelProps) {
  const [text, setText] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const fieldId = useId();

  const pending = phase.kind === "pending";

  function onFileChosen(file: File | undefined): void {
    if (file === undefined) return;
    void file.text().then((content) => setText(content));
  }

  async function submit(): Promise<void> {
    let document: unknown;
    try {
      document = JSON.parse(text);
    } catch {
      setPhase({ kind: "invalid_json" });
      return;
    }

    setPhase({ kind: "pending" });
    const result = await postJson<SkillImportResponse>(
      "/api/skills/import",
      { document },
      CONSOLE_MESSAGE_KEYS.skills_request_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setPhase({ kind: "imported", result: result.data });
    onImported?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.skills_import_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.skills_import_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.skills_import_intro)}</p>

      <div className={styles.field}>
        <Label htmlFor={fieldId}>{t(CONSOLE_MESSAGE_KEYS.skills_import_field_label)}</Label>
        <textarea
          id={fieldId}
          className={styles.textarea}
          value={text}
          disabled={pending}
          onChange={(event) => setText(event.target.value)}
        />
        <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.skills_import_field_hint)}</p>
        <input
          type="file"
          accept="application/json,.json"
          disabled={pending}
          onChange={(event) => onFileChosen(event.target.files?.[0])}
        />
      </div>

      <Button
        type="button"
        variant="primary"
        loading={pending}
        disabled={text.trim() === ""}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.skills_import_submit)}
      </Button>

      {phase.kind === "invalid_json" && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.skills_import_invalid_json)}
        </p>
      )}
      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.failure.messageKey, phase.failure.messageArgs, phase.failure.message)}
        </p>
      )}
      {phase.kind === "imported" && (
        <>
          <p className={styles.status} role="status" aria-live="polite">
            {t(CONSOLE_MESSAGE_KEYS.skills_import_success, { count: phase.result.imported_count })}
          </p>
          {phase.result.skills.length > 0 && (
            <>
              <h3 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.skills_import_results_heading)}</h3>
              <ul className={styles.results}>
                {phase.result.skills.map((skill) => (
                  <li key={skill.id}>{skill.display_name}</li>
                ))}
              </ul>
            </>
          )}
        </>
      )}
    </section>
  );
}
