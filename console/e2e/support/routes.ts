import fs from "node:fs";
import path from "node:path";

/**
 * App Router page-level route discovery.
 *
 * CONVENTIONS.md §3 requires automated a11y assertions on *every* page-level
 * route, and §8 requires the secret-leak gate to hold across the whole
 * browser-visible surface. Hard-coding a list of paths would silently stop
 * covering new routes the moment someone adds one — the classic "test that
 * passes whether or not the feature works".
 *
 * So instead we derive the route table from the filesystem, exactly the way
 * Next.js does: every `page.tsx` under `app/` is a page-level route. A new
 * route added by plan 08 is therefore covered by both gates with no edit to
 * this file — unless it is *dynamic*, in which case it is reported as
 * uncovered until a fixture path is registered below (see
 * `DYNAMIC_ROUTE_FIXTURES`), which fails the coverage guard test rather than
 * quietly shrinking the suite.
 */

export interface DiscoveredRoute {
  /** URL path, e.g. `/` or `/providers/[id]`. */
  readonly pattern: string;
  /** Concrete URL to visit, or `null` when a dynamic route has no fixture. */
  readonly url: string | null;
  /** Repo-relative source file that defines the route. */
  readonly source: string;
  readonly dynamic: boolean;
}

/**
 * Concrete URLs to substitute for dynamic route patterns.
 *
 * Plan 08 owns the real entries (e.g. `"/providers/[id]": "/providers/e2e-fixture"`).
 * Leaving a dynamic route out of this map is a *failing* condition, not an
 * implicit skip — see `test('every page-level route is covered', ...)`.
 */
export const DYNAMIC_ROUTE_FIXTURES: Readonly<Record<string, string>> = {
  // "/providers/[id]": "/providers/e2e-fixture-provider",
};

const PAGE_FILES = new Set(["page.tsx", "page.ts", "page.jsx", "page.js", "page.mjs"]);

/** Route groups: `(marketing)` contributes no URL segment. */
function isRouteGroup(segment: string): boolean {
  return segment.startsWith("(") && segment.endsWith(")");
}

/** Interception markers: `(.)photo`, `(..)photo`, `(..)(..)photo`, `(...)photo`. */
function isInterceptingSegment(segment: string): boolean {
  return /^\(\.{1,3}\)/.test(segment);
}

/** Parallel-route slots (`@modal`) are rendered *inside* another route. */
function isParallelSlot(segment: string): boolean {
  return segment.startsWith("@");
}

/** Private folders (`_components`) are never routable. */
function isPrivateFolder(segment: string): boolean {
  return segment.startsWith("_");
}

function isDynamicSegment(segment: string): boolean {
  return segment.startsWith("[") && segment.endsWith("]");
}

function walk(dir: string, segments: string[], out: DiscoveredRoute[]): void {
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }

  for (const entry of entries) {
    if (entry.isFile() && PAGE_FILES.has(entry.name)) {
      const pattern = "/" + segments.filter((s) => s.length > 0).join("/");
      const normalized = pattern === "/" ? "/" : pattern.replace(/\/+$/, "");
      const dynamic = segments.some(isDynamicSegment);
      const fixture = DYNAMIC_ROUTE_FIXTURES[normalized];
      out.push({
        pattern: normalized,
        url: dynamic ? (fixture ?? null) : normalized,
        source: path.join(dir, entry.name),
        dynamic,
      });
      continue;
    }

    if (!entry.isDirectory()) continue;

    const name = entry.name;
    if (
      isPrivateFolder(name) ||
      isParallelSlot(name) ||
      isInterceptingSegment(name) ||
      name === "node_modules"
    ) {
      continue;
    }

    const nextSegments = isRouteGroup(name) ? segments : [...segments, name];
    walk(path.join(dir, name), nextSegments, out);
  }
}

export function discoverPageRoutes(appDirectory: string): DiscoveredRoute[] {
  const found: DiscoveredRoute[] = [];
  walk(appDirectory, [], found);
  found.sort((a, b) => a.pattern.localeCompare(b.pattern));
  return found;
}

export type VisitableRoute = DiscoveredRoute & { readonly url: string };

/** Routes that can actually be navigated to right now. */
export function visitableRoutes(routes: DiscoveredRoute[]): VisitableRoute[] {
  return routes.filter((r): r is VisitableRoute => typeof r.url === "string");
}

/** Dynamic routes with no fixture URL — these fail the coverage guard. */
export function uncoveredRoutes(routes: DiscoveredRoute[]): DiscoveredRoute[] {
  return routes.filter((r) => r.url === null);
}
