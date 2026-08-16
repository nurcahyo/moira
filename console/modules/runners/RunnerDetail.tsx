"use client";

// One runner's lifecycle, end to end: provisioning, the authorization URL,
// pasting the code, finalizing against a provider, and deletion.
//
// ============================================================================
// `onRefresh`/`onDeleted`, NOT `useRouter()` — SAME SPLIT AS `ProviderList`
// ============================================================================
//
// `ProviderList` (`modules/llm/ProviderList.tsx`) takes an `onChanged` prop and
// never imports `next/navigation` itself; `LlmSettingsPanels` is the thin
// `"use client"` wrapper that calls `useRouter()` and wires
// `onChanged={() => router.refresh()}`. This organism follows the same split,
// through `RunnerDetailPanel.tsx`, for the same reason: `useRouter()` requires
// an App Router context that a bare `render()` in a unit test does not provide,
// so a component that called it directly would be untestable with this
// console's existing testing-library setup — there is no next/navigation mock
// wired into `bunfig.toml`'s preload, unlike some frameworks' test harnesses.
//
// `GET /api/runners/[id]` (and Moira's `GET /runners/{id}` beneath it) WRITES —
// it refreshes the row from the runner service, which is what surfaces
// `authorization_url` once the container's tty stream has rendered it. Polling
// is therefore `onRefresh` called on an interval rather than only after a
// mutation, and it stops once the runner reaches a terminal state
// (`linked`/`failed`/`expired`) or once this component has learned the runner
// is an unrecoverable write-off — see below.
//
// ============================================================================
// `runner_token_unavailable` IS HANDLED ENTIRELY CLIENT-SIDE, AND HONESTLY SO
// ============================================================================
//
// Moira writes the runner row only AFTER the credential exists
// (`src/domain/runners.rs`'s own doc comment), so a finalize call that fails
// with this code leaves the row still reporting `state: "ready"` — there is no
// server-side signal that distinguishes "never tried" from "tried and the
// one-shot token read was already spent". This component's local
// `tokenUnavailable` flag is therefore the ONLY place that fact is recorded,
// and it does not survive a hard reload of this page — only an `onRefresh`
// call (which, wired to `router.refresh()`, re-renders this same component
// instance rather than remounting it). That is a real, stated limitation, not
// an oversight: the alternative would be inventing a persisted signal Moira's
// own contract does not provide.

