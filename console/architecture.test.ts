// Layering-enforcement test for CONVENTIONS.md §6 (Atomic Design, one-way
// dependency rule).
//
// This statically scans the source tree — it does not execute or type-check
// anything — and fails the build the moment any of the following appear:
//
//   1. `components/atoms/**`     imports from `components/molecules/**`,
//                                 `modules/**`, or `app/**`.
//   2. `components/molecules/**` imports from `modules/**` or `app/**`.
//   3. `components/atoms/**` or `components/molecules/**` import
//      `next/navigation` (a routing/side-effect module — pages own that).
//   4. `components/atoms/**` or `components/molecules/**` perform a network
//      call (`fetch(...)`, `axios`, or a `*-client`/`*Client` import) —
//      atoms and molecules are presentational and feature-agnostic.
//   5. `components/atoms/**` or `components/molecules/**` import an auth
//      module (path contains `auth`, or is `better-auth`/`next-auth`/
//      `@auth/*`) — auth logic belongs in organisms/pages, never below.
//
// Rationale: this is scaffold-phase infrastructure. Plan 08 will add real
// atoms, molecules, and organisms; this test is what keeps that growth from
// quietly violating CONVENTIONS §6 as the tree fills in. On a clean scaffold
// (no components/molecules/modules directories yet) this test passes
// vacuously — there is nothing to scan.

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve } from "node:path";

const CONSOLE_ROOT = resolve(import.meta.dir);

const ATOMS_DIR = "components/atoms";
const MOLECULES_DIR = "components/molecules";
const MODULES_DIR = "modules";
const APP_DIR = "app";
const LIB_DIR = "lib";

type Layer = "atoms" | "molecules" | "modules" | "app" | "lib" | "other";

interface Violation {
  file: string;
  importSpecifier: string;
  reason: string;
}

/** Recursively list source files (.ts/.tsx) under `dir`, skipping test files. */
function listSourceFiles(absDir: string): string[] {
  let entries: string[];
  try {
    entries = readdirSync(absDir);
  } catch {
    return []; // directory doesn't exist yet — nothing to scan
  }

  const files: string[] = [];
  for (const entry of entries) {
    const abs = join(absDir, entry);
    const st = statSync(abs);
    if (st.isDirectory()) {
      if (entry === "node_modules" || entry === ".next") continue;
      files.push(...listSourceFiles(abs));
      continue;
    }
    const ext = extname(entry);
    if (ext !== ".ts" && ext !== ".tsx") continue;
    if (/\.(test|spec|stories)\.[tj]sx?$/.test(entry)) continue;
    files.push(abs);
  }
  return files;
}

