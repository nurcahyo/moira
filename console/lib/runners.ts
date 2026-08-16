// @server-only
//
// Containerised Claude runners (issue #275, workstream R3 of #272) — the
// console's business logic over `/api/v1/admin/runners*`.
//
// ============================================================================
// THE ONE SHARP EDGE THIS MODULE EXISTS TO HANDLE: DELETE NEEDS A FRESH ETag
// ============================================================================
//
// `GET /runners/{id}` WRITES — it refreshes the row from the runner service
// before answering, so the returned `version` (and any `ETag` built from it)
// ADVANCES ON EVERY POLL. A page that read a runner five seconds ago and then
// asks to delete it is very likely holding a version Moira has already moved
// past, and `deleteRunner` REQUIRES `If-Match`. Sending the stale one is not
// merely "probably fine" — it is the exact case `If-Match` exists to catch,
// and it would catch it: `409 resource_version_conflict`.
//
// `deleteRunnerSafely` is the fix, in one place rather than three (the runner
// list, the runner detail page, and any future surface that deletes a
// runner): it re-reads the runner IMMEDIATELY before deleting, uses that
// fresh version, and on a `resource_version_conflict` re-reads again rather
// than retrying with the same stale value — bounded, so a persistently
// conflicting caller (another operator racing this one) fails loudly instead
// of spinning.
//
// ============================================================================
// WHAT THIS MODULE DOES NOT DO
// ============================================================================
//
// It has no opinion about `runner_token_unavailable` — that is a UI decision
// ("say so plainly, offer delete-and-reprovision, never a retry button"), made
// in `modules/runners/RunnerDetail.tsx` against the failure a `finalizeRunner`
// call surfaces. This module's job stops at the Moira boundary.

import "server-only";

import { isMoiraRequestError } from "./errors";
import type { MoiraClient } from "./moira-client";
import { ifMatchFor } from "./moira-client";
import type {
  ClaudeRunnerProvisionRequest,
  ClaudeRunnerRecord,
  ClaudeRunnerScope,
  JsonValue,
} from "./types";

/** How many times `deleteRunnerSafely` will re-read after a version conflict. */
const DELETE_RETRY_ATTEMPTS = 3;

/**
 * `POST /api/v1/admin/runners`, with a deterministic idempotency key.
 *
 * Derived from `label` alone: Moira's own `duplicate_runner_label` (409)
 * already refuses a second runner under the same label, so a key scoped to it
 * makes a genuine double-submit of the SAME click replay with the row that was
 * created rather than racing that refusal with a fresh one.
 */
export async function provisionRunner(
  client: MoiraClient,
  request: {
    readonly label: string;
    readonly ttlSeconds?: number;
    readonly scope?: ClaudeRunnerScope | null;
    readonly metadata?: JsonValue;
  },
): Promise<ClaudeRunnerRecord> {
  const body: ClaudeRunnerProvisionRequest = {
    label: request.label,
    ...(request.ttlSeconds === undefined ? {} : { ttl_seconds: request.ttlSeconds }),
    ...(request.scope === undefined ? {} : { scope: request.scope }),
    ...(request.metadata === undefined ? {} : { metadata: request.metadata }),
  };
  return client.provisionRunner(body, { idempotencyKey: `runner-provision:${request.label}` });
}

/** Why `deleteRunnerSafely` gave up. */
export type DeleteRunnerFailure =
  | { readonly kind: "not_found" }
  | { readonly kind: "conflict_exhausted" };

export type DeleteRunnerOutcome =
  | { readonly ok: true }
  | { readonly ok: false; readonly failure: DeleteRunnerFailure };

/**
 * Delete a runner against a version read IMMEDIATELY before the call, retrying
 * the read-then-delete pair (never a bare retry of the delete) on
 * `resource_version_conflict`. See this module's header for why a version read
 * any earlier — including one already on the caller's screen — is not safe to
 * reuse here.
 */
export async function deleteRunnerSafely(
  client: MoiraClient,
  id: string,
): Promise<DeleteRunnerOutcome> {
  for (let attempt = 0; attempt < DELETE_RETRY_ATTEMPTS; attempt += 1) {
    let fresh: ClaudeRunnerRecord;
    try {
      fresh = await client.getRunner(id);
    } catch (error) {
      if (isNotFound(error)) return { ok: false, failure: { kind: "not_found" } };
      throw error;
    }

    try {
      await client.deleteRunner(id, ifMatchFor(fresh));
      return { ok: true };
    } catch (error) {
      if (isNotFound(error)) return { ok: false, failure: { kind: "not_found" } };
      if (isVersionConflict(error)) continue;
      throw error;
    }
  }
  return { ok: false, failure: { kind: "conflict_exhausted" } };
}

/** The Moira `code` on a thrown `MoiraRequestError`, or `null` for anything else. */
function errorCodeOf(error: unknown): string | null {
  if (!isMoiraRequestError(error)) return null;
  return error.moiraError.kind === "api" ? error.moiraError.code : null;
}

function isNotFound(error: unknown): boolean {
  return errorCodeOf(error) === "runner_not_found";
}

function isVersionConflict(error: unknown): boolean {
  return errorCodeOf(error) === "resource_version_conflict";
}
