// `POST /api/settings/llm/claude-subscription/acquire` — Mode A: mint a Claude
// subscription token by running the local `claude` CLI, then store it through
// the SAME chain the paste endpoint uses (`connectClaudeSubscription`).
//
// See `lib/claude-cli.ts` for the process-execution boundary this calls and
// `lib/claude-subscription.ts` for the storage chain. No new Moira HTTP
// endpoint — this is one more way to obtain the value
// `POST /api/settings/llm/claude-subscription` already knew how to store.
//
// ============================================================================
// NO REQUEST BODY IS EVER READ
// ============================================================================
//
// This handler takes no input from the caller at all — not a body, not a
// query string. `mintClaudeSetupToken()` runs a FIXED command with a FIXED
// argv; there is nothing for a request to influence even if it tried. That is
// the point: "no user-controlled input reaches the command line" is not a
// validation this handler performs, it is a shape this handler HAS NO WAY to
// violate.
//
// ============================================================================
// THE OPT-IN GATE
// ============================================================================
//
// `env.allowLocalCliCredentials` (`CONSOLE_ALLOW_LOCAL_CLI_CREDENTIALS`, see
// `lib/env.ts`) must be true before this handler runs the CLI at all. Checked
// FIRST, before anything else — a disabled deployment must never even attempt
// to spawn a process.
//
// Re-checks the session itself — `app/api/**` is outside every route group;
// see `app/api/llm/connect-vllm/route.ts` for the fuller rationale.

import { mintClaudeSetupToken, type ClaudeCliMintReason } from "@/lib/claude-cli";
import {
  connectClaudeSubscription,
  isClaudeSubscriptionError,
  resolveSubscriptionToken,
} from "@/lib/claude-subscription";
import { withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/** Every CLI failure reason gets its own keyed, actionable message — never a raw stderr dump. */
const CLI_FAILURE_MESSAGE_KEYS: Readonly<Record<ClaudeCliMintReason, string>> = {
  binary_missing: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_binary_missing,
  not_signed_in: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_not_signed_in,
  timeout: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_timeout,
  output_too_large: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_output_too_large,
  non_zero_exit: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_failed,
  invalid_output: CONSOLE_MESSAGE_KEYS.claude_subscription_cli_invalid_output,
};

export async function POST(request: Request): Promise<Response> {
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

    const minted = await mintClaudeSetupToken();
    if (!minted.ok) {
      return Response.json(
        {
          error: {
            code: "claude_cli_mint_failed",
            message_key: CLI_FAILURE_MESSAGE_KEYS[minted.reason],
          },
        },
        { status: 409, headers: NO_STORE },
      );
    }

    // The SAME narrowing a pasted token goes through — a CLI-minted value is
    // not exempt from being well-formed. `minted.token` never appears in the
    // response either way: on refusal this returns only a message key, and on
    // success `connectClaudeSubscription`'s own projection (below) is the same
    // three-field shape the paste endpoint returns, never the token itself.
    const resolved = resolveSubscriptionToken(minted.token);
    if (!resolved.ok) {
      return Response.json(
        { error: { code: "invalid_request", message_key: resolved.messageKey } },
        { status: 409, headers: NO_STORE },
      );
    }

    try {
      const result = await connectClaudeSubscription(client, { accessToken: resolved.token });
      return Response.json(
        {
          provider_id: result.providerId,
          credential_id: result.credentialId,
          outcome: result.outcome,
        },
        { status: 200, headers: NO_STORE },
      );
    } catch (error) {
      if (!isClaudeSubscriptionError(error)) throw error;
      return Response.json(
        { error: { code: "claude_subscription_failed", message_key: error.messageKey } },
        { status: 409, headers: NO_STORE },
      );
    }
  });
}
