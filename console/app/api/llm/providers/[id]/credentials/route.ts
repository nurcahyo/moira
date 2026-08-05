// `POST /api/llm/providers/{id}/credentials` — create the credential row a
// provider cannot run without.
//
// ============================================================================
// THE SECRET TRAVELS ONE WAY AND IS NEVER SEEN AGAIN
// ============================================================================
//
// The plaintext exists in this process for the duration of one request. It is
// read out of the body, handed to `apiKeyCredentialSecret`, and sent. Nothing
// here logs it, echoes it, stores it, or puts it in an error. The RESPONSE is a
// four-field projection — id, kind, status, version — built by hand rather than
// by spreading `CredentialRecord`, because a spread would carry `masked_secret`
// and `secret_fingerprint` across the boundary, and the second of those is a
// stable identifier for a secret value.
//
// ============================================================================
// A BLANK KEY IS THE NORMAL CASE, AND IT IS NOT AN EMPTY KEY
// ============================================================================
//
// A local vLLM ignores `Authorization` entirely, so an operator leaving the
// field blank is right about their endpoint. They are not right about Moira:
// routing resolves a credential ROW before it builds any request, and a provider
// without one fails `404 credential_not_found` — an error that reads as "your
// key is wrong" when the truth is "there is no key at all".
//
// So a blank field becomes a SERVER-GENERATED placeholder, and the operator is
// never asked to invent one. `apiKeyCredentialSecret` refuses an empty string
// outright, which is the backstop under this branch rather than a duplicate of
// it.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { generatedPlaceholderKey } from "@/lib/llm-settings";
import { apiKeyCredentialSecret } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const supplied = typeof body["api_key"] === "string" ? body["api_key"].trim() : "";

    // Resolved server-side from the path, then used as the write's target. A
    // `provider_id` taken from the body would let whoever sent it attach a key
    // to a provider they were never shown.
    const provider = await client.getProvider(id);

    const record = await client.createProviderCredential(
      {
        provider_id: provider.id,
        credential_type: "api_key",
        scope: { type: "global" },
        // `endpoint` MUST be absent, not null: `CredentialSecret` is untagged
        // and `{api_key, endpoint: null}` matches the azure arm as well, which
        // makes the request permanently ambiguous with no field named.
        secret: apiKeyCredentialSecret(supplied === "" ? generatedPlaceholderKey() : supplied),
      },
      { idempotencyKey: `llm-credential:${provider.id}:${crypto.randomUUID()}` },
    );

    return Response.json(
      // Projected, never spread.
      {
        id: record.id,
        kind: record.credential_type,
        status: record.status,
        version: record.version,
      },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
