// `POST /api/settings/llm/claude-subscription/acquire/start` — Mode A, phase
// 1 of 2 (issue #269): spawn the local `claude` CLI DETACHED from this
// request and return its job id immediately. Never waits for the child.
//
// See `lib/claude-cli.ts` for the job registry this calls, its header for why
// the previous single-request design (PR #263) could not work, and
// `.../acquire/status/route.ts` for phase 2 — the poll that eventually stores
// the minted token through the SAME chain the paste endpoint uses
// (`connectClaudeSubscription`).
//
// ============================================================================
// NO REQUEST BODY IS EVER READ
// ============================================================================
//
// This handler takes no input from the caller at all — not a body, not a
// query string. `startClaudeCliJob()` spawns a FIXED command with a FIXED
// argv; there is nothing for a request to influence even if it tried.
//
// ============================================================================
// THE OPT-IN GATE
// ============================================================================
//
// `env.allowLocalCliCredentials` (`CONSOLE_ALLOW_LOCAL_CLI_CREDENTIALS`, see
// `lib/env.ts`) must be true before this handler spawns anything. Checked
// FIRST — a disabled deployment must never even attempt to spawn a process.
//
// Re-checks the session itself — `app/api/**` is outside every route group;
// see `app/api/llm/connect-vllm/route.ts` for the fuller rationale.

import { startClaudeCliJob } from "@/lib/claude-cli";
import { withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ env }) => {
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

    const { jobId } = startClaudeCliJob();

    // `authorization_url` is always `null` at start time — the child has not
    // produced anything yet, and cannot have: it was spawned in this same
    // call, above. The browser learns the URL (if the running executor ever
    // captures one — see `lib/claude-cli.ts`'s header) from a subsequent
    // `GET .../acquire/status?job=…` poll.
    return Response.json({ job_id: jobId, authorization_url: null }, { status: 200, headers: NO_STORE });
  });
}
