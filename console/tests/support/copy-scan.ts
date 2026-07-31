// The scanner behind `tests/unit/lib/no-hardcoded-copy.test.tsx`.
//
// Finds English that a component renders directly instead of resolving through
// the catalog, in the three places it actually hides:
//
//   1. JSX TEXT POSITION   `<p>Contact your administrator.</p>`
//   2. A11Y-BEARING PROPS  `aria-label`, `aria-description`, `title`,
//                          `placeholder`, `alt`
//   3. DEFAULT PARAMETERS  `function X({ label = "Loading" })`
//
// (3) is the one that matters most in this tree: `Spinner.tsx` shipped with
// `label = "Loading"` as a default parameter, which is invisible to any scan
// that only looks at JSX text.
//
// ============================================================================
// THIS IS A HEURISTIC, AND THE POSITIVE CONTROL IS WHY THAT IS ACCEPTABLE
// ============================================================================
//
// There is no TypeScript parser in this workspace's test dependencies, so the
// scan is textual. Textual scans have two failure modes and they are not
// symmetric:
//
//   * a FALSE POSITIVE is loud — someone sees it and fixes the scanner;
//   * a FALSE NEGATIVE is silent, and a scanner that has quietly stopped
//     matching reports zero violations and goes green.
//
// So every narrowing below is paired with an assertion in the test file that the
// scanner still finds the things it claims to find:
// `tests/support/hardcoded-copy-fixture.tsx` carries one instance of each
// violation kind, and floors assert the walk found files and text nodes at all.
//
// ============================================================================
// WHAT IS NARROWED, AND WHY EACH NARROWING IS SAFE
// ============================================================================
//
//   * `{...}` expression containers on ONE LINE are blanked before matching, so
//     `{t(KEY)}` and `{" "}` are not read as text. Multi-line containers are
//     deliberately left alone: blanking by brace depth swallowed entire function
//     bodies (verified — the whole of `FixtureBanner` disappeared), which is a
//     false NEGATIVE and therefore the unacceptable direction.
//   * A candidate containing a TypeScript keyword or an `=` is discarded. Angle
//     brackets in generics (`Omit<InputHTMLAttributes<HTMLInputElement>, "size">`)
//     make `>…<` match ordinary code; `export const Input = forwardRef` was
//     reported as JSX text before this filter existed.
//   * A single-word candidate counts as copy only when it is Capitalised and at
//     least four letters. `neutral`, `primary`, `button`, `md`, `polite` are
//     variant tokens, not prose; `Loading`, `Copy`, `Copied` are prose.
//
// ============================================================================
// THE ALLOWLIST IS CHECKED IN REVERSE
// ============================================================================
//
// An allowlist entry that stops matching anything is "a stale carve-out hiding
// behind a passing test" (`src/i18n/catalog/mod.rs:989-995`), so the test
// asserts both directions: nothing `ALLOWED_GLYPHS` permits may be reported, and
// every entry in it must still be permitting something in the real tree.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";

export const CONSOLE_ROOT = resolve(import.meta.dir, "../..");

export interface CopyViolation {
  readonly file: string;
  readonly kind: "jsx-text" | "a11y-prop" | "default-parameter";
  readonly text: string;
}

export interface CopyScanResult {
  readonly violations: readonly CopyViolation[];
  /** Files opened. A floor on this catches a walk that found nothing. */
  readonly filesScanned: readonly string[];
  /** JSX text nodes seen at all, violating or not. A floor catches a dead regex. */
  readonly jsxTextNodesSeen: number;
  /** Text nodes suppressed by the allowlist, per allowlist entry. */
  readonly allowlistHits: Readonly<Record<string, number>>;
}

/**
 * Fragments that are not copy.
 *
 * Exactly one entry, and the reverse check is what keeps it that way: `*` is the
 * Label atom's required indicator, rendered as a bare glyph inside
 * `aria-hidden="true"` with the real text in `console.a11y.required` beside it.
 * Adding a second entry means adding a second live case for it.
 */
export const ALLOWED_GLYPHS: readonly string[] = ["*"];

/** `"(whitespace)"` when blank, the matched glyph when allow-listed, else null. */
function allowedAs(text: string): string | null {
  const trimmed = text.trim();
  if (trimmed === "") return "(whitespace)";
  return ALLOWED_GLYPHS.find((glyph) => trimmed === glyph) ?? null;
}

/** TypeScript that `>…<` matched by accident rather than JSX text. */
const CODE_TOKENS =
  /\b(export|import|const|let|var|function|return|interface|type|class|extends|implements|typeof|keyof|readonly|await|async|new)\b/;

