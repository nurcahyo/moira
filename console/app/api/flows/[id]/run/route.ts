// `POST /api/flows/{id}/run` — trigger a flow run.
//
// Same situation as `app/api/evals/suites/[id]/run/route.ts` — see that file's
// header for the full reasoning. `POST /api/v1/admin/flows/{id}/run` is landing
// in a parallel backend PR and is not yet in the committed `docs/openapi.json`,
// so it cannot be registered in `MoiraClient` without failing the contract
// test. The button is wired end-to-end; this handler answers a clean, keyed
// "not available yet" behind the same session gate every other mutation in
// this family uses. Follow-up once the spec catches up: add `triggerFlowRun` to
// `MOIRA_OPERATIONS` and `MoiraClient`, and replace this handler's body with
// the real call.

import { withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async () => {
    return Response.json(
      {
        error: {
          code: "flow_run_not_available",
          message_key: CONSOLE_MESSAGE_KEYS.flows_run_not_available,
        },
      },
      { status: 501, headers: { "cache-control": "no-store" } },
    );
  });
}
