// `DELETE /api/llm/providers/{id}/models/{modelId}` — disable one model.
// `POST   /api/llm/providers/{id}/models/{modelId}` — enable it again.
//
// ============================================================================
// THE OWNERSHIP CHECK IS THE POINT OF THE NESTED PATH
// ============================================================================
//
// Moira's disable operation is FLAT — `POST /api/v1/admin/provider-models/{id}/disable`
// — so a caller holding any model id can disable it with no provider id at all.
// This console refuses to be that caller. The model is looked up among the
// models of the provider named in the path, and a model id that does not appear
// there is a console-decided 404 rather than a write.
//
// It is also where the `If-Match` version comes from. There is no
// `GET /provider-models/{id}` in the spec, so the list is the only way to read
// the current version, and fabricating one would defeat the single thing
// stopping this from racing a concurrent patch.
//
// ============================================================================
// "DISABLE IS REVERSIBLE" IS A CLAIM, AND THIS IS WHAT MAKES IT TRUE
// ============================================================================
//
// The screen's undo is disable rather than delete, and the reason on record is
// that a disabled row stays readable and can be brought back. It could not be:
// `enableProviderModel` existed on the client with no caller anywhere, so the
// only route out of `status = 'disabled'` was a SQL prompt. Meanwhile Moira's
// routing joins `provider_models` on `pm.status = 'active'`, and the model's
// `model_key` still occupies the partial unique index — so re-adding it was a
// unique violation with no mapping, i.e. an opaque 500. Disable was a one-way
// door. `POST` is the door back.
//
// Both methods run the same lookup, so neither can act on a row the named
// provider does not own.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { notFound, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor, type MoiraClient } from "@/lib/moira-client";
import type { ProviderModelRecord } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/** The model, only if it belongs to the provider named in the path. */
async function ownedModel(
  client: MoiraClient,
  providerId: string,
  modelId: string,
): Promise<ProviderModelRecord | null> {
  const provider = await client.getProvider(providerId);
  const page = await client.listProviderModels(provider.id, { limit: LIST_PAGE_LIMIT });
  return page.data.find((row) => row.id === modelId) ?? null;
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; modelId: string }> },
): Promise<Response> {
  const { id, modelId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const model = await ownedModel(client, id, modelId);
    if (model === null) return notFound(CONSOLE_MESSAGE_KEYS.llm_model_not_found);

    const record = await client.disableProviderModel(model.id, ifMatchFor(model));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string; modelId: string }> },
): Promise<Response> {
  const { id, modelId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const model = await ownedModel(client, id, modelId);
    if (model === null) return notFound(CONSOLE_MESSAGE_KEYS.llm_model_not_found);

    // Already active: answer with the row rather than writing. Enabling an
    // active row would burn a version for nothing and turn a double-click into a
    // lost `If-Match` race for whoever clicked next.
    if (model.status === "active") {
      return Response.json({ id: model.id }, { headers: NO_STORE });
    }
    const record = await client.enableProviderModel(model.id, ifMatchFor(model));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}
