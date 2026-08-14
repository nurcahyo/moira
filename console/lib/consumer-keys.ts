// @server-only
//
// `/settings/keys` — the credential an APPLICATION presents to Moira.
//
// ============================================================================
// WHAT THIS SCREEN IS FOR, AND WHY IT IS NOT OPTIONAL POLISH
// ============================================================================
//
// The setup wizard ends with an operator who can administer the deployment. It
// does not end with a deployment anything can CALL. `/settings/llm` gets as far
// as "Ready: a prompt can reach this provider" and the prompt still has to come
// from somewhere, holding a credential the console had no way to mint (issue
// #180). Until this screen, that credential came from `make seed` or a
// hand-written admin call — so the guided first run stopped one step short of a
// usable deployment, and the last step was the one that needed a terminal.
//
// ============================================================================
// WHY APPLICATIONS ARE HERE TOO
// ============================================================================
//
// Not scope creep: `ConsumerKeyCreateRequest.application_id` is REQUIRED. There
// is no such thing as a consumer key belonging to no application, so a console
// that can mint keys and not create applications can mint nothing on a fresh
// deployment. The screen therefore lists applications, creates them, and hangs
// keys off them.
//
// ============================================================================
// THE PROJECTION IS THE POINT (D4, as on `/setup`)
// ============================================================================
//
// `ApiKeyRecord` carries `fingerprint` — a stable hash of the live key — and
// `pepper_version`. Neither buys an operator anything on screen and the first is
// a cryptographic value about a live credential, so the browser never receives
// an `ApiKeyRecord`: it receives `ConsumerKeyView`, built here, field by field.
// That is also what keeps `lib/moira-api-key-types.ts` server-only in practice
// rather than only by declaration.
//
// `key_prefix` IS included and is the identifier a human uses. It is Moira's own
// safe projection — enough to tell two keys apart in a list, not enough to
// authenticate with.
//
// ============================================================================
// THE SCOPES THIS SCREEN OFFERS, AND THE ONES IT REFUSES TO
// ============================================================================
//
// Moira's `ADMIN_SCOPES` catalogue holds roughly seventy scopes and
// `normalize_scopes` accepts any of them; `can_grant` then lets an admin mint a
// key carrying any scope the admin holds — which, for `moira:admin`, is all of
// them. So "offer the enum" would put `moira:system-keys:write` one checkbox
// away from an operator minting an application credential, with no ceremony
// anywhere on the path.
//
// `OFFERED_CONSUMER_SCOPES` is therefore a CURATED list of the scopes an
// application plausibly needs — the execution surface and the data surfaces it
// reaches — and the administrative families are absent by construction rather
// than filtered at the edge. An operator who genuinely needs an admin-scoped
// key still has the admin API; what they do not have is a two-click path to one.
//
// REVERSAL CONDITION: if a real application needs a scope that is not here, add
// it to this list with the reason. Do not replace the list with a filter over
// Moira's catalogue — the whole property is that the console cannot be talked
// into offering something nobody chose.

import "server-only";

// The view shapes live in `lib/keys-view.ts` — client-safe, because the
// organisms that render them may not import this module. Built here.
import type { ApplicationView, ConsumerKeyView, ConsumerKeysView } from "./keys-view";
import type { MoiraClient } from "./moira-client";
import type { KeyStatus } from "./types";

/**
 * The scope every key gets unless the operator says otherwise: without it a key
 * authenticates and can do nothing, which reads as a broken key rather than as
 * an empty one.
 */
export const DEFAULT_CONSUMER_SCOPE = "moira:responses:create";

/**
 * What the mint form may offer. Curated — see the header.
 *
 * Grouped by the question an operator is actually answering: "may this
 * application send prompts / keep conversations / keep memories / read what it
 * used". Each entry's copy lives in the catalog, keyed by the scope itself, so a
 * scope added here without copy fails the i18n gate rather than rendering a raw
 * `moira:` string at an operator.
 */
export const OFFERED_CONSUMER_SCOPES: readonly string[] = [
  "moira:responses:create",
  "moira:responses:stream",
  "moira:responses:read",
  "moira:conversations:create",
  "moira:conversations:read",
  "moira:conversations:write",
  "moira:memories:create",
  "moira:memories:read",
  "moira:rag-collections:read",
  "moira:rag-documents:read",
  "moira:usage:read",
];

