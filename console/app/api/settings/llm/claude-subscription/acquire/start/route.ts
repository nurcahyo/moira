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
// ============================================================================
// THE PTY GATE
// ============================================================================
//
// `lib/claude-cli.ts::ptyIsAvailable()` must ALSO be true before spawning —
// see its own doc comment. It is an honest capability statement, not a
// per-host probe: two dependency-free ways to give the child a real terminal
// were investigated and both measured to not work (see that module's
// header), so this answers `false` unconditionally today. Checked SECOND,
// after the opt-in gate — refusing here means no job is ever created whose
// `authorization_url` could only ever stay `null`.
//
// Re-checks the session itself — `app/api/**` is outside every route group;
// see `app/api/llm/connect-vllm/route.ts` for the fuller rationale.

import { ptyIsAvailable, startClaudeCliJob } from "@/lib/claude-cli";
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

    // Refuse BEFORE spawning anything, rather than start a job that can only
    // ever expire: `ptyIsAvailable()` is an honest capability statement, not
    // a per-host probe — see its own doc comment and this module's header
    // for the investigation (plain pipe, then `script(1)`, both measured and
    // ruled out) that pins it at `false` today. Starting a job anyway would
    // mean five silent minutes before the SAME keyed failure this check
    // gives instantly — the exact opacity #269 exists to remove, one layer up.
    if (!ptyIsAvailable()) {
      return Response.json(
        {
          error: {
            code: "claude_cli_pty_unavailable",
            message_key: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_pty_unavailable,
          },
        },
        { status: 409, headers: NO_STORE },
      );
    }

    const { jobId } = startClaudeCliJob();

    // `authorization_url` is always `null` at start time — the child has not
    // produced anything yet, and cannot have: it was spawned in this same
    // call, above. The browser learns the URL (on a host where
    // `ptyIsAvailable()` is ever `true`) from a subsequent
    // `GET .../acquire/status?job=…` poll.
    return Response.json({ job_id: jobId, authorization_url: null }, { status: 200, headers: NO_STORE });
  });
}
