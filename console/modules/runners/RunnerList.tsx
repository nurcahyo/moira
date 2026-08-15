// Every provisioned Claude runner: its label, whose account it belongs to, and
// where it is in its lifecycle.
//
// Presentational only — no fetch, no `"use client"` needed. `RunnerPanels`
// composes this beside `RunnerProvisionForm` on `/runners`.

import { Badge } from "@/components/atoms/Badge";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ClaudeRunnerRecord } from "@/lib/types";

import { scopeLabel, scopeTone, stateLabel, stateTone } from "./runner-view";
import styles from "./RunnerList.module.css";

export interface RunnerListProps {
  readonly runners: readonly ClaudeRunnerRecord[];
}

export function RunnerList({ runners }: RunnerListProps) {
  if (runners.length === 0) {
    return <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.runners_list_empty)}</p>;
  }

  return (
    <section aria-label={t(CONSOLE_MESSAGE_KEYS.runners_list_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.runners_list_heading)}</h2>
      <table className={styles.table}>
        <caption className={styles.caption}>{t(CONSOLE_MESSAGE_KEYS.runners_table_label)}</caption>
        <thead>
          <tr>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.runners_column_label)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.runners_column_scope)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.runners_column_state)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.runners_column_created)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.runners_column_actions)}</th>
          </tr>
        </thead>
        <tbody>
          {runners.map((runner) => (
            <tr key={runner.id}>
              <th scope="row" className={styles.label}>
                {runner.label}
              </th>
              <td>
                <Badge tone={scopeTone(runner.scope)}>{scopeLabel(runner.scope)}</Badge>
              </td>
              <td>
                <Badge tone={stateTone(runner.state)}>{stateLabel(runner.state)}</Badge>
              </td>
              <td>{runner.created_at}</td>
              <td>
                <a href={`/runners/${encodeURIComponent(runner.id)}`}>
                  {t(CONSOLE_MESSAGE_KEYS.runners_view)}
                </a>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
