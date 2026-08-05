// `POST /api/llm/providers/{id}/routing` — point default routing at one of this
// provider's models.
//
// ============================================================================
// THE ROUTE IS RESOLVED SERVER-SIDE AND IS NEVER IN THE BODY
// ============================================================================
//
// `RoutingPolicyCreateRequest` has three foreign keys and all three decide which
// provider live traffic reaches. Two of them come from the URL path and are
// verified against Moira before the write. The third — `route_id` — is never
// supplied by the browser at all: it is looked up by `route_key` from the row
// migration `0005` seeded.
//
// That is the difference between "the operator chose a model" and "the request
// chose where traffic goes". A body that could name a route could bind a policy
// to a route the operator was never shown, on a screen that only ever offers one.
//
// The route is LOOKED UP, NOT CREATED. `POST /api/v1/admin/routes` documents no
// 409 for a duplicate `route_key`, so a console that created one could land a
// second `general` and leave routing with two candidate definitions and no
// documented rule for choosing between them. Its absence is a configuration
// error with its own keyed message.
//
// ============================================================================
// THERE IS NO 409 TO CATCH, SO THE DEDUPE IS A READ
// ============================================================================
//
// Two identical policies on one route are both stored and both eligible. The
// existing set is listed first and a match is returned, which makes a
// double-click a no-op instead of a second eligible row.
//
// A MATCH IS RETURNED **ACTIVE**, NOT AS FOUND. A disabled policy still blocks
// the create — it is the same (route, provider, model) triple, and this handler
// would keep answering 200 with its id forever while `runtime.rs`, which joins
// `routing_policies` on `rp.status = 'active'`, kept refusing to select it. So
// an active policy for that triple could never be produced from this screen
// again, and the only undo the screen offers would be a one-way door. A disabled
// match is enabled instead.
//
// The dedupe also REFUSES A TRUNCATED PAGE rather than reading "not on page one"
// as "does not exist" — the same rule `findOnPage` applies inside the chain, for
// the same reason: guessing wrong creates the duplicate the dedupe exists to
// prevent. Issue #117 extended that rule to the MODEL lookup above it, which had
// been reading a truncated page as "this provider does not own that model".
//
// ============================================================================
// THE MODEL'S STATUS IS AN INVARIANT OF THIS ENDPOINT, NOT OF THE SELECT
// ============================================================================
//
// Issue #117: this handler accepted any `provider_model_id` the provider owned,
// whatever its status. The UI filters the dropdown, so only a direct caller
// could reach it — and Moira stores such a policy happily while `runtime.rs`,
// which joins `provider_models` on `pm.status = 'active'`, never selects it. The
// check lives here now, so the invariant does not depend on the client.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import {
  badRequest,
  lookupOnPage,
  notFound,
  readJsonBody,
  withConsoleSession,
} from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { GENERAL_ROUTE_KEY } from "@/lib/llm-view";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string }> },
): Promise<Response> {
  const { id } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_request_body_invalid);

    const requestedModelId =
      typeof body["provider_model_id"] === "string" ? body["provider_model_id"] : "";
    if (requestedModelId === "") return badRequest(CONSOLE_MESSAGE_KEYS.llm_model_key_required);

    const provider = await client.getProvider(id);
    const models = await client.listProviderModels(provider.id, { limit: LIST_PAGE_LIMIT });
    const found = lookupOnPage(models, (row) => row.id === requestedModelId);
    // The same rule the dedupe below already applied to the policy list, now
    // applied to the model list too (issue #117): a truncated page is refused,
    // not reported as a model this provider does not own.
    if (found.kind === "truncated") return badRequest(CONSOLE_MESSAGE_KEYS.llm_list_truncated);
    if (found.kind === "absent") return notFound(CONSOLE_MESSAGE_KEYS.llm_model_not_found);
    const model = found.row;

    // THE MODEL'S STATUS IS ENFORCED HERE, NOT ONLY IN THE SELECT (issue #117).
    //
    // The screen filters the dropdown, so this only fires for a caller posting
    // directly — which is exactly the caller a server-side invariant is for. A
    // policy bound to a `disabled` or `deprecated` model is STORED by Moira and
    // then never selected: `runtime.rs` joins `provider_models` on
    // `pm.status = 'active'`. The operator gets a routing row that reads as
    // configured, a chain report that says routing exists, and completions that
    // fail `no_eligible_model` with nothing on this screen contradicting them.
    //
    // Refused rather than repaired. Enabling the model as a side effect of
    // binding routing would undo a disable the operator performed deliberately,
    // from a control that never mentioned it; `POST .../models/{modelId}` is the
    // door back, and it is one click away on the same screen.
    if (model.status !== "active") {
      return badRequest(CONSOLE_MESSAGE_KEYS.llm_model_not_selectable);
    }

    const route = await client.findRouteByKey(GENERAL_ROUTE_KEY);
    if (route === null) return badRequest(CONSOLE_MESSAGE_KEYS.llm_general_route_missing);

    const existing = await client.listRoutingPolicies({ limit: LIST_PAGE_LIMIT });
    const already = existing.data.find(
      (policy) =>
        policy.route_id === route.id &&
        policy.provider_id === provider.id &&
        policy.provider_model_id === model.id &&
        policy.status !== "deleted",
    );
    if (already !== undefined) {
      if (already.status === "active") {
        return Response.json({ id: already.id }, { headers: { "cache-control": "no-store" } });
      }
      const revived = await client.enableRoutingPolicy(already.id, ifMatchFor(already));
      return Response.json({ id: revived.id }, { headers: { "cache-control": "no-store" } });
    }
    if (existing.pagination.has_more) {
      // "Not on this page" is not "does not exist". Creating anyway is how a
      // second eligible policy for one triple gets landed.
      return badRequest(CONSOLE_MESSAGE_KEYS.llm_list_truncated);
    }

    const record = await client.createRoutingPolicy(
      {
        route_id: route.id,
        provider_id: provider.id,
        provider_model_id: model.id,
        priority: 100,
        weight: 1,
      },
      { idempotencyKey: `llm-policy:${route.id}:${provider.id}:${model.id}` },
    );
    return Response.json(
      { id: record.id },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  });
}
