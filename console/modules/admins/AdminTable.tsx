"use client";

// The admin grants table.
//
// ============================================================================
// EVERY ROW IS A GRANT, NOT A PERSON — FINDING F24, MADE VISIBLE
// ============================================================================
//
// `admin_identities` is keyed on `(issuer, subject)`. On a console deployment
// `issuer` is the CONSOLE's own string, so:
//
//   * rendering an "issuer" column would show one constant value on every row
//     and disambiguate nothing;
//   * `subject` is an opaque IdP identifier;
//   * `email` is the only human-identifiable attribute on the record, which is
//     exactly why decision D5 makes it required and non-nullable.
//
// So the table LEADS WITH EMAIL. And from wave 4B, where each provider mints its
// own console issuer, one human signing in through two providers holds TWO
// grants with no column linking them: revocation is per-grant, and `is_primary`
// is globally unique so that human is primary through at most one of them.
//
// This screen does not invent a grouping to hide that. `console.admins.per_grant_note`
// states it in the operator's own terms, because a table that silently merged
// two rows would be claiming a person-level identity the data model does not
// have — and the operator would revoke "the" row and leave a live back door.
//
// ============================================================================
// `is_primary` IS ROW STATE, NOT A CAPABILITY OF THE READER
// ============================================================================
//
// `lib/types.ts` carries the same warning on the field. The badge says "this row
// is the owner"; it says nothing about whether the person looking at the screen
// may transfer or revoke. That is decided by `require_primary_actor` server-side
// from the CALLER's own row, and a console that treated the badge as a
// permission would disagree with Moira the moment a non-primary admin opened the
// page. Every control is therefore rendered, and a refusal arrives as
// `403 admin_identity_not_primary` with its own copy.
//
// ============================================================================
// THE OWNER'S REVOKE CONTROL IS DISABLED, WITH THE REASON BESIDE IT
// ============================================================================
//
// Decision D-F20: `revoke_grant` clears `is_primary`, and the last-primary guard
// refuses that — so on a deployment whose sole admin is the owner,
// `DELETE /admin-identities/{id}` is permanently unavailable for that row. That
// is a RULE, and it is rendered as one, with its remedy ("transfer ownership
// first"). Surfacing it as a failed request would teach the operator that the
// console is unreliable rather than that the model is deliberate.

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AdminIdentityRecord } from "@/lib/types";

import styles from "./AdminTable.module.css";

export interface AdminTableProps {
  readonly identities: readonly AdminIdentityRecord[];
  /** Id of the row a request is in flight for, or null. */
  readonly busyId: string | null;
  readonly onTransfer: (identity: AdminIdentityRecord) => void;
  readonly onRevoke: (identity: AdminIdentityRecord) => void;
}

function statusLabel(identity: AdminIdentityRecord): string {
  return identity.status === "active"
    ? t(CONSOLE_MESSAGE_KEYS.admins_status_active)
    : t(CONSOLE_MESSAGE_KEYS.admins_status_revoked);
}

export function AdminTable({ identities, busyId, onTransfer, onRevoke }: AdminTableProps) {
  if (identities.length === 0) {
    return <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.admins_empty)}</p>;
  }

  return (
    <table className={styles.table}>
      <caption className={styles.caption}>{t(CONSOLE_MESSAGE_KEYS.admins_table_label)}</caption>
      <thead>
        <tr>
          <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_column_email)}</th>
          <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_column_status)}</th>
          <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_column_created)}</th>
          <th scope="col">{t(CONSOLE_MESSAGE_KEYS.admins_column_actions)}</th>
        </tr>
      </thead>
      <tbody>
        {identities.map((identity) => {
          const revoked = identity.status !== "active";
          const busy = busyId === identity.id;
          return (
            <tr key={identity.id}>
              <th scope="row" className={styles.email}>
                {identity.email}
                {identity.is_primary && (
                  <>
                    {" "}
                    <Badge tone="info">{t(CONSOLE_MESSAGE_KEYS.admins_owner_badge)}</Badge>
                  </>
                )}
              </th>
              <td>{statusLabel(identity)}</td>
              <td>{identity.created_at}</td>
              <td className={styles.actions}>
                {!identity.is_primary && !revoked && (
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    loading={busy}
                    onClick={() => onTransfer(identity)}
                  >
                    {t(CONSOLE_MESSAGE_KEYS.admins_transfer)}
                  </Button>
                )}
                {!revoked &&
                  (identity.is_primary ? (
                    // Not a hidden control and not a silent no-op: the rule is
                    // stated where the action would have been.
                    <p className={styles.rule}>
                      {t(CONSOLE_MESSAGE_KEYS.admins_owner_not_revocable)}
                    </p>
                  ) : (
                    <Button
                      type="button"
                      variant="danger"
                      size="sm"
                      loading={busy}
                      onClick={() => onRevoke(identity)}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.admins_revoke)}
                    </Button>
                  ))}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