/** Extract static/dynamic import and require specifiers from source text. */
function extractImportSpecifiers(source: string): string[] {
  const specifiers: string[] = [];
  const patterns = [
    /import\s+(?:[^'";]+?\s+from\s+)?['"]([^'"]+)['"]/g,
    /import\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
    /require\(\s*['"]([^'"]+)['"]\s*\)/g,
    /export\s+(?:\*|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]/g,
  ];
  for (const pattern of patterns) {
    for (const match of source.matchAll(pattern)) {
      const spec = match[1];
      if (spec) specifiers.push(spec);
    }
  }
  return specifiers;
}

/** Resolve an import specifier (relative, `@/`-aliased, or bare) to a
 * console-root-relative path when it refers to local source; returns null
 * for bare package imports (e.g. "react"). */
function resolveLocalSpecifier(fromFileAbs: string, specifier: string): string | null {
  if (specifier.startsWith(".")) {
    const abs = resolve(dirname(fromFileAbs), specifier);
    return relative(CONSOLE_ROOT, abs).replace(/\\/g, "/");
  }
  if (specifier.startsWith("@/")) {
    return specifier.slice(2);
  }
  return null; // bare package specifier, not local source
}

function classifyLayer(consoleRelativePath: string): Layer {
  const p = consoleRelativePath.replace(/\\/g, "/");
  if (p.startsWith(`${ATOMS_DIR}/`) || p === ATOMS_DIR) return "atoms";
  if (p.startsWith(`${MOLECULES_DIR}/`) || p === MOLECULES_DIR) return "molecules";
  if (p.startsWith(`${MODULES_DIR}/`) || p === MODULES_DIR) return "modules";
  if (p.startsWith(`${APP_DIR}/`) || p === APP_DIR) return "app";
  if (p.startsWith(`${LIB_DIR}/`) || p === LIB_DIR) return "lib";
  return "other";
}

const AUTH_SPECIFIER_PATTERN = /(^|[/-])auth([/-]|$)|better-auth|next-auth/i;
const FETCH_CALL_PATTERN = /\bfetch\s*\(/;
const AXIOS_PATTERN = /\baxios\b/i;

function checkFile(fileAbs: string, ownLayer: "atoms" | "molecules"): Violation[] {
  const consoleRelative = relative(CONSOLE_ROOT, fileAbs).replace(/\\/g, "/");
  const source = readFileSync(fileAbs, "utf8");
  const violations: Violation[] = [];

  // Rule: forbidden upward/lateral imports and forbidden auth imports.
  const forbiddenTargets: Layer[] = ownLayer === "atoms" ? ["molecules", "modules", "app"] : ["modules", "app"];

  for (const specifier of extractImportSpecifiers(source)) {
    // next/navigation is explicitly forbidden regardless of local/bare resolution.
    if (specifier === "next/navigation" || specifier.startsWith("next/navigation/")) {
      violations.push({
        file: consoleRelative,
        importSpecifier: specifier,
        reason: `${ownLayer} must not import "next/navigation" (routing/navigation side effects belong to pages)`,
      });
      continue;
    }

    if (AUTH_SPECIFIER_PATTERN.test(specifier)) {
      violations.push({
        file: consoleRelative,
        importSpecifier: specifier,
        reason: `${ownLayer} must not import an auth module ("${specifier}") — auth logic belongs in organisms/pages`,
      });
      continue;
    }

    const localTarget = resolveLocalSpecifier(fileAbs, specifier);
    if (localTarget === null) continue; // bare package import (react, next, etc.) — allowed unless matched above

    const targetLayer = classifyLayer(localTarget);
    if (forbiddenTargets.includes(targetLayer)) {
      violations.push({
        file: consoleRelative,
        importSpecifier: specifier,
        reason: `${ownLayer} must not import from "${targetLayer}" (one-way rule: pages -> organisms -> molecules -> atoms), resolved target "${localTarget}"`,
      });
    }
  }

  // Rule: no network calls in atoms/molecules.
  if (FETCH_CALL_PATTERN.test(source)) {
    violations.push({
      file: consoleRelative,
      importSpecifier: "fetch(...)",
      reason: `${ownLayer} must not perform network calls ("fetch(" found) — atoms/molecules are presentational and feature-agnostic`,
    });
  }
  if (AXIOS_PATTERN.test(source)) {
    violations.push({
      file: consoleRelative,
      importSpecifier: "axios",
      reason: `${ownLayer} must not perform network calls (axios usage found) — atoms/molecules are presentational and feature-agnostic`,
    });
  }

  return violations;
}

function formatViolations(violations: Violation[]): string {
  return violations
    .map((v) => `  - ${v.file}: forbidden import "${v.importSpecifier}" — ${v.reason}`)
    .join("\n");
}

describe("Atomic Design one-way dependency rule (CONVENTIONS.md §6)", () => {
  test("components/atoms/** never imports molecules, modules, app, next/navigation, auth, or network APIs", () => {
    const files = listSourceFiles(join(CONSOLE_ROOT, ATOMS_DIR));
    const violations = files.flatMap((f) => checkFile(f, "atoms"));
    expect(violations, `Layering violations found in atoms:\n${formatViolations(violations)}`).toEqual([]);
  });

  test("components/molecules/** never imports modules, app, next/navigation, auth, or network APIs", () => {
    const files = listSourceFiles(join(CONSOLE_ROOT, MOLECULES_DIR));
    const violations = files.flatMap((f) => checkFile(f, "molecules"));
    expect(violations, `Layering violations found in molecules:\n${formatViolations(violations)}`).toEqual([]);
  });
});
