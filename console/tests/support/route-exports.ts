// Which HTTP handlers a `route.ts` actually exports, read from the source.
//
// ============================================================================
// WHY A SHARED MODULE AND NOT A COPY IN EACH SUITE
// ============================================================================
//
// Two suites need this and they need it for opposite halves of one rule:
//
//   `tests/unit/architecture/route-handler-session.test.ts` asks whether EVERY
//   exported handler calls `withConsoleSession`. Its header records the mutation
//   that forced the granularity — a file-level scan stayed green while `DELETE`
//   was unguarded, because the `PATCH` beside it was not.
//
//   `tests/unit/api/llm-routes.test.ts` asks whether its own list of handlers is
//   the WHOLE surface. That list was a hand-maintained literal with
//   `expect(EVERY_HANDLER.length).toBe(11)` beside it — a floor that can only
//   ever detect the eleven handlers somebody already remembered, and which the
//   twelfth passes straight through.
//
// A second transcription of the extractor is how those two answers drift apart,
// so there is one.
//
// ============================================================================
// THE EXTRACTOR IS DELIBERATELY PESSIMISTIC
// ============================================================================
//
// Comments are stripped first, so a mention of a guard in prose is not a call.
// An alias export (`export const GET = handle;`) is recorded with an EMPTY body
// rather than skipped: a handler whose implementation this cannot see is one it
// cannot vouch for, and skipping it would make the alias form a way through
// every rule built on top.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { extname, join, relative } from "node:path";

/** The HTTP verbs Next.js routes to an exported function of the same name. */
export const HTTP_EXPORTS = [
  "GET",
  "POST",
  "PUT",
  "PATCH",
  "DELETE",
  "HEAD",
  "OPTIONS",
] as const;

export interface ExportedHandler {
  readonly file: string;
  readonly method: string;
  readonly body: string;
}

export interface RouteModule {
  readonly path: string;
  readonly source: string;
}

export function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

/** Every `route.ts` under `root`, with its path relative to `base`. */
export function routeModules(root: string, base: string): RouteModule[] {
  const found: RouteModule[] = [];
  const walk = (absolute: string): void => {
    let entries: string[];
    try {
      entries = readdirSync(absolute);
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = join(absolute, entry);
      if (statSync(full).isDirectory()) {
        walk(full);
        continue;
      }
      if (extname(entry) !== ".ts" && extname(entry) !== ".tsx") continue;
      if (!/^route\.tsx?$/.test(entry)) continue;
      found.push({
        path: relative(base, full).split("/").join("/"),
        source: readFileSync(full, "utf8"),
      });
    }
  };
  walk(root);
  return found.sort((a, b) => a.path.localeCompare(b.path));
}

/**
 * Index of the `{` that opens a handler's BODY, skipping its parameter list.
 *
 * Returns -1 when the declaration is an alias (`= handle;`) with no body.
 *
 * THE PARAMETER LIST HAS TO BE SKIPPED FIRST, and getting this wrong is how the
 * extractor silently reported a guarded handler as unguarded: Next's own
 * signature is
 *   export async function POST(request: Request, context: { params: … })
 * so the first `{` after the function name is inside the SECOND PARAMETER's type
 * annotation, and brace-matching from there ends the "body" before the real one
 * begins. Caught by re-running the mutation, not by reading the regex.
 */
function openingBraceOfBody(source: string, from: number): number {
  const paren = source.indexOf("(", from);
  const semicolon = source.indexOf(";", from);
  if (paren === -1 || (semicolon !== -1 && semicolon < paren)) return -1;
  let depth = 0;
  let index = paren;
  for (; index < source.length; index += 1) {
    const character = source[index];
    if (character === "(") depth += 1;
    else if (character === ")") {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  const brace = source.indexOf("{", index);
  const end = source.indexOf(";", index);
  if (brace === -1 || (end !== -1 && end < brace)) return -1;
  return brace;
}

/**
 * Every exported HTTP handler in a route module, with its own body.
 *
 * Brace-matched rather than split on the next `export`, so a nested function or
 * an object literal inside the handler does not truncate it. Both the
 * `export async function POST(...)` form and
 * `export const POST = async (...) => {...}` are matched, because Next accepts
 * either and a guard that only saw one would be silent about the other.
 */
export function exportedHandlers(file: string, rawSource: string): ExportedHandler[] {
  const source = stripComments(rawSource);
  const found: ExportedHandler[] = [];
  const declaration = new RegExp(
    `export\\s+(?:async\\s+)?(?:function\\s+(${HTTP_EXPORTS.join("|")})\\b|const\\s+(${HTTP_EXPORTS.join("|")})\\s*=)`,
    "g",
  );
  for (const match of source.matchAll(declaration)) {
    const method = match[1] ?? match[2] ?? "";
    const after = (match.index ?? 0) + match[0].length;
    const bodyStart = openingBraceOfBody(source, after);
    // `export const GET = handle;` — an ALIAS to a function defined elsewhere,
    // with no body of its own. Recorded with an EMPTY body so it reads as
    // unguarded, which is the safe direction.
    if (bodyStart === -1) {
      found.push({ file, method, body: "" });
      continue;
    }
    const open = bodyStart;
    let depth = 0;
    let index = open;
    for (; index < source.length; index += 1) {
      const character = source[index];
      if (character === "{") depth += 1;
      else if (character === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    found.push({ file, method, body: source.slice(open, index + 1) });
  }
  return found;
}

/** `"METHOD path"` for every exported handler under `root`, sorted. */
export function exportedHandlersUnder(root: string, base: string): string[] {
  return routeModules(root, base)
    .flatMap((module) => exportedHandlers(module.path, module.source))
    .map((handler) => `${handler.method} ${handler.file}`)
    .sort();
}
