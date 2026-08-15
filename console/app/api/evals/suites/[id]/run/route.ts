// `POST /api/evals/suites/{id}/run` — trigger an eval run.
//
// ============================================================================
// THIS ENDPOINT DOES NOT EXIST IN MOIRA YET — DELIBERATELY WIRED ANYWAY
// ============================================================================
//
// `POST /api/v1/admin/eval-suites/{id}/run` is landing in a parallel backend
// PR and is not yet in the committed `docs/openapi.json` this branch was built
// against. `MoiraClient`'s operation registry is re-derived from that file on
// every test run (`tests/contract/openapi-contract.test.ts`), so registering an
// operation whose path is not in the spec fails the contract gate outright —
// there is no way to call it through the client honestly until the spec
// catches up.
//
// So the button on `/evals` is wired end-to-end — it POSTs here, this handler
// still runs behind `withConsoleSession` like every other mutation in this
// family — and the tolerance is built in from the start rather than discovered
// live: it answers a clean, keyed "not available yet" rather than either
// silently doing nothing or throwing a raw 404 from a client call that isn't
// registered. Once the parallel PR lands and `docs/openapi.json` gains the
// operation, the follow-up is: add `triggerEvalRun` to `MOIRA_OPERATIONS` and
// `MoiraClient`, and replace the body of this handler with the real call.

import { withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async () => {
    return Response.json(
      {
        error: {
          code: "eval_run_not_available",
          message_key: CONSOLE_MESSAGE_KEYS.evals_run_not_available,
        },
      },
      { status: 501, headers: { "cache-control": "no-store" } },
    );
  });
}
