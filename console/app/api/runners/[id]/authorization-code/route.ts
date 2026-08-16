// `POST /api/runners/[id]/authorization-code` — forward the operator's pasted
// OAuth authorization code to the runner service. Issue #275/#272 workstream
// R3.
//
// The code is single-use and is never stored, logged, or echoed back by
// anything in this console — see `assertRunnerFinalizeRequestIsSafe`'s sibling
// comment on `submitRunnerAuthorizationCode` in `lib/moira-client.ts` for the
// same rule applied to the token itself.
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

    const code = body["code"];
    if (typeof code !== "string" || code.trim() === "") {
      return badRequest(CONSOLE_MESSAGE_KEYS.runners_authorization_code_required);
    }

    const record = await client.submitRunnerAuthorizationCode(id, { code });
    return Response.json(record, { headers: NO_STORE });
  });
}
