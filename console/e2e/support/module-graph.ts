// A transitive import walk over the console's own source, for the e2e suite.
//
// ============================================================================
// WHY IT EXISTS: A TRIPWIRE THAT WOULD HAVE MISSED ITS OWN TRIGGER
// ============================================================================
//
// `secret-leak.e2e.ts` carried an assertion armed for plan 09 wave 5: *"no
// shipped route mounts OnceOnlySecretModal yet — and this fails when one does"*.
// It implemented that by grepping `app/**` for the literal string
// `modules/secrets/OnceOnlySecretModal`.
//
// The mount that arrived is `app/(console)/admins/page.tsx` importing
// `modules/admins/InviteAdminForm.tsx`, which imports the modal. The page's own
// source contains no such string, so the grep would have found nothing and the
// tripwire would have stayed GREEN through exactly the change it was written to
// catch — an accidental evasion, in a guard whose entire purpose is to notice.
//
// So reachability is computed over the import GRAPH. The resolver handles the
// two specifier forms this codebase uses (`@/`-aliased and relative), which is
// the same pair `tests/support/server-only-derivation.ts` resolves — that module
// cannot be reused here because it is written for Bun (`import.meta.dir`) and
// this suite runs under Node.
//
// TYPE-ONLY IMPORTS COUNT. This walk is about "can a render reach this
// component", and unlike the credential derivation there is no argument for
// excluding an edge: a component is not imported for its type.

import fs from "node:fs";
import path from "node:path";

import { CONSOLE_ROOT } from "./paths";

const SOURCE_EXTENSIONS = [".ts", ".tsx"];

/** Every shipped source file under `roots`, console-root-relative. */
export function sourceFiles(roots: readonly string[]): string[] {
  const found: string[] = [];
  const walk = (absolute: string): void => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(absolute, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (entry.name === "node_modules" || entry.name === ".next") continue;
      const full = path.join(absolute, entry.name);
      if (entry.isDirectory()) {
        walk(full);
        continue;
      }
      if (!SOURCE_EXTENSIONS.includes(path.extname(entry.name))) continue;
      if (/\.(test|spec|stories|e2e)\.[tj]sx?$/.test(entry.name)) continue;
      found.push(path.relative(CONSOLE_ROOT, full).split(path.sep).join("/"));
    }
  };
  for (const root of roots) walk(path.join(CONSOLE_ROOT, root));
  return found.sort();
}

const IMPORT_PATTERNS: readonly RegExp[] = [
  /import\s+(?:[^'";]*?\s+from\s+)?['"]([^'"]+)['"]/g,
  /import\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
  /export\s+(?:\*|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]/g,
];

function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

/** Resolve a specifier to a console-relative module path, or null. */
function resolveSpecifier(fromPath: string, specifier: string): string | null {
  if (specifier.startsWith(".")) {
    const absolute = path.resolve(path.dirname(path.join(CONSOLE_ROOT, fromPath)), specifier);
    return path.relative(CONSOLE_ROOT, absolute).split(path.sep).join("/");
  }
  if (specifier.startsWith("@/")) return specifier.slice(2);
  return null;
}

/** Map a resolved specifier onto a real file in `known`. */
function matchModule(known: ReadonlySet<string>, resolved: string): string | null {
  for (const candidate of [resolved, `${resolved}.ts`, `${resolved}.tsx`, `${resolved}/index.ts`]) {
    if (known.has(candidate)) return candidate;
  }
  return null;
}

/** Every console-relative module `file` imports directly. */
export function directImports(known: ReadonlySet<string>, file: string): string[] {
  let source: string;
  try {
    source = stripComments(fs.readFileSync(path.join(CONSOLE_ROOT, file), "utf8"));
  } catch {
    return [];
  }
  const targets: string[] = [];
  for (const pattern of IMPORT_PATTERNS) {
    for (const match of source.matchAll(pattern)) {
      const specifier = match[1];
      if (specifier === undefined) continue;
      const resolved = resolveSpecifier(file, specifier);
      if (resolved === null) continue;
      const matched = matchModule(known, resolved);
      if (matched !== null) targets.push(matched);
    }
  }
  return targets;
}

/** Does `entry` reach `target` through any chain of imports? */
export function reaches(known: ReadonlySet<string>, entry: string, target: string): boolean {
  const seen = new Set<string>([entry]);
  const queue = [entry];
  // Bounded by the file count rather than `while (queue.length)` alone: a cycle
  // plus a resolver bug should not spin forever inside a test.
  for (let visited = 0; queue.length > 0 && visited <= known.size + 1; visited += 1) {
    const current = queue.shift()!;
    if (current === target) return true;
    for (const next of directImports(known, current)) {
      if (seen.has(next)) continue;
      seen.add(next);
      queue.push(next);
    }
  }
  return false;
}

/** The module every once-only render must go through. */
export const ONCE_ONLY_MODAL_MODULE = "modules/secrets/OnceOnlySecretModal.tsx";

/**
 * Every `app/**` page whose import graph reaches `OnceOnlySecretModal`.
 *
 * Pages only — a route handler cannot render a component, and including them
 * would make the result noisier without making it stronger.
 */
export function routesMountingTheModal(): string[] {
  const files = sourceFiles(["app", "components", "lib", "modules"]);
  const known = new Set(files);
  return files
    .filter((file) => file.startsWith("app/") && /\/page\.tsx?$/.test(file))
    .filter((page) => reaches(known, page, ONCE_ONLY_MODAL_MODULE))
    .sort();
}
