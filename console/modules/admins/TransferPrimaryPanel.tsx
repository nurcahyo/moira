"use client";

// Ownership transfer and grant revocation: the stateful half of `AdminTable`.
//
// ============================================================================
// TRANSFER IS **ONE** REQUEST (plan 09 §0.8 W5-B8)
// ============================================================================
//
// The plan body describes transfer as "two sequential calls, each with its own
// `Idempotency-Key` and `If-Match`" — promote the target, then demote the actor.
// That is wrong after PR #39 and would actively break:
//
//   * `set_primary` calls `demote_active_primaries_other_than` INSIDE THE SAME
//     TRANSACTION, so the incumbent is already demoted when the first call
//     returns;
//   * `admin_identities_single_active_primary` is UNIQUE on `(is_primary)` where
//     the row is live, so a second owner cannot exist to be demoted;
//   * the actor's `version` has moved by then, so the second call would either
//     demote the person just promoted or 409 on a stale `If-Match`.
//
// So: one `PATCH`, with `If-Match` from the row being promoted.
//
// ============================================================================
// WHY THE MUTATIONS GO THROUGH ROUTE HANDLERS
// ============================================================================
//
// This is a `"use client"` module, and `layer-dependencies.test.ts` rule 5
// forbids a client module from importing any credential-carrying module —
// `lib/moira-client.ts` is in that set by name. Decision W5-D5 puts the
// mutations behind route handlers under `app/api/**` rather than server actions,
// because that is the shipped precedent (`SignInPanel` posts to a mounted
// handler) and because adopting server actions means adding `nextCookies()` to
// the Better Auth plugin list in a wave that should not touch the auth instance.
//
// Each handler re-checks the session itself: `app/api/**` sits outside every
// route group and therefore outside `(console)`'s gate.

import { useState } from "react";

import { DangerConfirmDialog } from "@/components/molecules/DangerConfirmDialog";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AdminIdentityRecord, ResponseText } from "@/lib/types";

import { AdminTable } from "./AdminTable";
import styles from "./TransferPrimaryPanel.module.css";

/** What the browser asks the console's own API to do. */
type Intent =
  | { readonly kind: "transfer"; readonly identity: AdminIdentityRecord }
  | { readonly kind: "revoke"; readonly identity: AdminIdentityRecord };

/** The outcome, as something renderable through `t()`. */
type Outcome =
  | { readonly kind: "idle" }
  | { readonly kind: "busy" }
  | { readonly kind: "message"; readonly messageKey: string; readonly text: ResponseText | null };

export interface TransferPrimaryPanelProps {
  readonly identities: readonly AdminIdentityRecord[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites reload the server data. */
  readonly onChanged?: () => void;
}

/** The console's own mutation endpoints. Not Moira's. */
function identityPath(id: string): string {
  return `/api/admins/identities/${encodeURIComponent(id)}`;
}

export function TransferPrimaryPanel({
  identities,
  fetchImpl,
  onChanged,
}: TransferPrimaryPanelProps) {
  const [intent, setIntent] = useState<Intent | null>(null);
  const [outcome, setOutcome] = useState<Outcome>({ kind: "idle" });

  async function apply(current: Intent): Promise<void> {
    setOutcome({ kind: "busy" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(identityPath(current.identity.id), {
        method: current.kind === "transfer" ? "PATCH" : "DELETE",
        headers: { "content-type": "application/json" },
        // The version the browser was RENDERED with, which the route handler
        // turns into `If-Match`. That is optimistic concurrency, not an
        // authorization input: a client that sends a wrong version gets a
        // `409 resource_version_conflict` from Moira and nothing else, and one
        // that sends the right version still has to pass
        // `require_primary_actor`. There is no `GET /admin-identities/{id}` in
        // the spec, so the alternative — re-reading the row server-side — means
        // paging a list to find one record on every mutation.
        body: JSON.stringify({ version: current.identity.version }),
      });
    } catch {
      // The thrown cause is deliberately not read: a fetch failure can carry a
      // URL with credentials in it.
      setOutcome({
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

    setIntent(null);

    if (!response.ok) {
      // Moira's own refusal, keyed. `lib/console-api.ts` forwards the
      // client-safe `MoiraError`, so the key and the server's English are both
      // available and `t()` prefers the catalog.
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setOutcome({
        kind: "message",
        messageKey: text?.message_key ?? error?.message_key ?? CONSOLE_MESSAGE_KEYS.admins_request_failed,
        text,
      });
      return;
    }

    const record = body as AdminIdentityRecord | undefined;
    setOutcome({
      kind: "message",
      messageKey: record?.notice.message_key ?? CONSOLE_MESSAGE_KEYS.admins_working,
      text: record?.notice ?? null,
    });
    onChanged?.();
  }

  const busyId = outcome.kind === "busy" && intent !== null ? intent.identity.id : null;

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.admins_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.admins_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.admins_intro)}</p>
      <p className={styles.note}>{t(CONSOLE_MESSAGE_KEYS.admins_per_grant_note)}</p>

      <AdminTable
        identities={identities}
        busyId={busyId}
        onTransfer={(identity) => setIntent({ kind: "transfer", identity })}
        onRevoke={(identity) => setIntent({ kind: "revoke", identity })}
      />

      {/* Present BEFORE it is populated. A live region created and filled in the
          same tick is frequently missed by assistive technology. */}
      <p
        className={styles.activity}
        role="status"
        aria-live="polite"
        aria-label={t(CONSOLE_MESSAGE_KEYS.admins_activity_label)}
      >
        {outcome.kind === "busy" && t(CONSOLE_MESSAGE_KEYS.admins_working)}
        {outcome.kind === "message" &&
          t(outcome.messageKey, outcome.text?.message_args, outcome.text?.message)}
      </p>

      <DangerConfirmDialog
        open={intent !== null}
        busy={outcome.kind === "busy"}
        title={
          intent?.kind === "revoke"
            ? t(CONSOLE_MESSAGE_KEYS.admins_revoke_confirm_title)
            : t(CONSOLE_MESSAGE_KEYS.admins_transfer_confirm_title)
        }
        body={
          intent?.kind === "revoke"
            ? t(CONSOLE_MESSAGE_KEYS.admins_revoke_confirm_body, {
                email: intent.identity.email,
              })
            : t(CONSOLE_MESSAGE_KEYS.admins_transfer_confirm_body, {
                email: intent?.identity.email ?? "",
              })
        }
        confirmLabel={
          intent?.kind === "revoke"
            ? t(CONSOLE_MESSAGE_KEYS.admins_revoke_confirm_action)
            : t(CONSOLE_MESSAGE_KEYS.admins_transfer_confirm_action)
        }
        onCancel={() => setIntent(null)}
        onConfirm={() => {
          if (intent !== null) void apply(intent);
        }}
      />
    </section>
  );
}
