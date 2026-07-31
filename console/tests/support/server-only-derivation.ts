// Which modules carry a credential — DERIVED from the source, not listed.
//
// ============================================================================
// THE HOLE THIS CLOSES
// ============================================================================
//
// Before this module there were two hand-maintained arrays:
//
//   `SERVER_ONLY_MODULES`     in server-only-guards.test.ts   — 4 entries
//   `MUST_IMPORT_SERVER_ONLY` in server-only-import.test.ts   — 10 entries
//
// They disagreed on SIX of ten, and nothing asserted they agreed. The four in
// the smaller list are the ones the reachability rules ("no `use client` file
// imports one", "nothing under components/** imports one") actually used — so
// `lib/console-secrets.ts` and `lib/auth-config.ts`, the two modules that hold
// PLAINTEXT (`console-secrets.ts:185` `reveal()`, `auth-config.ts:118`
// `clientSecret: string`), were outside every reachability rule.
//
// `server-only-import.test.ts:33-34` admits the underlying problem in its own
// prose: "adding the import without adding the module here is silently fine".
// A new credential module was therefore covered by nothing until somebody
// remembered to edit two arrays.
//
// ============================================================================
// WHY THIS IS NOT DERIVED FROM `import "server-only"` ITSELF
// ============================================================================
//
// The obvious derivation — "the credential modules are the ones that carry the
// marker" — is self-defeating. Delete the marker and the module leaves the
// derived set, both consumers still agree with the derivation, and every gate
// stays green while the guard evaporates. That is the same shape as the
// allowlist-that-matches-nothing failure.
//
// So the derivation runs on CREDENTIAL SHAPE instead, and `import "server-only"`
// becomes the thing ASSERTED rather than the thing measured:
//
//   SEED   a module that names a credential mechanism directly — reads
//          `process.env`, sends `X-Moira-System-Key` or `Authorization`,
//          imports the `pg` driver, constructs an AEAD, or handles a
//          `clientSecret`.
//   CLOSE  a module that value-imports a seed. Reachability is transitive:
//          `lib/setup-flow.ts` holds no credential of its own, it holds a
//          `MoiraClient` that holds one.
//
// A module added in a later wave (`lib/github.ts`, `lib/invites.ts`) is covered
// on the commit that adds it, with no array to remember.
//
// ============================================================================
// WHY `db/**` IS SCANNED BUT NEVER REQUIRED TO CARRY THE MARKER
// ============================================================================
//
// `db/dsn.ts`, `db/migrate.ts` and `db/generate.ts` import `pg` and read
// `CONSOLE_DATABASE_URL`, so they are unambiguously credential-carrying and they
// belong in every REACHABILITY scan — they were in none before this commit.
//
// They must nonetheless NOT import `server-only`: they are plain `bun run`
// scripts (`bun run db:migrate`), the package's `default` export condition is a
// bare `throw`, and `bunfig.toml`'s preload shim applies to `bun test` only. The
// import would break the migration entrypoint, and `db/dsn.ts:3-8` already
// states this as a deliberate property. The containment argument for `db/` is
// different and stronger: nothing under `lib/`, `app/`, `components/` or
// `modules/` imports it at all, which is asserted directly.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";

export const CONSOLE_ROOT = resolve(import.meta.dir, "../..");

/** Directories that ship, in dependency order of interest. */
export const SOURCE_ROOTS = ["lib", "app", "components", "modules", "db"] as const;

/** The subset whose modules Next.js may bundle into a page. */
export const BUNDLED_ROOT = "lib";

export interface SourceFile {
  /** Console-root-relative, forward slashes. */
  readonly path: string;
  readonly source: string;
  /** Comments removed. Every rule here is about what the CODE does. */
  readonly code: string;
}

export function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

