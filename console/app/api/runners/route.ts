// `GET /api/runners` (list) and `POST /api/runners` (provision) — containerised
// Claude runners, issue #275/#272 workstream R3.
//
// ============================================================================
// THE TOKEN NEVER APPEARS HERE, AND NEVER WILL
// ============================================================================
//
// Neither this handler nor `lib/runners.ts`/`lib/moira-client.ts` beneath it
// ever reads a token. `ClaudeRunnerRecord` (`lib/types.ts`) has no token-shaped
// field — Moira's own `src/domain/runners.rs` pins the same invariant against
// its generated schema. The console orchestrates provisioning; Moira is the
// only process that ever calls the runner service's one-shot
// `GET /v1/runners/{id}/token`, and it moves the result straight into the
// existing credential-create chain without this console being involved at all.
//
// ============================================================================
// SCOPE IS COLLECTED HERE, AND NOWHERE ELSE
// ============================================================================
//
// `scope` is settable only at provision: it is sealed into the resulting
// credential's AAD, so `POST .../finalize` rejects one outright. This handler
// is therefore the console's only chance to ask "whose account is this for" —
// absent means the platform-wide account, `{"type":"tenant", ...}` overrides it
// for one tenant. See `lib/types.ts`'s `ClaudeRunnerScope` header for why that
// type is a structural mirror of `CredentialScope` rather than an import of it.
//
// Re-checks the session itself — `app/api/**` is outside every route group; see
// `app/api/llm/connect-vllm/route.ts` for the fuller rationale.

import { badRequest, readJsonBody, withConsoleSession } from "@/lib/console-api";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { provisionRunner } from "@/lib/runners";
import {
  CLAUDE_RUNNER_LABEL_PATTERN,
  CLAUDE_RUNNER_TTL_SECONDS_MAX,
  CLAUDE_RUNNER_TTL_SECONDS_MIN,
  type ClaudeRunnerScope,
} from "@/lib/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const NO_STORE = { "cache-control": "no-store" } as const;

/** `undefined`, `null`, `{type:"global"}`, or `{type:"tenant", external_tenant_id}` — the only shapes this console's provisioning form can produce. */
function narrowScope(value: unknown): { readonly ok: true; readonly scope: ClaudeRunnerScope | null } | { readonly ok: false } {
  if (value === undefined || value === null) return { ok: true, scope: null };
  if (typeof value !== "object" || Array.isArray(value)) return { ok: false };
  const record = value as Record<string, unknown>;
  if (record["type"] === "global") return { ok: true, scope: { type: "global" } };
  if (
    record["type"] === "tenant" &&
    typeof record["external_tenant_id"] === "string" &&
    record["external_tenant_id"].trim() !== ""
  ) {
    return { ok: true, scope: { type: "tenant", external_tenant_id: record["external_tenant_id"] } };
  }
  return { ok: false };
}

export async function GET(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const url = new URL(request.url);
    const limitParam = url.searchParams.get("limit");
    const cursorParam = url.searchParams.get("cursor");
    const limit = limitParam === null ? undefined : Number(limitParam);
    const page = await client.listRunners({
      ...(limit === undefined ? {} : { limit }),
      ...(cursorParam === null ? {} : { cursor: cursorParam }),
    });
    return Response.json(page, { headers: NO_STORE });
  });
}

export async function POST(request: Request): Promise<Response> {
  return withConsoleSession(request, async ({ client }) => {
    const body = await readJsonBody(request);
    if (body === null) return badRequest(CONSOLE_MESSAGE_KEYS.runners_request_body_invalid);

    const label = body["label"];
    if (typeof label !== "string" || !CLAUDE_RUNNER_LABEL_PATTERN.test(label)) {
      return badRequest(CONSOLE_MESSAGE_KEYS.runners_provision_label_required);
    }

    let ttlSeconds: number | undefined;
    if ("ttl_seconds" in body && body["ttl_seconds"] !== undefined) {
      const raw = body["ttl_seconds"];
      if (
        typeof raw !== "number" ||
        !Number.isInteger(raw) ||
        raw < CLAUDE_RUNNER_TTL_SECONDS_MIN ||
        raw > CLAUDE_RUNNER_TTL_SECONDS_MAX
      ) {
        return badRequest(CONSOLE_MESSAGE_KEYS.runners_provision_ttl_invalid);
      }
      ttlSeconds = raw;
    }

    const scope = narrowScope(body["scope"]);
    if (!scope.ok) return badRequest(CONSOLE_MESSAGE_KEYS.runners_provision_scope_invalid);

    const record = await provisionRunner(client, {
      label,
      ...(ttlSeconds === undefined ? {} : { ttlSeconds }),
      ...(scope.scope === null ? {} : { scope: scope.scope }),
    });
    return Response.json(record, { status: 201, headers: NO_STORE });
  });
}
