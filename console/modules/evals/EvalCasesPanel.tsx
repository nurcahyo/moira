"use client";

// One suite's cases: list, author, delete. Fetched on demand when the suite
// row expands — same posture as `SkillExecutorPanel` for the same reason: most
// suites are not being looked at on any given render, and a page-level fetch
// of every suite's cases would be an N+1 read for a fact the expand toggle
// already exists to defer.
//
// `EvalCaseRecord` carries no `version`, so there is no edit surface here —
// only add and delete, mirroring `deleteEvalCase`'s own lack of `If-Match`.

import { useEffect, useId, useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Label } from "@/components/atoms/Label";
import { type ConsoleApiFailure, getJson, postJson, sendDelete } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { EvalCaseRecord, GradingKind, ListResponse } from "@/lib/types";

import styles from "./EvalCasesPanel.module.css";

export interface EvalCasesPanelProps {
  readonly suiteId: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

const GRADING_KIND_KEYS: Record<GradingKind, string> = {
  exact_match: CONSOLE_MESSAGE_KEYS.evals_grading_exact_match,
  contains: CONSOLE_MESSAGE_KEYS.evals_grading_contains,
  schema_valid: CONSOLE_MESSAGE_KEYS.evals_grading_schema_valid,
};

function casesUrl(suiteId: string, suffix = ""): string {
  return `/api/evals/suites/${encodeURIComponent(suiteId)}/cases${suffix}`;
}

export function EvalCasesPanel({ suiteId, fetchImpl }: EvalCasesPanelProps) {
  const [cases, setCases] = useState<readonly EvalCaseRecord[] | null>(null);
  const [loadFailure, setLoadFailure] = useState<ConsoleApiFailure | null>(null);

  const [inputText, setInputText] = useState("");
  const [expectedText, setExpectedText] = useState("");
  const [gradingKind, setGradingKind] = useState<GradingKind>("exact_match");
  const [adding, setAdding] = useState(false);
  const [addFailure, setAddFailure] = useState<ConsoleApiFailure | null>(null);
  const [invalidJson, setInvalidJson] = useState(false);
  const [justAdded, setJustAdded] = useState(false);

  const inputId = useId();
  const expectedId = useId();

  /** Manual reload, called after a case is added or removed. */
  async function reload(): Promise<void> {
    const result = await getJson<ListResponse<EvalCaseRecord>>(
      casesUrl(suiteId),
      CONSOLE_MESSAGE_KEYS.evals_load_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setLoadFailure(result.failure);
      return;
    }
    setLoadFailure(null);
    setCases(result.data.data);
  }

  // The initial, on-expand fetch. Defined inline with its own `cancelled` guard
  // — rather than calling `reload` above — so a suite switched away from before
  // the response arrives never applies a stale result to this component's state.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const result = await getJson<ListResponse<EvalCaseRecord>>(
        casesUrl(suiteId),
        CONSOLE_MESSAGE_KEYS.evals_load_failed,
        fetchImpl,
      );
      if (cancelled) return;
      if (!result.ok) {
        setLoadFailure(result.failure);
        return;
      }
      setLoadFailure(null);
      setCases(result.data.data);
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [suiteId]);

  async function addCase(): Promise<void> {
    let input: unknown;
    let expected: unknown;
    try {
      input = JSON.parse(inputText);
      expected = JSON.parse(expectedText);
    } catch {
      setInvalidJson(true);
      return;
    }
    setInvalidJson(false);
    setAdding(true);
    setAddFailure(null);
    setJustAdded(false);
    const result = await postJson<EvalCaseRecord>(
      casesUrl(suiteId),
      { input, expected, grading_kind: gradingKind },
      CONSOLE_MESSAGE_KEYS.evals_request_failed,
      fetchImpl,
    );
    setAdding(false);
    if (!result.ok) {
      setAddFailure(result.failure);
      return;
    }
    setInputText("");
    setExpectedText("");
    setJustAdded(true);
    await reload();
  }

  async function removeCase(caseId: string): Promise<void> {
    const result = await sendDelete<void>(
      casesUrl(suiteId, `/${encodeURIComponent(caseId)}`),
      CONSOLE_MESSAGE_KEYS.evals_request_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setAddFailure(result.failure);
      return;
    }
    await reload();
  }

  return (
    <div className={styles.panel}>
      <h4 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.evals_cases_heading)}</h4>

      {loadFailure !== null && (
        <p className={styles.problem} role="alert">
          {t(loadFailure.messageKey, loadFailure.messageArgs, loadFailure.message)}
        </p>
      )}

      {cases !== null && cases.length === 0 && (
        <p className={styles.status}>{t(CONSOLE_MESSAGE_KEYS.evals_cases_empty)}</p>
      )}

      {cases !== null && cases.length > 0 && (
        <ul className={styles.rows}>
          {cases.map((testCase) => (
            <li key={testCase.id} className={styles.row}>
              <span>{JSON.stringify(testCase.input)}</span>
              <span>→</span>
              <span>{JSON.stringify(testCase.expected)}</span>
              <span>{t(GRADING_KIND_KEYS[testCase.grading_kind])}</span>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => {
                  void removeCase(testCase.id);
                }}
              >
                {t(CONSOLE_MESSAGE_KEYS.evals_case_delete)}
              </Button>
            </li>
          ))}
        </ul>
      )}

      <div>
        <Label htmlFor={inputId}>{t(CONSOLE_MESSAGE_KEYS.evals_case_input_label)}</Label>
        <textarea
          id={inputId}
          className={styles.textarea}
          value={inputText}
          disabled={adding}
          onChange={(event) => setInputText(event.target.value)}
        />
        <p className={styles.status}>{t(CONSOLE_MESSAGE_KEYS.evals_case_input_hint)}</p>
      </div>
      <div>
        <Label htmlFor={expectedId}>{t(CONSOLE_MESSAGE_KEYS.evals_case_expected_label)}</Label>
        <textarea
          id={expectedId}
          className={styles.textarea}
          value={expectedText}
          disabled={adding}
          onChange={(event) => setExpectedText(event.target.value)}
        />
        <p className={styles.status}>{t(CONSOLE_MESSAGE_KEYS.evals_case_expected_hint)}</p>
      </div>
      <label>
        {t(CONSOLE_MESSAGE_KEYS.evals_case_grading_label)}
        <select
          className={styles.select}
          value={gradingKind}
          disabled={adding}
          onChange={(event) => setGradingKind(event.target.value as GradingKind)}
        >
          <option value="exact_match">{t(CONSOLE_MESSAGE_KEYS.evals_grading_exact_match)}</option>
          <option value="contains">{t(CONSOLE_MESSAGE_KEYS.evals_grading_contains)}</option>
          <option value="schema_valid">{t(CONSOLE_MESSAGE_KEYS.evals_grading_schema_valid)}</option>
        </select>
      </label>
      <Button
        type="button"
        variant="secondary"
        loading={adding}
        disabled={inputText.trim() === "" || expectedText.trim() === ""}
        onClick={() => {
          void addCase();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.evals_case_add_submit)}
      </Button>

      {justAdded && (
        <p className={styles.status} role="status" aria-live="polite">
          {t(CONSOLE_MESSAGE_KEYS.evals_case_added)}
        </p>
      )}
      {invalidJson && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.evals_case_invalid_json)}
        </p>
      )}
      {addFailure !== null && (
        <p className={styles.problem} role="alert">
          {t(addFailure.messageKey, addFailure.messageArgs, addFailure.message)}
        </p>
      )}
    </div>
  );
}
