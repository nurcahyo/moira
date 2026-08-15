// `POST /api/playground/run` — the playground's non-streaming fallback.
//
// Proxies `POST /api/v1/responses` after resolving the console session
// server-side, exactly like every other mutation under `app/api/**`. See
// `app/api/playground/stream/route.ts`'s header for the streaming twin, and
// why the two are separate route handlers rather than one that branches on a
// query flag: `MoiraClient#request<T>` (which this route uses, through
// `createResponse`) always parses the body as JSON, which would consume an
// SSE stream before a single frame reached the browser.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { buildPublicResponseRequest, readPlaygroundRunBody } from "@/lib/playground-request";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_request_body_invalid);

    const input = readPlaygroundRunBody(body);
    if (input === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_prompt_required);

    const record = await client.createResponse(buildPublicResponseRequest(input));
    return Response.json(record, { headers: NO_STORE });
  });
}
