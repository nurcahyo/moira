// The rolling reachability window for every enabled provider (issue #83).
//
// A plain presentational table — no client interactivity, so this stays a
// server-renderable organism with no `"use client"` directive. Reloading the
// data means reloading `/providers/health`, the same posture `/graph` takes
// for its own read-only view.

import { Badge, type BadgeTone } from "@/components/atoms/Badge";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ProviderHealthEntry, ProviderHealthResponse, ProviderHealthStatus } from "@/lib/types";

import styles from "./ProviderHealthTable.module.css";

export interface ProviderHealthTableProps {
  readonly health: ProviderHealthResponse;
}

const STATUS_KEYS: Record<ProviderHealthStatus, string> = {
  healthy: CONSOLE_MESSAGE_KEYS.providerhealth_status_healthy,
  degraded: CONSOLE_MESSAGE_KEYS.providerhealth_status_degraded,
  unhealthy: CONSOLE_MESSAGE_KEYS.providerhealth_status_unhealthy,
  unknown: CONSOLE_MESSAGE_KEYS.providerhealth_status_unknown,
};

const STATUS_TONES: Record<ProviderHealthStatus, BadgeTone> = {
  healthy: "success",
  degraded: "warning",
  unhealthy: "danger",
  unknown: "neutral",
};

function timestamp(value: string | null | undefined): string {
  return value ?? t(CONSOLE_MESSAGE_KEYS.providerhealth_never);
}

function latency(entry: ProviderHealthEntry): string {
  return entry.average_latency_ms === null || entry.average_latency_ms === undefined
    ? t(CONSOLE_MESSAGE_KEYS.providerhealth_latency_unknown)
    : `${Math.round(entry.average_latency_ms)} ms`;
}

export function ProviderHealthTable({ health }: ProviderHealthTableProps) {
  if (health.providers.length === 0) {
    return <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.providerhealth_empty)}</p>;
  }

  return (
    <div className={styles.wrapper}>
      <table className={styles.table} aria-label={t(CONSOLE_MESSAGE_KEYS.providerhealth_table_label)}>
        <thead>
          <tr>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_provider)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_type)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_status)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_probes)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_latency)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_last_probe)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_last_success)}</th>
            <th scope="col">{t(CONSOLE_MESSAGE_KEYS.providerhealth_column_last_failure)}</th>
          </tr>
        </thead>
        <tbody>
          {health.providers.map((entry) => (
            <tr key={entry.provider_id}>
              <td>{entry.display_name}</td>
              <td>{entry.provider_type}</td>
              <td>
                <Badge tone={STATUS_TONES[entry.status]}>{t(STATUS_KEYS[entry.status])}</Badge>
              </td>
              <td>
                {entry.probes_successful} / {entry.probes_total}
              </td>
              <td>{latency(entry)}</td>
              <td>{timestamp(entry.last_probe_at)}</td>
              <td>{timestamp(entry.last_success_at)}</td>
              <td>{timestamp(entry.last_failure_at)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