/**
 * Does this string contain prose?
 *
 * Deliberately not "any string": a className, a CSS value, an id, a route path
 * and a variant token are all strings in a component and none of them is copy.
 */
export function looksLikeCopy(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed === "") return false;
  if (/^[\d\s\p{P}\p{S}]+$/u.test(trimmed)) return false; // digits/punctuation/symbols only
  const words = trimmed.split(/\s+/).filter((word) => /[A-Za-z]/.test(word));
  if (words.length >= 2) return true;
  // A lone lowercase token is a variant/enum value, not a sentence.
  return words.length === 1 && /^[A-Z][A-Za-z'’-]{3,}$/.test(words[0] ?? "");
}

/** Strip comments so prose in a doc block is never reported as copy. */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

/**
 * Blank single-line `{...}` expression containers.
 *
 * Single-line only, on purpose — see the header. This is what makes
 * `<span>{" "}*</span>` read as the glyph `*` rather than as an unmatchable node.
 */
function blankSimpleExpressions(source: string): string {
  let previous = "";
  let current = source;
  for (let pass = 0; pass < 5 && current !== previous; pass += 1) {
    previous = current;
    current = current.replace(/\{[^{}\n]*\}/g, " ");
  }
  return current;
}

const A11Y_PROPS = ["aria-label", "aria-description", "title", "placeholder", "alt"];

/** `>text<` between two tags, with no brace left inside. */
const JSX_TEXT = />([^<>{}]+)</g;
const A11Y_PROP_LITERAL = new RegExp(`\\b(${A11Y_PROPS.join("|")})\\s*=\\s*"([^"]*)"`, "g");
/** `{ label = "Loading" }` / `label = "Loading",` in a parameter list. */
const DEFAULT_PARAMETER = /\b([A-Za-z_$][\w$]*)\s*=\s*(?:"([^"]*)"|'([^']*)')\s*(?=[,}])/g;

/** Every JSX text candidate in a source, pre-filtering. */
function jsxTextCandidates(rawSource: string): string[] {
  const prepared = blankSimpleExpressions(stripComments(rawSource));
  return [...prepared.matchAll(JSX_TEXT)].map((match) => match[1] ?? "");
}

export function scanSourceForCopy(file: string, rawSource: string): CopyViolation[] {
  const violations: CopyViolation[] = [];
  const source = stripComments(rawSource);

  for (const text of jsxTextCandidates(rawSource)) {
    if (allowedAs(text) !== null) continue;
    if (CODE_TOKENS.test(text) || text.includes("=")) continue;
    if (!looksLikeCopy(text)) continue;
    violations.push({ file, kind: "jsx-text", text: text.trim() });
  }

  for (const match of source.matchAll(A11Y_PROP_LITERAL)) {
    const text = match[2] ?? "";
    if (allowedAs(text) !== null) continue;
    if (!looksLikeCopy(text)) continue;
    violations.push({ file, kind: "a11y-prop", text: `${match[1]}="${text.trim()}"` });
  }

  for (const match of source.matchAll(DEFAULT_PARAMETER)) {
    const text = match[2] ?? match[3] ?? "";
    if (allowedAs(text) !== null) continue;
    if (!looksLikeCopy(text)) continue;
    violations.push({ file, kind: "default-parameter", text: `${match[1]} = "${text.trim()}"` });
  }

  return violations;
}

function walk(absDir: string, out: string[]): void {
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
    out.push(abs);
  }
}

/** Scan every layer that renders: atoms, molecules, organisms, and pages. */
export function scanComponentTree(
  roots: readonly string[] = ["components", "modules", "app"],
): CopyScanResult {
  const absolutes: string[] = [];
  for (const root of roots) walk(join(CONSOLE_ROOT, root), absolutes);
  absolutes.sort();

  const violations: CopyViolation[] = [];
  const filesScanned: string[] = [];
  const allowlistHits: Record<string, number> = {};
  let jsxTextNodesSeen = 0;

  for (const abs of absolutes) {
    const file = relative(CONSOLE_ROOT, abs).replace(/\\/g, "/");
    const raw = readFileSync(abs, "utf8");
    filesScanned.push(file);

    for (const text of jsxTextCandidates(raw)) {
      jsxTextNodesSeen += 1;
      const allowed = allowedAs(text);
      if (allowed !== null) allowlistHits[allowed] = (allowlistHits[allowed] ?? 0) + 1;
    }

    violations.push(...scanSourceForCopy(file, raw));
  }

  return { violations, filesScanned, jsxTextNodesSeen, allowlistHits };
}