import { useEffect, useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { CopyButton } from "@/components/atoms/CopyButton";
import { Label } from "@/components/atoms/Label";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import {
  CLAUDE_RUNNER_TERMINAL_STATES,
  type ClaudeRunnerRecord,
  type ProviderRecord,
} from "@/lib/types";

import { postRunnerJson, sendRunnerDelete, type RunnerFailure } from "./request";
import { scopeLabel, scopeTone, stateLabel, stateTone } from "./runner-view";
import styles from "./RunnerDetail.module.css";

/** How often this page re-reads the runner while its state may still move. */
const POLL_INTERVAL_MS = 4000;

const RUNNER_TOKEN_UNAVAILABLE_CODE = "runner_token_unavailable";

export interface RunnerDetailProps {
  readonly runner: ClaudeRunnerRecord;
  /** Providers the finalize form may bind this runner's credential to. */
  readonly providers: readonly ProviderRecord[];
  /**
   * Called after a successful authorization-code submit, after a successful
   * finalize, and on every poll tick while the runner's state may still move.
   * Shipped call sites (`RunnerDetailPanel`) wire this to `router.refresh()`.
   */
  readonly onRefresh: () => void;
  /** Called after a successful delete. Shipped call sites navigate to `/runners`. */
  readonly onDeleted: () => void;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

type ActionPhase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly failure: RunnerFailure };

export function RunnerDetail({ runner, providers, onRefresh, onDeleted, fetchImpl }: RunnerDetailProps) {
  const [authCode, setAuthCode] = useState("");
  const [authPhase, setAuthPhase] = useState<ActionPhase>({ kind: "idle" });

  const [providerId, setProviderId] = useState(providers[0]?.id ?? "");
  const [displayName, setDisplayName] = useState("");
  const [finalizePhase, setFinalizePhase] = useState<ActionPhase>({ kind: "idle" });
  const [tokenUnavailable, setTokenUnavailable] = useState(false);

  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deletePhase, setDeletePhase] = useState<ActionPhase>({ kind: "idle" });

  const terminal = CLAUDE_RUNNER_TERMINAL_STATES.has(runner.state);

  useEffect(() => {
    if (terminal || tokenUnavailable) return;
    const timer = setInterval(() => onRefresh(), POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [terminal, tokenUnavailable, onRefresh]);

  async function submitAuthorizationCode(): Promise<void> {
    const code = authCode.trim();
    if (code === "") return;
    setAuthPhase({ kind: "pending" });
    const result = await postRunnerJson<ClaudeRunnerRecord>(
      `/api/runners/${encodeURIComponent(runner.id)}/authorization-code`,
      { code },
      fetchImpl,
    );
    if (!result.ok) {
      setAuthPhase({ kind: "failed", failure: result.failure });
      return;
    }
    setAuthPhase({ kind: "idle" });
    setAuthCode("");
    onRefresh();
  }

  async function submitFinalize(): Promise<void> {
    if (providerId === "") return;
    setFinalizePhase({ kind: "pending" });
    const trimmedDisplayName = displayName.trim();
    const result = await postRunnerJson<ClaudeRunnerRecord>(
      `/api/runners/${encodeURIComponent(runner.id)}/finalize`,
      {
        provider_id: providerId,
        ...(trimmedDisplayName === "" ? {} : { display_name: trimmedDisplayName }),
      },
      fetchImpl,
    );
    if (!result.ok) {
      if (result.failure.code === RUNNER_TOKEN_UNAVAILABLE_CODE) {
        // A write-off. See this file's header — nothing here is retryable, and
        // showing the ordinary failure banner (which invites another click)
        // would be actively wrong.
        setTokenUnavailable(true);
        setFinalizePhase({ kind: "idle" });
        return;
      }
      setFinalizePhase({ kind: "failed", failure: result.failure });
      return;
    }
    setFinalizePhase({ kind: "idle" });
    onRefresh();
  }

  async function confirmDelete(): Promise<void> {
    setDeletePhase({ kind: "pending" });
    const result = await sendRunnerDelete(`/api/runners/${encodeURIComponent(runner.id)}`, fetchImpl);
    if (!result.ok) {
      setDeletePhase({ kind: "failed", failure: result.failure });
      setDeleteDialogOpen(false);
      return;
    }
    onDeleted();
  }

  return (
    <article className={styles.article}>
      <a className={styles.back} href="/runners">
        {t(CONSOLE_MESSAGE_KEYS.runners_detail_back)}
      </a>

      <header className={styles.header}>
        <h1 className={styles.title}>{runner.label}</h1>
        <Badge tone={stateTone(runner.state)}>{stateLabel(runner.state)}</Badge>
      </header>

      <dl className={styles.meta}>
        <div className={styles.metaRow}>
          <dt>{t(CONSOLE_MESSAGE_KEYS.runners_detail_scope_label)}</dt>
          <dd>
            <Badge tone={scopeTone(runner.scope)}>{scopeLabel(runner.scope)}</Badge>
          </dd>
        </div>
        {runner.expires_at != null && (
          <div className={styles.metaRow}>
            <dt>{t(CONSOLE_MESSAGE_KEYS.runners_detail_expires_label)}</dt>
            <dd>{runner.expires_at}</dd>
          </div>
        )}
      </dl>

      {tokenUnavailable ? (
        <section className={styles.section} aria-label={t(CONSOLE_MESSAGE_KEYS.runners_token_unavailable_heading)}>
          <h2 className={styles.sectionHeading}>
            {t(CONSOLE_MESSAGE_KEYS.runners_token_unavailable_heading)}
          </h2>
          <p role="alert">{t(CONSOLE_MESSAGE_KEYS.runners_token_unavailable_body)}</p>
        </section>
      ) : (
        <>
          {runner.state === "provisioning" && (
            <p role="status" aria-live="polite">
              {t(CONSOLE_MESSAGE_KEYS.runners_state_provisioning_body)}
            </p>
          )}

          {runner.state === "awaiting_authorization" && (
            <section
              className={styles.section}
              aria-label={t(CONSOLE_MESSAGE_KEYS.runners_authorization_heading)}
            >
              <h2 className={styles.sectionHeading}>
                {t(CONSOLE_MESSAGE_KEYS.runners_authorization_heading)}
              </h2>
              <p>{t(CONSOLE_MESSAGE_KEYS.runners_authorization_intro)}</p>

              {runner.authorization_url != null && (
                <p className={styles.urlRow}>
                  <span className={styles.urlLabel}>
                    {t(CONSOLE_MESSAGE_KEYS.runners_authorization_url_label)}
                  </span>{" "}
                  <a
                    id="runner-authorization-url"
                    href={runner.authorization_url}
                    target="_blank"
                    rel="noopener noreferrer"
                  >
                    {runner.authorization_url}
                  </a>{" "}
                  <CopyButton targetId="runner-authorization-url" />
                </p>
              )}

              <FormField
                label={t(CONSOLE_MESSAGE_KEYS.runners_authorization_code_label)}
                hint={t(CONSOLE_MESSAGE_KEYS.runners_authorization_code_hint)}
                required
                inputProps={{
                  value: authCode,
                  disabled: authPhase.kind === "pending",
                  onChange: (event) => setAuthCode(event.target.value),
                }}
              />
              <Button
                type="button"
                variant="primary"
                loading={authPhase.kind === "pending"}
                disabled={authCode.trim() === ""}
                onClick={() => {
                  void submitAuthorizationCode();
                }}
              >
                {t(CONSOLE_MESSAGE_KEYS.runners_authorization_submit)}
              </Button>
              <p role="status" aria-live="polite" className={styles.activity}>
                {authPhase.kind === "pending" && t(CONSOLE_MESSAGE_KEYS.runners_authorization_pending)}
              </p>
              {authPhase.kind === "failed" && (
                <p role="alert" className={styles.problem}>
                  {t(authPhase.failure.messageKey, authPhase.failure.messageArgs, authPhase.failure.message)}
                </p>
              )}
            </section>
          )}

          {runner.state === "exchanging" && (
            <p role="status" aria-live="polite">
              {t(CONSOLE_MESSAGE_KEYS.runners_state_exchanging_body)}
            </p>
          )}

          {runner.state === "ready" && (
            <section
              className={styles.section}
              aria-label={t(CONSOLE_MESSAGE_KEYS.runners_finalize_heading)}
            >
              <h2 className={styles.sectionHeading}>{t(CONSOLE_MESSAGE_KEYS.runners_finalize_heading)}</h2>
              <p>{t(CONSOLE_MESSAGE_KEYS.runners_finalize_intro)}</p>

              {providers.length === 0 ? (
                <p role="alert">{t(CONSOLE_MESSAGE_KEYS.runners_finalize_no_providers)}</p>
              ) : (
                <>
                  <div className={styles.field}>
                    <Label htmlFor="runner-finalize-provider" required>
                      {t(CONSOLE_MESSAGE_KEYS.runners_finalize_provider_label)}
                    </Label>
                    <select
                      id="runner-finalize-provider"
                      value={providerId}
                      disabled={finalizePhase.kind === "pending"}
                      onChange={(event) => setProviderId(event.target.value)}
                    >
                      <option value="" disabled>
                        {t(CONSOLE_MESSAGE_KEYS.runners_finalize_provider_placeholder)}
                      </option>
                      {providers.map((provider) => (
                        <option key={provider.id} value={provider.id}>
                          {provider.display_name}
                        </option>
                      ))}
                    </select>
                  </div>

                  <FormField
                    label={t(CONSOLE_MESSAGE_KEYS.runners_finalize_display_name_label)}
                    inputProps={{
                      value: displayName,
                      disabled: finalizePhase.kind === "pending",
                      onChange: (event) => setDisplayName(event.target.value),
                    }}
                  />

                  <Button
                    type="button"
                    variant="primary"
                    loading={finalizePhase.kind === "pending"}
                    disabled={providerId === ""}
                    onClick={() => {
                      void submitFinalize();
                    }}
                  >
                    {t(CONSOLE_MESSAGE_KEYS.runners_finalize_submit)}
                  </Button>
                  <p role="status" aria-live="polite" className={styles.activity}>
                    {finalizePhase.kind === "pending" && t(CONSOLE_MESSAGE_KEYS.runners_finalize_pending)}
                  </p>
                  {finalizePhase.kind === "failed" && (
                    <p role="alert" className={styles.problem}>
                      {t(
                        finalizePhase.failure.messageKey,
                        finalizePhase.failure.messageArgs,
                        finalizePhase.failure.message,
                      )}
                    </p>
                  )}
                </>
              )}
            </section>
          )}

          {runner.state === "linked" && (
            <section className={styles.section} aria-label={t(CONSOLE_MESSAGE_KEYS.runners_linked_heading)}>
              <h2 className={styles.sectionHeading}>{t(CONSOLE_MESSAGE_KEYS.runners_linked_heading)}</h2>
              <p>{t(CONSOLE_MESSAGE_KEYS.runners_linked_body)}</p>
            </section>
          )}

          {runner.state === "failed" && (
            <section className={styles.section} aria-label={t(CONSOLE_MESSAGE_KEYS.runners_failed_heading)}>
              <h2 className={styles.sectionHeading}>{t(CONSOLE_MESSAGE_KEYS.runners_failed_heading)}</h2>
              <p role="alert">{t(CONSOLE_MESSAGE_KEYS.runners_failed_body)}</p>
            </section>
          )}

          {runner.state === "expired" && (
            <section className={styles.section} aria-label={t(CONSOLE_MESSAGE_KEYS.runners_expired_heading)}>
              <h2 className={styles.sectionHeading}>{t(CONSOLE_MESSAGE_KEYS.runners_expired_heading)}</h2>
              <p role="alert">{t(CONSOLE_MESSAGE_KEYS.runners_expired_body)}</p>
            </section>
          )}
        </>
      )}

      <Button
        type="button"
        variant="danger"
        size="sm"
        onClick={() => setDeleteDialogOpen(true)}
      >
        {t(CONSOLE_MESSAGE_KEYS.runners_delete_button)}
      </Button>

      {deletePhase.kind === "failed" && (
        <p role="alert" className={styles.problem}>
          {t(deletePhase.failure.messageKey, deletePhase.failure.messageArgs, deletePhase.failure.message)}
        </p>
      )}

      <DangerConfirmDialog
        open={deleteDialogOpen}
        title={t(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_body)}
        confirmLabel={
          deletePhase.kind === "pending"
            ? t(CONSOLE_MESSAGE_KEYS.runners_delete_pending)
            : t(CONSOLE_MESSAGE_KEYS.runners_delete_confirm_action)
        }
        busy={deletePhase.kind === "pending"}
        onConfirm={() => {
          void confirmDelete();
        }}
        onCancel={() => setDeleteDialogOpen(false)}
      />
    </article>
  );
}
