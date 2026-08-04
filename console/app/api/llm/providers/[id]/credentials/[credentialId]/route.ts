// `DELETE /api/llm/providers/{id}/credentials/{credentialId}` — disable one
// credential row.
// `POST` on the same path — enable it again.
//
// The same ownership check the model path makes, for the same reason: Moira's
// disable is flat (`POST /provider-credentials/{id}/disable`), so the console
// verifies the row belongs to the provider named in the path before acting, and
// answers a console-decided 404 when it does not.
//
// The list is filtered SERVER-SIDE by `provider_id` — a documented query
// parameter on this operation — so other providers' credential rows never enter
// this process at all, and the response says nothing about the secret: the id is
// echoed back and nothing else. That holds for the enable path too, which reads
// the same row and returns the same one field.
//
// ============================================================================
// ENABLE EXISTS BECAUSE DISABLE CLAIMED TO BE REVERSIBLE
// ============================================================================
//
// `runtime.rs` resolves a provider credential whose status is `active`. A
// disabled row therefore fails a completion with `404 credential_not_found` —
// the same error a MISSING row produces, which is the error this whole screen
// was built to stop an operator meeting. `enableProviderCredential` shipped on
// the client with no caller, so the only undo the screen offered had no undo of
// its own. This is it.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { notFound, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor, type MoiraClient } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/**
 * The row shape, derived from the client rather than imported.
 *
 * `CredentialRecord` lives in `lib/moira-credential-types.ts`, the server-only
 * home of every secret-carrying DTO, and this module has no need of a second
 * import edge into it: what it wants is exactly "an element of what
 * `listProviderCredentials` returns", which is what this says.
 */
type OwnedCredential = Awaited<ReturnType<MoiraClient["listProviderCredentials"]>>["data"][number];

/** The credential row, only if it belongs to the provider named in the path. */
async function ownedCredential(
  client: MoiraClient,
  providerId: string,
  credentialId: string,
): Promise<OwnedCredential | null> {
  const provider = await client.getProvider(providerId);
  const page = await client.listProviderCredentials({
    providerId: provider.id,
    limit: LIST_PAGE_LIMIT,
  });
  return (
    page.data.find(
      (candidate) => candidate.id === credentialId && candidate.provider_id === provider.id,
    ) ?? null
  );
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; credentialId: string }> },
): Promise<Response> {
  const { id, credentialId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const row = await ownedCredential(client, id, credentialId);
    if (row === null) return notFound(CONSOLE_MESSAGE_KEYS.llm_key_row_not_found);

    const record = await client.disableProviderCredential(row.id, ifMatchFor(row));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string; credentialId: string }> },
): Promise<Response> {
  const { id, credentialId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const row = await ownedCredential(client, id, credentialId);
    if (row === null) return notFound(CONSOLE_MESSAGE_KEYS.llm_key_row_not_found);

    // Already active: answer with the row rather than burning a version on a
    // write that changes nothing.
    if (row.status === "active") {
      return Response.json({ id: row.id }, { headers: NO_STORE });
    }
    const record = await client.enableProviderCredential(row.id, ifMatchFor(row));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}
