// Tool-call visibility (issue #261 §5) — observable ONLY when the run went
// through `POST /api/v1/admin/runtime/diagnose`. `events` is `null` outside
// diagnostics mode, and the panel renders the honesty note instead of an
// empty list: an empty list would read as "no tools fired" when the true
// answer is "this view cannot see tool calls at all" — see
// `console.playground.tools_unavailable_note`.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { RuntimeEventEnvelope } from "@/lib/types";

import { toolCalls } from "./runtime-events";
import styles from "./PlaygroundToolCalls.module.css";

export interface PlaygroundToolCallsProps {
  /** `null` outside diagnostics mode. */
  readonly events: readonly RuntimeEventEnvelope[] | null;
}

export function PlaygroundToolCalls({ events }: PlaygroundToolCallsProps) {
  const calls = events === null ? [] : toolCalls(events);

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.playground_tools_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.playground_tools_heading)}</h2>

      {events === null && (
        <p className={styles.note}>{t(CONSOLE_MESSAGE_KEYS.playground_tools_unavailable_note)}</p>
      )}

      {events !== null && calls.length === 0 && (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.playground_tools_empty)}</p>
      )}

      {calls.length > 0 && (
        <ul className={styles.list}>
          {calls.map((call, index) => (
            <li key={`${call.name ?? "unknown"}-${index}`} className={styles.call}>
              <p className={styles.callName}>{call.name ?? "—"}</p>
              <dl className={styles.factList}>
                <div className={styles.fact}>
                  <dt>{t(CONSOLE_MESSAGE_KEYS.playground_tool_arguments_label)}</dt>
                  <dd className={styles.code}>{call.arguments ?? "—"}</dd>
                </div>
                <div className={styles.fact}>
                  <dt>{t(CONSOLE_MESSAGE_KEYS.playground_tool_outcome_label)}</dt>
                  <dd>{call.outcome ?? call.failureKind ?? "—"}</dd>
                </div>
              </dl>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
