// `DELETE /api/llm/providers/{id}/models/{modelId}` — disable one model.
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
// Re-checks the session itself — `app/api/**` is outside every route group.

import { notFound, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; modelId: string }> },
): Promise<Response> {
  const { id, modelId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const provider = await client.getProvider(id);
    const page = await client.listProviderModels(provider.id, { limit: LIST_PAGE_LIMIT });
    const model = page.data.find((row) => row.id === modelId);
    if (model === undefined) return notFound(CONSOLE_MESSAGE_KEYS.llm_model_not_found);

    const record = await client.disableProviderModel(model.id, ifMatchFor(model));
    return Response.json({ id: record.id }, { headers: { "cache-control": "no-store" } });
  });
}
