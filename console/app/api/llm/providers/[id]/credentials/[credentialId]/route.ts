// `DELETE /api/llm/providers/{id}/credentials/{credentialId}` — disable one
// credential row.
//
// The same ownership check the model path makes, for the same reason: Moira's
// disable is flat (`POST /provider-credentials/{id}/disable`), so the console
// verifies the row belongs to the provider named in the path before acting, and
// answers a console-decided 404 when it does not.
//
// The list is filtered SERVER-SIDE by `provider_id` — a documented query
// parameter on this operation — so other providers' credential rows never enter
// this process at all, and the response says nothing about the secret: the id is
// echoed back and nothing else.
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
  context: { params: Promise<{ id: string; credentialId: string }> },
): Promise<Response> {
  const { id, credentialId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const provider = await client.getProvider(id);
    const page = await client.listProviderCredentials({
      providerId: provider.id,
      limit: LIST_PAGE_LIMIT,
    });
    const row = page.data.find(
      (candidate) => candidate.id === credentialId && candidate.provider_id === provider.id,
    );
    if (row === undefined) return notFound(CONSOLE_MESSAGE_KEYS.llm_key_row_not_found);

    const record = await client.disableProviderCredential(row.id, ifMatchFor(row));
    return Response.json({ id: record.id }, { headers: { "cache-control": "no-store" } });
  });
}
