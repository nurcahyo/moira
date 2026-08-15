// `POST /api/settings/llm/claude-api-key` — Mode B: store an official
// `sk-ant-…` Anthropic Console API key as an `api_key` provider credential,
// through Moira's EXISTING `POST /api/v1/admin/provider-credentials`.
//
// The one FULLY sanctioned mode of the three: an API key is the credential
// shape `RuntimeFactory`'s `ProviderType::Anthropic` arm actually accepts
// today (`require_credential_type(..., &[CredentialType::ApiKey])`,
// `src/orchestration/runtime_factory.rs:112`), so this is not storage-only the
// way the subscription token is — a provider/model/routing chain built on top
// of the row this creates can execute real completions against the Messages
// API. This screen only stores the credential; wiring a model and a routing
// policy is the existing generic `/settings/llm` chain, same as any other
// provider.
//
// See `lib/claude-subscription.ts` for `connectClaudeApiKey` and
// `resolveAnthropicApiKey`, and that module's header for why this writes to a
// DIFFERENT dedicated provider row than the subscription token does.
//
// ============================================================================
// THE KEY'S ONLY TRIP THROUGH A PROCESS THAT ISN'T MOIRA'S OWN STORAGE
// ============================================================================
//
// Same shape as `app/api/settings/llm/claude-subscription/route.ts`: the
// plaintext exists in this handler and in `connectClaudeApiKey` underneath it
// for the duration of one request, is never logged or echoed, and the
// response below is a three-field projection, never a spread of whatever
// Moira answered with.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import {
  connectClaudeApiKey,
  isClaudeSubscriptionError,
  resolveAnthropicApiKey,
} from "@/lib/claude-subscription";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) {
      return badRequest(CONSOLE_MESSAGE_KEYS.claude_api_key_request_body_invalid);
    }

    const resolved = resolveAnthropicApiKey(body["api_key"]);
    if (!resolved.ok) return badRequest(resolved.messageKey);

    try {
      const result = await connectClaudeApiKey(client, { apiKey: resolved.apiKey });
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
        { error: { code: "claude_api_key_failed", message_key: error.messageKey } },
        { status: 409, headers: NO_STORE },
      );
    }
  });
}
