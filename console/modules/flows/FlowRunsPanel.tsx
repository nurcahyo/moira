"use client";

// One flow's run history, and the button that triggers a new one.
//
// Same posture as `EvalRunsPanel` for the same reason: `POST /api/flows/{id}/run`
// answers a deliberate, keyed 501 until the parallel backend PR lands (see that
// route's header), rendered through the same `ConsoleApiFailure` shape a real
// failure would use — no separate code path to update once the endpoint works.

import { useEffect, useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { type ConsoleApiFailure, getJson, postJson } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AgentFlowRunRecord, FlowRunStatus, ListResponse } from "@/lib/types";

import styles from "./FlowRunsPanel.module.css";

export interface FlowRunsPanelProps {
  readonly flowId: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

const RUN_STATUS_KEYS: Record<FlowRunStatus, string> = {
  running: CONSOLE_MESSAGE_KEYS.flows_run_status_running,
  completed: CONSOLE_MESSAGE_KEYS.flows_run_status_completed,
  failed: CONSOLE_MESSAGE_KEYS.flows_run_status_failed,
  cancelled: CONSOLE_MESSAGE_KEYS.flows_run_status_cancelled,
};

function runsUrl(flowId: string): string {
  return `/api/flows/${encodeURIComponent(flowId)}/runs`;
}

export function FlowRunsPanel({ flowId, fetchImpl }: FlowRunsPanelProps) {
  const [runs, setRuns] = useState<readonly AgentFlowRunRecord[] | null>(null);
  const [loadFailure, setLoadFailure] = useState<ConsoleApiFailure | null>(null);
  const [triggering, setTriggering] = useState(false);
  const [triggerFailure, setTriggerFailure] = useState<ConsoleApiFailure | null>(null);

  /** Manual reload, called after a run is triggered. */
  async function reload(): Promise<void> {
    const result = await getJson<ListResponse<AgentFlowRunRecord>>(
      runsUrl(flowId),
      CONSOLE_MESSAGE_KEYS.flows_load_failed,
      fetchImpl,
    );
    if (!result.ok) {
      setLoadFailure(result.failure);
      return;
    }
    setLoadFailure(null);
    setRuns(result.data.data);
  }

  // The initial, on-expand fetch. Defined inline with its own `cancelled` guard
  // — rather than calling `reload` above — so a flow switched away from before
  // the response arrives never applies a stale result to this component's state.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const result = await getJson<ListResponse<AgentFlowRunRecord>>(
        runsUrl(flowId),
        CONSOLE_MESSAGE_KEYS.flows_load_failed,
        fetchImpl,
      );
      if (cancelled) return;
      if (!result.ok) {
        setLoadFailure(result.failure);
        return;
      }
      setLoadFailure(null);
      setRuns(result.data.data);
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [flowId]);

  async function triggerRun(): Promise<void> {
    setTriggering(true);
    setTriggerFailure(null);
    const result = await postJson<AgentFlowRunRecord>(
      `/api/flows/${encodeURIComponent(flowId)}/run`,
      {},
      CONSOLE_MESSAGE_KEYS.flows_run_not_available,
      fetchImpl,
    );
    setTriggering(false);
    if (!result.ok) {
      setTriggerFailure(result.failure);
      return;
    }
    await reload();
  }

  return (
    <div className={styles.panel}>
      <h4 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.flows_runs_heading)}</h4>

      <Button
        type="button"
        variant="secondary"
        loading={triggering}
        onClick={() => {
          void triggerRun();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.flows_run_trigger)}
      </Button>

      {triggerFailure !== null && (
        <p
          className={triggerFailure.messageKey === CONSOLE_MESSAGE_KEYS.flows_run_not_available ? styles.notice : styles.problem}
          role="status"
        >
          {t(triggerFailure.messageKey, triggerFailure.messageArgs, triggerFailure.message)}
        </p>
      )}

      {loadFailure !== null && (
        <p className={styles.problem} role="alert">
          {t(loadFailure.messageKey, loadFailure.messageArgs, loadFailure.message)}
        </p>
      )}

      {runs !== null && runs.length === 0 && (
        <p className={styles.status}>{t(CONSOLE_MESSAGE_KEYS.flows_runs_empty)}</p>
      )}

      {runs !== null && runs.length > 0 && (
        <ul className={styles.rows}>
          {runs.map((run) => (
            <li key={run.id} className={styles.row}>
              <Badge tone={run.status === "completed" ? "success" : run.status === "failed" ? "danger" : "neutral"}>
                {t(RUN_STATUS_KEYS[run.status])}
              </Badge>
              <span>
                {t(CONSOLE_MESSAGE_KEYS.flows_run_created_label)}: {run.created_at}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
