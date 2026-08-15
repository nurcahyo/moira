// `POST /api/playground/diagnose` — full routing transparency and tool-call
// visibility, when this deployment has the endpoint enabled and the signed-in
// operator holds `moira:runtime:diagnose`.
//
// Proxies `POST /api/v1/admin/runtime/diagnose` verbatim. This is the ONLY
// committed HTTP surface that returns per-candidate rank/score/selection-
// reason (`candidate_ranked`) or any `tool_call_*`/`tool_result` event —
// `map_runtime_event` (`src/application/public.rs`) deliberately drops all
// five from the public SSE stream `.../stream/route.ts` proxies. See
// `lib/moira-client.ts`'s `diagnoseRuntime` doc comment and the PR description
// for issue #261 for the full accounting of what this console can and cannot
// show on each path.
//
// A caller lacking the scope, or a deployment with
// `runtime.diagnostic_endpoint_enabled = false`, gets Moira's own keyed
// refusal (403 / 404 respectively) through the normal `MoiraRequestError`
// path — there is no special-casing here, by design: `PlaygroundDiagnostics`
// renders whatever `message_key` arrives, same as any other Moira error.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { buildDiagnosticExecutionRequest, readPlaygroundDiagnoseBody } from "@/lib/playground-request";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_request_body_invalid);

    const input = readPlaygroundDiagnoseBody(body);
    if (input === null) return badRequest(CONSOLE_MESSAGE_KEYS.playground_prompt_required);

    const result = await client.diagnoseRuntime(buildDiagnosticExecutionRequest(input));
    return Response.json(result, { headers: NO_STORE });
  });
}
