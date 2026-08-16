// Shared rendering helpers for the runner list and detail screens — a state to
// a badge tone and message key, and a scope to its display copy. Pure and
// feature-agnostic in the sense that matters here: no fetch, no navigation, so
// both `RunnerList` and `RunnerDetail` can import it without either owning the
// other's copy.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ClaudeRunnerScope, ClaudeRunnerState } from "@/lib/types";

import type { BadgeTone } from "@/components/atoms/Badge";

const STATE_KEYS: Readonly<Record<ClaudeRunnerState, string>> = {
  provisioning: CONSOLE_MESSAGE_KEYS.runners_state_provisioning,
  awaiting_authorization: CONSOLE_MESSAGE_KEYS.runners_state_awaiting_authorization,
  exchanging: CONSOLE_MESSAGE_KEYS.runners_state_exchanging,
  ready: CONSOLE_MESSAGE_KEYS.runners_state_ready,
  linked: CONSOLE_MESSAGE_KEYS.runners_state_linked,
  failed: CONSOLE_MESSAGE_KEYS.runners_state_failed,
  expired: CONSOLE_MESSAGE_KEYS.runners_state_expired,
};

const STATE_TONES: Readonly<Record<ClaudeRunnerState, BadgeTone>> = {
  provisioning: "info",
  awaiting_authorization: "warning",
  exchanging: "info",
  ready: "warning",
  linked: "success",
  failed: "danger",
  expired: "neutral",
};

export function stateLabel(state: ClaudeRunnerState): string {
  return t(STATE_KEYS[state]);
}

export function stateTone(state: ClaudeRunnerState): BadgeTone {
  return STATE_TONES[state];
}

/**
 * `Badge tone="info"` for the platform account, `"neutral"` for a tenant's —
 * the platform account is the common case and deliberately gets the quieter
 * treatment nowhere else, since an operator scanning the table is looking for
 * the tenant rows that override it.
 */
export function scopeTone(scope: ClaudeRunnerScope): BadgeTone {
  return scope.type === "tenant" ? "neutral" : "info";
}

/**
 * Whose account a runner belongs to, in the operator's own terms.
 *
 * Only `global` and `tenant` are rendered specially — those are the two shapes
 * this console's provisioning form can produce (see `RunnerProvisionForm`).
 * `application`/`user` scopes are real on the wire (the full `CredentialScope`
 * union) but nothing here creates one; they fall back to the platform label
 * rather than rendering nothing, so a runner provisioned some other way still
 * shows an account rather than a blank cell.
 */
export function scopeLabel(scope: ClaudeRunnerScope): string {
  if (scope.type === "tenant") {
    return t(CONSOLE_MESSAGE_KEYS.runners_scope_tenant, { tenant_id: scope.external_tenant_id });
  }
  return t(CONSOLE_MESSAGE_KEYS.runners_scope_platform);
}
