// `GET /api/settings/llm/claude-subscription/acquire/status?job=…` — Mode A,
// phase 2 of 2 (issue #269): poll a job `start` registered. On the FIRST poll
// that observes the child exited successfully, stores the minted token
// through the SAME chain the paste endpoint uses (`connectClaudeSubscription`)
// and caches the result so a repeated poll answers from the cache rather than
// storing twice.
//
// See `lib/claude-cli.ts` for the job registry and its header for the two
// things investigated before building this: whether an already-authenticated
// CLI session's credential could be reused instead (no documented,
// non-private way to do that was found), and why the shipped executor cannot
// actually render `claude setup-token`'s interactive UI over a plain pipe
// (verified locally; see the same header).
//
// ============================================================================
// WHY STORAGE HAPPENS HERE AND NOT IN A COMPLETION CALLBACK ON THE CHILD
// ============================================================================
//
// `lib/claude-cli.ts` deliberately does not talk to Moira (see its header).
// Storing the token also needs a `MoiraClient` authenticated as the signed-in
// operator, and `withConsoleSession` mints that FRESH per request from the
// caller's own session cookie. A completion handler attached at job-start
// time would have to close over the START request's client and hope it is
// still valid minutes later, when the human has actually finished the
// browser step — this handler instead uses the STATUS request's own,
// always-freshly-authenticated client, which is only ever as old as the poll
// that is currently running.
//
// ============================================================================
// EVERY FAILURE IS KEYED; THE RAW TOKEN NEVER REACHES THE BROWSER
// ============================================================================
//
// `job.state.kind === "pending_storage"` is the only branch that ever reads
// the raw candidate (via `takeClaudeCliJobToken`), and it is fed straight
// into `resolveSubscriptionToken` / `connectClaudeSubscription` — never
// returned in a response body. Every terminal outcome below is a keyed
// `message_key`, matching PR #263's own "never a raw stderr dump" rule.

import {
  peekClaudeCliJob,
  setClaudeCliJobResult,
  setClaudeCliJobStorageFailed,
  takeClaudeCliJobToken,
  type ClaudeCliJobFailureReason,
} from "@/lib/claude-cli";
import {
  connectClaudeSubscription,
  isClaudeSubscriptionError,
  resolveSubscriptionToken,
} from "@/lib/claude-subscription";
import { badRequest, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/** A generous bound on a job id, before it is even looked up. */
const MAX_JOB_ID_LENGTH = 200;

/** Every process-level failure reason gets its own keyed, actionable message — never a raw stderr dump. */
const CLI_FAILURE_MESSAGE_KEYS: Readonly<Record<ClaudeCliJobFailureReason, string>> = {
  binary_missing: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_binary_missing,
  not_signed_in: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in,
  timeout: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_timeout,
  output_too_large: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_output_too_large,
  non_zero_exit: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_failed,
  invalid_output: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_output,
};

function processFailed(reason: ClaudeCliJobFailureReason): Response {
  return Response.json(
    { error: { code: "claude_cli_mint_failed", message_key: CLI_FAILURE_MESSAGE_KEYS[reason] } },
    { status: 409, headers: NO_STORE },
  );
}

function storageFailed(messageKey: string): Response {
  return Response.json(
    { error: { code: "claude_subscription_failed", message_key: messageKey } },
    { status: 409, headers: NO_STORE },
  );
}

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client, env }) => {
    if (!env.allowLocalCliCredentials) {
      return Response.json(
        {
          error: {
            code: "claude_cli_acquisition_disabled",
            message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_disabled,
          },
        },
        { status: 403, headers: NO_STORE },
      );
    }

    const jobId = new URL(request.url).searchParams.get("job");
    if (jobId === null || jobId.trim() === "" || jobId.length > MAX_JOB_ID_LENGTH) {
      return badRequest(CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_job);
    }

    const snapshot = peekClaudeCliJob(jobId);
    switch (snapshot.kind) {
      case "not_found":
        return Response.json(
          {
            error: {
              code: "claude_cli_job_not_found",
              message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_job_not_found,
            },
          },
          { status: 404, headers: NO_STORE },
        );

      case "running":
        return Response.json(
          { status: "running", authorization_url: snapshot.authorizationUrl },
          { status: 200, headers: NO_STORE },
        );

      case "process_failed":
        return processFailed(snapshot.reason);

      case "storage_failed":
        return storageFailed(snapshot.messageKey);

      case "succeeded":
        return Response.json({ status: "succeeded", ...snapshot.result }, { status: 200, headers: NO_STORE });

      case "pending_storage": {
        // Claims the raw candidate exactly once. A concurrent poll that loses
        // the race gets `null` here (already claimed) and is told to keep
        // polling — the SAME shape `peekClaudeCliJob` already returns while
        // the child is still running, since from the browser's point of view
        // both mean "not done yet, ask again".
        const token = takeClaudeCliJobToken(jobId);
        if (token === null) {
          return Response.json(
            { status: "running", authorization_url: null },
            { status: 200, headers: NO_STORE },
          );
        }

        // The SAME narrowing a pasted token goes through — a CLI-minted
        // value is not exempt from being well-formed.
        const resolved = resolveSubscriptionToken(token);
        if (!resolved.ok) {
          setClaudeCliJobStorageFailed(jobId, resolved.messageKey);
          return storageFailed(resolved.messageKey);
        }

        try {
          const result = await connectClaudeSubscription(client, { accessToken: resolved.token });
          const payload = {
            provider_id: result.providerId,
            credential_id: result.credentialId,
            outcome: result.outcome,
          };
          setClaudeCliJobResult(jobId, payload);
          return Response.json({ status: "succeeded", ...payload }, { status: 200, headers: NO_STORE });
        } catch (error) {
          if (!isClaudeSubscriptionError(error)) throw error;
          setClaudeCliJobStorageFailed(jobId, error.messageKey);
          return storageFailed(error.messageKey);
        }
      }
    }
  });
}
