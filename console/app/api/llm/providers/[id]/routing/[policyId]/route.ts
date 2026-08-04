// `DELETE /api/llm/providers/{id}/routing/{policyId}` — stop routing traffic to
// this provider through one policy.
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
// Re-checks the session itself — `app/api/**` is outside every route group.

import { notFound, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { LIST_PAGE_LIMIT } from "@/lib/llm-settings";
import { ifMatchFor } from "@/lib/moira-client";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function DELETE(
  request: Request,
  context: { params: Promise<{ id: string; policyId: string }> },
): Promise<Response> {
  const { id, policyId } = await context.params;
  return withConsoleSession(request, async ({ client }) => {
    const provider = await client.getProvider(id);
    const page = await client.listRoutingPolicies({ limit: LIST_PAGE_LIMIT });
    const policy = page.data.find(
      (candidate) => candidate.id === policyId && candidate.provider_id === provider.id,
    );
    if (policy === undefined) return notFound(CONSOLE_MESSAGE_KEYS.llm_policy_not_found);

    const record = await client.disableRoutingPolicy(policy.id, ifMatchFor(policy));
    return Response.json({ id: record.id }, { headers: { "cache-control": "no-store" } });
  });
}