function walk(absDir: string, out: SourceFile[]): void {
  let entries: string[];
  try {
    entries = readdirSync(absDir);
  } catch {
    return;
  }
  for (const entry of entries) {
    if (entry === "node_modules" || entry === ".next") continue;
    const abs = join(absDir, entry);
    if (statSync(abs).isDirectory()) {
      walk(abs, out);
      continue;
    }
    const ext = extname(entry);
    if (ext !== ".ts" && ext !== ".tsx") continue;
    if (/\.(test|spec|stories)\.[tj]sx?$/.test(entry)) continue;
    const source = readFileSync(abs, "utf8");
    out.push({
      path: relative(CONSOLE_ROOT, abs).replace(/\\/g, "/"),
      source,
      code: stripComments(source),
    });
  }
}

/** Every shipped source file under `SOURCE_ROOTS`. */
export function allSourceFiles(): SourceFile[] {
  const files: SourceFile[] = [];
  for (const root of SOURCE_ROOTS) walk(join(CONSOLE_ROOT, root), files);
  files.sort((a, b) => a.path.localeCompare(b.path));
  return files;
}

/* -------------------------------------------------------------------------- */
/* Import graph                                                               */
/* -------------------------------------------------------------------------- */

/**
 * Import specifiers, split by whether the statement was `import type`.
 *
 * Type-only edges are excluded from the credential closure: `import type
 * { MoiraClient } from "./moira-client"` erases at compile time and drags no
 * runtime code, so treating it as reachability would make a module
 * credential-carrying for naming a type. Every other form — including
 * `import { type Foo, bar }` — counts as a value edge.
 */
