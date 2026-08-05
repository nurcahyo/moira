// `DELETE /api/llm/providers/{id}/routing/{policyId}` — stop routing traffic to
// this provider through one policy.
// `POST` on the same path — start again through the same policy.
//
// The undo for the step that actually moves live traffic, and therefore the one
// with the strictest ownership check: the policy must name the provider in the
// path, or the console answers its own 404 and writes nothing. Moira's disable
// is flat and would act on any policy id at all.
//
// Disable, not delete: `routing_policies` has no delete operation in the
// registry and a disabled policy is reversible, readable, and still explains
// what the deployment used to do.
//
// ============================================================================
// "REVERSIBLE" NEEDED SOMETHING TO REVERSE IT
// ============================================================================
//
// `runtime.rs` joins `routing_policies` on `rp.status = 'active'`, so a disabled
// policy is invisible to routing — and it is NOT invisible to this console's
// dedupe, which matches on the (route, provider, model) triple. Together those
// two facts made disable terminal: "Bind routing" kept answering 200 with the
// disabled policy's id and creating nothing, so an active policy for that triple
// could never be produced from this screen again. `enableRoutingPolicy` was on
// the client already, with no caller. This is its caller.
//
// Re-checks the session itself — `app/api/**` is outside every route group.

import {
  badRequest,
  lookupOnPage,
  notFound,
  withConsoleSession,
  type PageLookup,
} from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor, type MoiraClient } from "@/lib/moira-client";
import type { RoutingPolicyRecord } from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/**
 * The policy, only if it points at the provider named in the path.
 *
 * Issue #117: three-case, not `record | null`. `routing_policies` is listed
 * UNFILTERED — the operation takes no `provider_id` query parameter — so this is
 * the lookup on this surface most likely to outgrow one page, and the one where
 * "not on page one" reported as 404 would tell an operator their live routing
 * policy does not exist.
 */
async function ownedPolicy(
  client: MoiraClient,
  providerId: string,
  policyId: string,
): Promise<PageLookup<RoutingPolicyRecord>> {
  const provider = await client.getProvider(providerId);
  const page = await client.listRoutingPolicies({ limit: LIST_PAGE_LIMIT });
  return lookupOnPage(
    page,
    (candidate) => candidate.id === policyId && candidate.provider_id === provider.id,
  );
}

/** The refusal for a lookup that did not find its row. */
function refuse(lookup: PageLookup<unknown>): Response {
  return lookup.kind === "truncated"
    ? badRequest(CONSOLE_MESSAGE_KEYS.llm_list_truncated)
    : notFound(CONSOLE_MESSAGE_KEYS.llm_policy_not_found);
}

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; policyId: string }> },
): Promise<Response> {
  const { id, policyId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const lookup = await ownedPolicy(client, id, policyId);
    if (lookup.kind !== "found") return refuse(lookup);
    const policy = lookup.row;

    const record = await client.disableRoutingPolicy(policy.id, ifMatchFor(policy));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}

export async function POST(
  request: Request,
  context: { params: Promise<{ id: string; policyId: string }> },
): Promise<Response> {
  const { id, policyId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const lookup = await ownedPolicy(client, id, policyId);
    if (lookup.kind !== "found") return refuse(lookup);
    const policy = lookup.row;

    // Already active: answer with the row rather than burning a version on a
    // write that changes nothing.
    if (policy.status === "active") {
      return Response.json({ id: policy.id }, { headers: NO_STORE });
    }
    const record = await client.enableRoutingPolicy(policy.id, ifMatchFor(policy));
    return Response.json({ id: record.id }, { headers: NO_STORE });
  });
}
