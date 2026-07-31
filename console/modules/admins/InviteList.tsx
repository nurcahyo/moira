"use client";

// Issued invitations, and the control that withdraws one.
//
// ============================================================================
// `expired` IS DERIVED, AND `status` NEVER SAYS SO
// ============================================================================
//
// `AdminInviteStatus` is `pending | consumed | revoked`. There is NO `expired`
// value, because nothing sweeps for it: expiry is computed at read time and
// arrives as the separate boolean `AdminInviteRecord.expired`. A list that keyed
// off `status` alone would show a dead invitation as "waiting to be redeemed"
// forever, which is exactly the state an operator would then wait on.
//
// ============================================================================
// THIS LIST IS PERSONAL DATA
// ============================================================================
//
// `value` is the invited address or domain and `consumed_subject` is the
// redeemer's IdP subject. Both are returned to any holder of
// `moira:admins:read`, which is the right audience — but a screen that renders a
// directory of who was invited should say so rather than leave it to be
// discovered, so it does (`console.admins.invites_privacy_note`).
//
// `consumed_subject` is deliberately NOT rendered: it is an opaque IdP
// identifier that identifies nobody to an operator, and printing it would add a
// second personal identifier to the page for no benefit.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AdminInviteRecord, ResponseText } from "@/lib/types";

import styles from "./InviteList.module.css";

export interface InviteListProps {
  readonly invites: readonly AdminInviteRecord[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites reload the server data. */
  readonly onChanged?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "message"; readonly messageKey: string; readonly text: ResponseText | null };

/** The invitation's state as an operator sees it — `expired` wins over `status`. */
export function inviteStateKey(invite: AdminInviteRecord): string {
  if (invite.status === "consumed") return CONSOLE_MESSAGE_KEYS.admins_invite_status_consumed;
  if (invite.status === "revoked") return CONSOLE_MESSAGE_KEYS.admins_invite_status_revoked;
  if (invite.expired) return CONSOLE_MESSAGE_KEYS.admins_invite_status_expired;
  return CONSOLE_MESSAGE_KEYS.admins_invite_status_pending;
}

/** Withdrawal is only meaningful while the invitation could still be redeemed. */
export function isWithdrawable(invite: AdminInviteRecord): boolean {
  return invite.status === "pending" && !invite.expired;
}

function revokePath(id: string): string {
  return `/api/admins/invites/${encodeURIComponent(id)}/revoke`;
}

export function InviteList({ invites, fetchImpl, onChanged }: InviteListProps) {
  const [target, setTarget] = useState<AdminInviteRecord | null>(null);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  async function withdraw(invite: AdminInviteRecord): Promise<void> {
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;
    let response: Response;
    try {
      response = await send(revokePath(invite.id), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      });
    } catch {
      setPhase({
        kind: "message",
        messageKey: CONSOLE_MESSAGE_KEYS.admins_request_failed,
        text: null,
      });
      return;
    }

    let body: unknown;
    try {
      body = await response.json();
    } catch {
      body = undefined;
    }

    setTarget(null);

    if (!response.ok) {
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setPhase({
        kind: "message",
        messageKey:
          text?.message_key ?? error?.message_key ?? CONSOLE_MESSAGE_KEYS.admins_request_failed,
        text,
      });
      return;
    }

    setPhase({ kind: "idle" });
    onChanged?.();
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.admins_invites_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.admins_invites_heading)}</h2>
      <p className={styles.note}>{t(CONSOLE_MESSAGE_KEYS.admins_invites_privacy_note)}</p>

      {invites.length === 0 ? (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.admins_invites_empty)}</p>
      ) : (
        <table className={styles.table}>
          <caption className={styles.caption}>
            {t(CONSOLE_MESSAGE_KEYS.admins_invites_table_label)}
          </caption>
          <thead>
            <tr>
              <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_invite_column_value)}</th>
              <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_invite_column_status)}</th>
              <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_invite_column_expires)}</th>
              <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_column_actions)}</th>
            </tr>
          </thead>
          <tbody>
            {invites.map((invite) => (
              <tr key={invite.id}>
                <th scope="row">{invite.value}</th>
                <td>{t(inviteStateKey(invite))}</td>
                <td>{invite.expires_at}</td>
                <td>
                  {isWithdrawable(invite) && (
                    <Button
                      type="button"
                      variant="secondary"
                      size="sm"
                      loading={phase.kind === "pending" && target?.id === invite.id}
                      onClick={() => setTarget(invite)}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.admins_invite_revoke)}
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <p className={styles.activity} role="status" aria-live="polite">
        {phase.kind === "message" &&
          t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
      </p>

      <DangerConfirmDialog
        open={target !== null}
        busy={phase.kind === "pending"}
        title={t(CONSOLE_MESSAGE_KEYS.admins_invite_revoke_confirm_title)}
        body={t(CONSOLE_MESSAGE_KEYS.admins_invite_revoke_confirm_body, {
          value: target?.value ?? "",
        })}
        confirmLabel={t(CONSOLE_MESSAGE_KEYS.admins_invite_revoke_confirm_action)}
        onCancel={() => setTarget(null)}
        onConfirm={() => {
          if (target !== null) void withdraw(target);
        }}
      />
    </section>
  );
}