export function importSpecifiers(code: string): { value: string[]; typeOnly: string[] } {
  const value: string[] = [];
  const typeOnly: string[] = [];
  const patterns: ReadonlyArray<RegExp> = [
    /import\s+(type\s+)?(?:[^'";]*?\s+from\s+)?['"]([^'"]+)['"]/g,
    /import\s*\(\s*()['"]([^'"]+)['"]\s*\)/g,
    /require\(\s*()['"]([^'"]+)['"]\s*\)/g,
    /export\s+(type\s+)?(?:\*|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]/g,
  ];
  for (const pattern of patterns) {
    for (const match of code.matchAll(pattern)) {
      const specifier = match[2];
      if (specifier === undefined) continue;
      if (match[1] !== undefined && match[1].trim() === "type") typeOnly.push(specifier);
      else value.push(specifier);
    }
  }
  return { value, typeOnly };
}

/**
 * Resolve a specifier to a console-relative module path, or null when it is a
 * bare package import.
 *
 * The same two forms `architecture.test.ts:92-101` resolves. `importsAny` in
 * `server-only-guards.test.ts:73-82` handled only `@/lib/<stem>` and
 * same-directory relatives, so `from "../../lib/console-db"` slipped through it.
 */
export function resolveLocalSpecifier(fromPath: string, specifier: string): string | null {
  if (specifier.startsWith(".")) {
    const abs = resolve(dirname(join(CONSOLE_ROOT, fromPath)), specifier);
    return relative(CONSOLE_ROOT, abs).replace(/\\/g, "/");
  }
  if (specifier.startsWith("@/")) return specifier.slice(2);
  return null;
}

/** Map a resolved specifier onto an actual file path in `files`. */
export function matchModule(files: readonly SourceFile[], resolved: string): string | null {
  const candidates = [resolved, `${resolved}.ts`, `${resolved}.tsx`, `${resolved}/index.ts`];
  for (const candidate of candidates) {
    if (files.some((file) => file.path === candidate)) return candidate;
  }
  return null;
}

/**
 * Every module `file` reaches directly.
 *
 * `typeOnly: false` drops `import type` edges — see `importSpecifiers`.
 */
export function directImports(
  files: readonly SourceFile[],
  file: SourceFile,
  options: { readonly includeTypeOnly: boolean },
): string[] {
  const { value, typeOnly } = importSpecifiers(file.code);
  const specifiers = options.includeTypeOnly ? [...value, ...typeOnly] : value;
  const targets: string[] = [];
  for (const specifier of specifiers) {
    const local = resolveLocalSpecifier(file.path, specifier);
    if (local === null) continue;
    const matched = matchModule(files, local);
    if (matched !== null) targets.push(matched);
  }
  return targets;
}

/* -------------------------------------------------------------------------- */
/* Credential seeds                                                           */
/* -------------------------------------------------------------------------- */

/**
 * Direct evidence that a module handles a credential.
 *
 * Each entry is asserted to still match something in `layer-dependencies.test.ts`
 * — a seed that matches nothing is a rule that has stopped applying, and it
 * would shrink the derived set silently.
 */
export const CREDENTIAL_SEEDS: ReadonlyArray<{ readonly label: string; readonly pattern: RegExp }> =
  [
    { label: "reads process.env", pattern: /\bprocess\.env\b/ },
    { label: "imports the pg driver", pattern: /from\s+["']pg["']|require\(\s*["']pg["']\s*\)/ },
    { label: "sends the Moira system key", pattern: /X-Moira-System-Key/ },
    { label: "sends an Authorization header", pattern: /["']Authorization["']/ },
    { label: "constructs an AEAD", pattern: /create(?:Cipher|Decipher)iv/ },
    { label: "handles an OAuth client secret", pattern: /\bclientSecret\b/ },
    { label: "constructs the Better Auth instance", pattern: /\bbetterAuth\s*\(/ },
    { label: "names the console database URL", pattern: /\bCONSOLE_DATABASE_URL\b/ },
  ];

export interface CredentialModule {
  readonly path: string;
  /** `"seed"` — direct evidence; `"reachable"` — value-imports one that has it. */
  readonly why: "seed" | "reachable";
  /** Seed labels, or the seed module reached. */
  readonly evidence: readonly string[];
}

/** Seed labels matched by this file, if any. */
export function seedEvidence(file: SourceFile): string[] {
  return CREDENTIAL_SEEDS.filter((seed) => seed.pattern.test(file.code)).map((seed) => seed.label);
}

/**
 * Every module under `lib/` that holds or reaches a credential.
 *
 * Restricted to `lib/` because that is the set Next.js may bundle into a page
 * and therefore the set for which `import "server-only"` is the build guard.
 * `db/**` is credential-carrying and deliberately excluded — see the header.
 */
export function deriveCredentialModules(files: readonly SourceFile[]): CredentialModule[] {
  const bundled = files.filter((file) => file.path.startsWith(`${BUNDLED_ROOT}/`));

  const result = new Map<string, CredentialModule>();
  for (const file of bundled) {
    const evidence = seedEvidence(file);
    if (evidence.length > 0) result.set(file.path, { path: file.path, why: "seed", evidence });
  }

  // Transitive closure over value imports. Bounded rather than `while (true)`:
  // a cycle plus a bug would otherwise spin forever inside a test.
  for (let pass = 0; pass < bundled.length + 1; pass += 1) {
    let grew = false;
    for (const file of bundled) {
      if (result.has(file.path)) continue;
      const reached = directImports(files, file, { includeTypeOnly: false }).filter((target) =>
        result.has(target),
      );
      if (reached.length > 0) {
        result.set(file.path, { path: file.path, why: "reachable", evidence: reached.sort() });
        grew = true;
      }
    }
    if (!grew) break;
  }

  return [...result.values()].sort((a, b) => a.path.localeCompare(b.path));
}

/** The derived paths only — what the two consumer test files use. */
export function deriveCredentialModulePaths(
  files: readonly SourceFile[] = allSourceFiles(),
): string[] {
  return deriveCredentialModules(files).map((entry) => entry.path);
}

/** A real side-effect import, not a mention in a comment. */
export function hasServerOnlyImport(source: string): boolean {
  return /^\s*import\s+["']server-only["']\s*;?\s*$/m.test(stripComments(source));
}

/** Does `file` import any of `targets` (relative, `@/`-aliased, or dynamic)? */
export function importsAnyOf(
  files: readonly SourceFile[],
  file: SourceFile,
  targets: readonly string[],
): string[] {
  const reached = new Set(directImports(files, file, { includeTypeOnly: true }));
  return targets.filter((target) => reached.has(target));
}

/** `"use client"` at the top of the module. */
export function isClientComponent(source: string): boolean {
  return /^\s*["']use client["']/m.test(source);
}
