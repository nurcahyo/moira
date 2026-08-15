// `GET /api/playground/providers/{id}/models` — the provider/model picker's
// dependent second level (`GET /api/v1/admin/providers/{provider_id}/models`),
// fetched client-side once the operator has chosen a provider. Routes and
// providers themselves are loaded server-side by
// `app/(console)/playground/page.tsx`, same as `/flows` loads its agent-profile
// picker — this one endpoint exists because the model list depends on a
// choice the operator makes in the browser, after the page has already
// rendered.

import { withConsoleSession } from "@/lib/console-api";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const list = await client.listProviderModels(id, { limit: 200 });
    return Response.json(list, { headers: NO_STORE });
  });
}
