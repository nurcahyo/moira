// `POST /api/settings/llm/claude-subscription` — store a long-lived Claude
// subscription token (`claude setup-token` output) as an `oauth2` provider
// credential, through Moira's EXISTING `POST /api/v1/admin/provider-credentials`.
//
// No new Moira HTTP endpoint. See `docs/claude-subscription-sidecar.md` for the
// architecture and `lib/claude-subscription.ts` for the chain this calls.
//
// ============================================================================
// THE TOKEN'S ONLY TRIP THROUGH A PROCESS THAT ISN'T MOIRA'S OWN STORAGE
// ============================================================================
//
// The plaintext exists in this handler, and in `connectClaudeSubscription`
// underneath it, for the duration of one request. It is read out of the body,
// narrowed by `resolveSubscriptionToken`, handed to Moira, and never logged,
// echoed into an error, or reflected back in the response — the response below
// is a three-field projection (`provider_id`, `credential_id`, `outcome`), never
// a spread of whatever Moira answered with, so `masked_secret` and
// `secret_fingerprint` (themselves not the raw token, but still not this
// screen's business) never cross either.
//
// Re-checks the session itself — `app/api/**` is outside every route group; see
// `app/api/llm/connect-vllm/route.ts` for the fuller rationale, which applies
// here unchanged.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import {
  connectClaudeSubscription,
  isClaudeSubscriptionError,
  resolveSubscriptionToken,
} from "@/lib/claude-subscription";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) {
      return badRequest(CONSOLE_MESSAGE_KEYS.claude_subscription_request_body_invalid);
    }

    const resolved = resolveSubscriptionToken(body["token"]);
    if (!resolved.ok) return badRequest(resolved.messageKey);

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
