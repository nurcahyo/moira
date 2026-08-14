"use client";

// One application's issued keys, and the one destructive control on the screen.
//
// ============================================================================
// WHAT A ROW MAY SHOW
// ============================================================================
//
// `key_prefix`, and nothing else about the credential. The view model it renders
// (`ConsumerKeyView`) does not carry `fingerprint` or `pepper_version` at all, so
// this is a property of the data rather than a discipline in the markup — a
// well-meaning "show the fingerprint so we can match it in the logs" edit has
// nothing to reach for.
//
// ============================================================================
// REVOKE ASKS FIRST
// ============================================================================
//
// It is the only control here that breaks running software, and the
// confirmation names both halves of what happens: traffic stops immediately, and
// the row stays so the operator can still read when it was last used. A dialog
// that said only "are you sure" would be a speed bump rather than information.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ConsumerKeyView } from "@/lib/keys-view";
import type { KeyStatus, ResponseText } from "@/lib/types";

import { scopeMessageKey } from "./scope-copy";
import styles from "./KeyList.module.css";

const STATUS_MESSAGE_KEYS: Readonly<Record<KeyStatus, string>> = {
  active: CONSOLE_MESSAGE_KEYS.keys_status_active,
  revoked: CONSOLE_MESSAGE_KEYS.keys_status_revoked,
  expired: CONSOLE_MESSAGE_KEYS.keys_status_expired,
  deleted: CONSOLE_MESSAGE_KEYS.keys_status_deleted,
};

/** Colour is never the only signal — the badge always carries its own text. */
function toneFor(status: KeyStatus): "success" | "neutral" | "warning" {
  if (status === "active") return "success";
  if (status === "expired") return "warning";
  return "neutral";
}

export interface KeyListProps {
  readonly keys: readonly ConsumerKeyView[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites re-read the server data. */
  readonly onRevoked?: () => void;
}

export function KeyList({ keys, fetchImpl, onRevoked }: KeyListProps) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [failure, setFailure] = useState<{
    readonly messageKey: string;
    readonly text: ResponseText | null;
  } | null>(null);

  async function revoke(id: string): Promise<void> {
    setRevoking(id);
    setFailure(null);
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(`/api/keys/${encodeURIComponent(id)}/revoke`, { method: "POST" });
    } catch {
      setRevoking(null);
      setConfirming(null);
      setFailure({ messageKey: CONSOLE_MESSAGE_KEYS.keys_request_failed, text: null });
      return;
    }

    setRevoking(null);
    setConfirming(null);

    if (!response.ok) {
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        body = undefined;
      }
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setFailure({
        messageKey:
          text?.message_key ?? error?.message_key ?? CONSOLE_MESSAGE_KEYS.keys_request_failed,
        text,
      });
      return;
    }

    onRevoked?.();
  }

  if (keys.length === 0) {
    return <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.keys_issued_empty)}</p>;
  }

  return (
    <>
      <ul className={styles.list}>
        {keys.map((key) => (
          <li key={key.id} className={styles.row}>
            <span className={styles.name}>{key.display_name}</span>
            <Badge tone={toneFor(key.status)}>{t(STATUS_MESSAGE_KEYS[key.status])}</Badge>

            <span className={styles.detail}>
              {t(CONSOLE_MESSAGE_KEYS.keys_prefix_label)}: <code>{key.key_prefix}</code>
            </span>

            <span className={styles.detail}>
              {t(CONSOLE_MESSAGE_KEYS.keys_last_used_label)}:{" "}
              {key.last_used_at ?? t(CONSOLE_MESSAGE_KEYS.keys_never_used)}
            </span>

            <span className={styles.detail}>
              {t(CONSOLE_MESSAGE_KEYS.keys_expires_label)}:{" "}
              {key.expires_at ?? t(CONSOLE_MESSAGE_KEYS.keys_expires_never)}
            </span>

            <span className={styles.detail}>
              {t(CONSOLE_MESSAGE_KEYS.keys_scopes_label)}:{" "}
              {key.scopes
                .map((scope) => {
                  const messageKey = scopeMessageKey(scope);
                  // A scope minted elsewhere — through the admin API, or before
                  // this screen curated its list — has no copy here. Its raw
                  // string is the honest rendering: dropping it would show a key
                  // as less capable than it is.
                  return messageKey === null ? scope : t(messageKey);
                })
                .join(", ")}
            </span>

            {/* Only a live key can be revoked. A revoked one keeps its row and
                loses its control, so the list stays readable without offering an
                action that would 409. */}
            {key.status === "active" && (
              <Button
                type="button"
                variant="secondary"
                loading={revoking === key.id}
                onClick={() => setConfirming(key.id)}
              >
                {t(CONSOLE_MESSAGE_KEYS.keys_revoke_button)}
              </Button>
            )}

            <DangerConfirmDialog
              open={confirming === key.id}
              title={t(CONSOLE_MESSAGE_KEYS.keys_revoke_button)}
              body={t(CONSOLE_MESSAGE_KEYS.keys_revoke_confirm_body)}
              confirmLabel={t(CONSOLE_MESSAGE_KEYS.keys_revoke_button)}
              busy={revoking === key.id}
              onCancel={() => setConfirming(null)}
              onConfirm={() => {
                void revoke(key.id);
              }}
            />
          </li>
        ))}
      </ul>

      <p className={styles.activity} role="status" aria-live="polite">
        {revoking !== null && t(CONSOLE_MESSAGE_KEYS.keys_revoke_pending)}
      </p>

      {failure !== null && (
        <p className={styles.problem} role="alert">
          {t(failure.messageKey, failure.text?.message_args, failure.text?.message)}
        </p>
      )}
    </>
  );
}
