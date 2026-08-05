// `GET /api/llm/providers` — everything `/settings/llm` renders.
// `POST /api/llm/providers` — register one provider by hand.
//
// ============================================================================
// ORDINARY ADMINISTRATION, NOT SETUP
// ============================================================================
//
// Every handler in this family sits behind `withConsoleSession`, the same gate
// `app/api/admins/**` uses, and none of them appears on
// `route-handler-session.test.ts`'s exemption list. LLM configuration is not
// first-run bootstrap: there is an `admin_identities` grant to check against, an
// operator to be signed in as, and nothing here creates the first one. A
// deployment whose first admin has not been claimed must not be able to reach
// any of it.
//
// ============================================================================
// ONLY `open_ai_compatible` IS OFFERED, AND THE OMISSION IS THE DECISION
// ============================================================================
//
// `ProviderType` has eight values. Seven of them are a vendor with its own
// credential ceremony — an Azure deployment needs `endpoint` inside the
// credential secret, Anthropic and Gemini take no `base_url` at all — and a form
// that offered the enum without those ceremonies would create rows that cannot
// be completed. This screen registers endpoints the operator runs, which is the
// arm that needs a `base_url` and a plain API key, and the arm
// `assertLlmProviderCreateIsSafe` refuses without one: the compatible arm with
// no base URL does not fail, it falls back to the vendor default, and sends the
// operator's prompts to a third party.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { canonicalOpenAiBaseUrl, loadLlmSettings } from "@/lib/llm-settings";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const view = await loadLlmSettings(client);
    return Response.json(view, { headers: { "cache-control": "no-store" } });
  });
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const displayName = typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
    if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.llm_display_name_required);

    const resolved = canonicalOpenAiBaseUrl(body["base_url"]);
    if (!resolved.ok) return badRequest(resolved.messageKey);

    const record = await client.createProvider(
      {
        provider_type: "open_ai_compatible",
        display_name: displayName,
        base_url: resolved.baseUrl,
      },
      // Derived from the provider's own identity, so a double-submit replays
      // rather than landing two rows routing then has to choose between.
      { idempotencyKey: `llm-provider:${resolved.baseUrl}` },
    );

    return Response.json(
      { id: record.id },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
