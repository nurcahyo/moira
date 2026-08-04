// `PATCH /api/llm/providers/{id}` — correct a provider's endpoint or its name.
// `DELETE /api/llm/providers/{id}` — disable it.
//
// ============================================================================
// A WRONG ENDPOINT IS THE MOST LIKELY FIRST MISTAKE
// ============================================================================
//
// So both directions out of it exist: PATCH the address, or disable the row. An
// operator who can create a provider and cannot undo it is stuck with a row that
// routing may already be pointing at.
//
// `DELETE` on this path DISABLES rather than deletes, and the verb is the one
// Next routes by rather than a claim about Moira's own semantics: the LLM
// operation registry has no delete for any of these families, deliberately, and
// disable is reversible while a deletion that cascaded into a routing policy
// would not be. Nothing here is destroyed; a disabled provider stops being
// eligible and stays readable.
//
// ============================================================================
// `provider_type` IS NEVER PATCHED, AND THE GUARD SAYS WHY
// ============================================================================
//
// `ProviderPatchRequest` does not declare it and the schema is
// `additionalProperties: false`, so a body carrying it is a flat 400 naming no
// field. `assertLlmProviderPatchIsSafe` refuses it inside the client; this
// handler never builds one, so the operator is never shown a validation failure
// for a request that is not invalid but impossible.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { canonicalOpenAiBaseUrl } from "@/lib/llm-settings";
import { ifMatchFor } from "@/lib/moira-client";
import type { ProviderPatchRequest } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function PATCH(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const patch: { -readonly [K in keyof ProviderPatchRequest]: ProviderPatchRequest[K] } = {};

    if ("display_name" in body) {
      const displayName =
        typeof body["display_name"] === "string" ? body["display_name"].trim() : "";
      if (displayName === "") return badRequest(CONSOLE_MESSAGE_KEYS.llm_display_name_required);
      patch.display_name = displayName;
    }
    if ("base_url" in body) {
      const resolved = canonicalOpenAiBaseUrl(body["base_url"]);
      if (!resolved.ok) return badRequest(resolved.messageKey);
      patch.base_url = resolved.baseUrl;
    }
    if (Object.keys(patch).length === 0) {
      return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);
    }

    // The version is READ FROM MOIRA, not taken from the body. `If-Match` is
    // optimistic concurrency rather than an authorization input, but reading the
    // record here also confirms the path id names a provider that exists before
    // anything is written.
    const current = await client.getProvider(id);
    const record = await client.patchProvider(current.id, patch, ifMatchFor(current));
    return Response.json({ id: record.id }, { headers: { "cache-control": "no-store" } });
  });
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const current = await client.getProvider(id);
    const record = await client.disableProvider(current.id, ifMatchFor(current));
    return Response.json({ id: record.id }, { headers: { "cache-control": "no-store" } });
  });
}
