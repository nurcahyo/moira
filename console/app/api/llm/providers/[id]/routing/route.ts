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
// existing set is listed first and a match is returned as-is, which makes a
// double-click a no-op instead of a second eligible row.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import { badRequest, notFound, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { GENERAL_ROUTE_KEY } from "@/lib/llm-view";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";

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
    const model = models.data.find((row) => row.id === requestedModelId);
    if (model === undefined) return notFound(CONSOLE_MESSAGE_KEYS.llm_model_not_found);

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
      return Response.json({ id: already.id }, { headers: { "cache-control": "no-store" } });
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
