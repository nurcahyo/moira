// The routing-transparency panel — what makes the playground a testing tool
// rather than a toy (issue #261's own framing). Renders whichever of the two
// sources produced data:
//
//   `public`     — always available, no extra scope: `GET
//                  /api/v1/executions/{execution_id}` plus any
//                  `response.fallback.selected` events observed live while
//                  streaming.
//   `diagnostic` — `POST /api/v1/admin/runtime/diagnose`'s full result:
//                  candidate ranking (rank/score/selection reason) and every
//                  provider attempt, on top of everything `public` has.
//
// A presentational organism — no `fetch` of its own, unlike `PlaygroundControls`.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type {
  ExecutionFailure,
  ProviderAttemptSummary,
  PublicUsageSummary,
  RuntimeEventEnvelope,
  UsageSummary,
} from "@/lib/types";

import { candidateRanking } from "./runtime-events";
import type { RoutingSummaryState } from "./types";
import styles from "./PlaygroundRoutingSummary.module.css";

function usageLine(usage: PublicUsageSummary | UsageSummary): string {
  return t(CONSOLE_MESSAGE_KEYS.playground_routing_usage_tokens, {
    input: usage.input_tokens ?? "—",
    output: usage.output_tokens ?? "—",
    total: usage.total_tokens ?? "—",
  });
}

export interface PlaygroundRoutingSummaryProps {
  readonly state: RoutingSummaryState;
}

export function PlaygroundRoutingSummary({ state }: PlaygroundRoutingSummaryProps) {
  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.playground_routing_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.playground_routing_heading)}</h2>

      {state.kind === "empty" && (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_unavailable)}</p>
      )}

      {state.kind === "pending" && (
        <p className={styles.empty} role="status">
          {t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_pending)}
        </p>
      )}

      {state.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.playground_execution_summary_failed)}
        </p>
      )}

      {state.kind === "public" && (
        <>
          <dl className={styles.factList}>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_route_label)}</dt>
              <dd>{state.summary.route?.key ?? "—"}</dd>
            </div>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_model_label)}</dt>
              <dd>{state.summary.model?.key ?? "—"}</dd>
            </div>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_status_label)}</dt>
              <dd>{state.summary.status}</dd>
            </div>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_latency_label)}</dt>
              <dd>{state.summary.latency_ms !== null && state.summary.latency_ms !== undefined ? `${state.summary.latency_ms} ms` : "—"}</dd>
            </div>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_attempt_count_label)}</dt>
              <dd>{state.summary.attempt_count}</dd>
            </div>
            <div className={styles.fact}>
              <dt>{t(CONSOLE_MESSAGE_KEYS.playground_routing_usage_label)}</dt>
              <dd>{usageLine(state.summary.usage)}</dd>
            </div>
          </dl>

          {state.fallbackHops.length > 0 && (
            <div>
              <h3 className={styles.subheading}>
                {t(CONSOLE_MESSAGE_KEYS.playground_routing_fallback_heading)}
              </h3>
              <ul className={styles.list}>
                {state.fallbackHops.map((hop, index) => (
                  <li key={`${hop.fromProviderId ?? "unknown"}-${hop.toProviderId ?? "unknown"}-${index}`}>
                    {hop.fromProviderId ?? "—"} → {hop.toProviderId ?? "—"}
                    {hop.failureClass !== null ? ` (${hop.failureClass})` : ""}
                  </li>
                ))}
              </ul>
            </div>
          )}

          <p className={styles.note}>{t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_public_note)}</p>
        </>
      )}

      {state.kind === "diagnostic" && (
        <DiagnosticRoutingSummary
          attempts={state.result.outcome.attempts}
          events={state.result.events}
          failure={state.result.outcome.failure ?? null}
        />
      )}
    </section>
  );
}

function DiagnosticRoutingSummary({
  attempts,
  events,
  failure,
}: {
  readonly attempts: readonly ProviderAttemptSummary[];
  readonly events: readonly RuntimeEventEnvelope[];
  readonly failure: ExecutionFailure | null;
}) {
  const candidates = candidateRanking(events);

  return (
    <>
      <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_heading)}</h3>

      {failure !== null && (
        <div className={styles.problem} role="alert">
          <h4 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_failure_heading)}</h4>
          <p>
            {failure.class}: {failure.message}
          </p>
        </div>
      )}

      {candidates.length > 0 && (
        <div>
          <h3 className={styles.subheading}>
            {t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_candidates_heading)}
          </h3>
          <table className={styles.table}>
            <thead>
              <tr>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_candidate_rank_label)}</th>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_candidate_score_label)}</th>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_candidate_reason_label)}</th>
              </tr>
            </thead>
            <tbody>
              {candidates.map((candidate, index) => (
                <tr key={`${candidate.providerModelId ?? "unknown"}-${index}`}>
                  <td>{candidate.candidateRank ?? "—"}</td>
                  <td>{candidate.candidateScore ?? "—"}</td>
                  <td>{candidate.selectionReason ?? "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {attempts.length > 0 && (
        <div>
          <h3 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.playground_diagnostics_attempts_heading)}</h3>
          <table className={styles.table}>
            <thead>
              <tr>
                <th scope="col">#</th>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_routing_status_label)}</th>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_routing_latency_label)}</th>
                <th scope="col">{t(CONSOLE_MESSAGE_KEYS.playground_routing_usage_label)}</th>
              </tr>
            </thead>
            <tbody>
              {attempts.map((attempt) => (
                <tr key={attempt.attempt_id}>
                  <td>{attempt.attempt_number}</td>
                  <td>{attempt.failure_class ?? attempt.status}</td>
                  <td>{attempt.latency_ms !== null && attempt.latency_ms !== undefined ? `${attempt.latency_ms} ms` : "—"}</td>
                  <td>{usageLine(attempt.usage)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
