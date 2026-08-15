// `POST /api/runners/[id]/finalize` — store a runner's minted token as a
// provider credential. Issue #275/#272 workstream R3.
//
// ============================================================================
// `409 runner_token_unavailable` MEANS THIS RUNNER IS A WRITE-OFF
// ============================================================================
//
// The token read on Moira's side is one-shot: if the runner service already
// gave it up and Moira failed to persist it, no retry of this endpoint can
// ever succeed again. This handler does not special-case that response — it
// forwards Moira's `MoiraRequestError` exactly like every other refusal — and
// the special-casing lives entirely in `modules/runners/RunnerDetail.tsx`,
// which is what decides "say so plainly, offer delete-and-reprovision, never a
// retry button". Duplicating that judgment here would be a second place for
// the two to disagree.
//
// ============================================================================
// NO `scope` FIELD, EVER
// ============================================================================
//
// The scope is fixed at provisioning and sealed into the credential's AAD.
// This handler refuses a body carrying one with the console's own keyed 400,
// rather than letting Moira's `deny_unknown_fields` answer with a generic
// validation failure that names no field.
//
// Re-checks the session itself — see `app/api/llm/connect-vllm/route.ts` for
// the rationale.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.runners_request_body_invalid);

    if ("scope" in body) {
      return badRequest(CONSOLE_MESSAGE_KEYS.runners_finalize_scope_forbidden);
    }

    const providerId = body["provider_id"];
    if (typeof providerId !== "string" || providerId.trim() === "") {
      return badRequest(CONSOLE_MESSAGE_KEYS.runners_finalize_provider_id_required);
    }

    const displayName = body["display_name"];
    const record = await client.finalizeRunner(id, {
      provider_id: providerId,
      ...(typeof displayName === "string" && displayName.trim() !== ""
        ? { display_name: displayName }
        : {}),
    });
    return Response.json(record, { headers: NO_STORE });
  });
}