/**
 * One page each. Deliberately not a cursor walk: an operator with more than a
 * hundred applications is past what a flat list can serve, and the honest answer
 * is `truncated` plus a note, not a screen that silently shows a prefix.
 */
const PAGE_LIMIT = 100;

function keyView(record: {
  readonly id: string;
  readonly display_name: string;
  readonly key_prefix: string;
  readonly scopes: readonly string[];
  readonly status: KeyStatus;
  readonly created_at: string;
  readonly last_used_at?: string | null;
  readonly expires_at?: string | null;
}): ConsumerKeyView {
  return {
    id: record.id,
    display_name: record.display_name,
    key_prefix: record.key_prefix,
    scopes: [...record.scopes],
    status: record.status,
    created_at: record.created_at,
    last_used_at: record.last_used_at ?? null,
    expires_at: record.expires_at ?? null,
  };
}

/**
 * Everything `/settings/keys` renders, in two calls.
 *
 * The keys list is fetched WHOLE and grouped here rather than fetched per
 * application: one request per application would multiply an admin round trip by
 * the application count on every render, and the grouping is a `Map` build.
 */
export async function loadConsumerKeys(client: MoiraClient): Promise<ConsumerKeysView> {
  const [applications, keys] = await Promise.all([
    client.listApplications({ limit: PAGE_LIMIT }),
    client.listConsumerKeys({ limit: PAGE_LIMIT }),
  ]);

  const byApplication = new Map<string, ConsumerKeyView[]>();
  const unattached: ConsumerKeyView[] = [];
  for (const record of keys.data) {
    const view = keyView(record);
    const applicationId = record.application_id ?? null;
    if (applicationId === null) {
      unattached.push(view);
      continue;
    }
    const bucket = byApplication.get(applicationId);
    if (bucket === undefined) byApplication.set(applicationId, [view]);
    else bucket.push(view);
  }

  const applicationViews: ApplicationView[] = applications.data.map((record) => ({
    id: record.id,
    display_name: record.display_name,
    application_slug: record.application_slug ?? null,
    status: record.status,
    keys: byApplication.get(record.id) ?? [],
  }));

  // Anything left in the map belongs to an application this page did not list.
  const listed = new Set(applications.data.map((record) => record.id));
  for (const [applicationId, bucket] of byApplication) {
    if (!listed.has(applicationId)) unattached.push(...bucket);
  }

  return {
    applications: applicationViews,
    unattachedKeys: unattached,
    truncated: applications.pagination.has_more || keys.pagination.has_more,
    offeredScopes: OFFERED_CONSUMER_SCOPES,
    defaultScope: DEFAULT_CONSUMER_SCOPE,
  };
}

/**
 * Narrow an operator's scope selection to what this screen offers.
 *
 * Applied SERVER-SIDE, on the request, not merely when rendering the form: the
 * form is a client component and its checkboxes are a suggestion, so a curated
 * list enforced only in the UI is not enforced. An empty result falls back to
 * the default scope rather than minting a key with no scopes — Moira accepts
 * `scopes: []` and the result is a credential that authenticates and can do
 * nothing, which is indistinguishable from a broken deployment at the caller.
 */
export function narrowScopes(requested: unknown): string[] {
  const offered = new Set(OFFERED_CONSUMER_SCOPES);
  const selected = Array.isArray(requested)
    ? [...new Set(requested.filter((scope): scope is string => typeof scope === "string"))].filter(
        (scope) => offered.has(scope),
      )
    : [];
  return selected.length === 0 ? [DEFAULT_CONSUMER_SCOPE] : selected.sort();
}

/**
 * A slug Moira will accept, or `null` to let it assign nothing.
 *
 * Moira's own constraint, mirrored rather than guessed at: lower-case letters,
 * digits and hyphens. A blank field means "no slug", which is a legal
 * application — the id is the identity, the slug is a convenience.
 */
export function normalizeApplicationSlug(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "") return null;
  return /^[a-z0-9-]+$/.test(trimmed) ? trimmed : null;
}
