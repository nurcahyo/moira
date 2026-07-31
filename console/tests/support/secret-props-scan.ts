// The scanner behind `tests/unit/architecture/no-secret-props.test.ts`.
//
// CONVENTIONS.md §6 rule 5: a secret must never be passed as a prop past the
// page/server boundary, because organisms, molecules and atoms all render
// client-side. `plans/08:1438` specifies the check as a NAME scan over `*Props`
// interfaces.
//
// Two rules, because the name scan alone cannot express the property that
// matters:
//
//   (a) NAME  — no `*Props` interface in `components/atoms/**`,
//               `components/molecules/**` or `modules/**` declares a
//               secret-shaped member, except in an explicitly allow-listed file.
//   (b) FLOW  — inside an allow-listed file, the secret-bearing identifier may
//               not be handed to another component as a prop. Plan 09:324 states
//               the intent in prose — "the invite token appears exactly once, in
//               `OnceOnlySecretModal`, and never as a prop on a reusable
//               component beyond that modal's own render" — and nothing
//               expressed it.
//
// (b) IS A HEURISTIC. It matches three spellings (`<X secret=`,
// `<X value={secret}`, `{...{ secret }}`) and a determined author can defeat it
// by renaming through an intermediate. The real gate is the e2e needle: a fixed
// fake token seeded into the Moira stub's once-only response and added to
// `forbiddenValues()`, which fails if the value reaches the browser by ANY
// route. This file is the fast, local, always-on half.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";

export const CONSOLE_ROOT = resolve(import.meta.dir, "../..");

/** Layers whose files render client-side and therefore may not hold a secret. */
export const RENDERING_ROOTS = ["components/atoms", "components/molecules", "modules"] as const;

/**
 * A prop name that names a credential.
 *
 * Source: `plans/CONVENTIONS.md:159` §6 rule 5, specified at `plans/08:1438`.
 */
export const SECRET_PROP_PATTERN =
  /(secret|systemKey|privateKey|clientSecret|apiKey|token|password|credential)/i;

export interface PropsInterface {
  readonly file: string;
  readonly name: string;
  readonly members: readonly string[];
}

export interface SecretPropViolation {
  readonly file: string;
  readonly interfaceName: string;
  readonly member: string;
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

export function renderingSourceFiles(roots: readonly string[] = RENDERING_ROOTS): string[] {
  const found: string[] = [];
  for (const root of roots) walk(join(CONSOLE_ROOT, root), found);
  return found.map((abs) => relative(CONSOLE_ROOT, abs).replace(/\\/g, "/")).sort();
}

function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

/** `interface XProps { … }` / `type XProps = { … }` bodies and their members. */
export function extractPropsInterfaces(file: string, rawSource: string): PropsInterface[] {
  const source = stripComments(rawSource);
  const found: PropsInterface[] = [];
  // Anchored to the start of a line, and stopped by `;`. Without the anchor,
  // `import { Input, type InputProps } from "../atoms/Input";` matched — `type
  // InputProps` followed by the `{` of the NEXT import statement — and produced
  // a phantom interface with zero members, i.e. an interface whose members were
  // never checked. The "parsed with zero members" assertion in the test is what
  // caught it.
  const declaration = /^[ \t]*(?:export\s+)?(?:interface|type)\s+([A-Za-z0-9_]*Props)\b[^{;]*\{/gm;

  for (const match of source.matchAll(declaration)) {
    const name = match[1] ?? "";
    // Walk to the matching close brace so a nested object type does not end the
    // body early.
    let depth = 0;
    let index = (match.index ?? 0) + match[0].length - 1;
    const start = index + 1;
    for (; index < source.length; index += 1) {
      const character = source[index];
      if (character === "{") depth += 1;
      else if (character === "}") {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    const body = source.slice(start, index);
    const members = [
      ...body.matchAll(/(?:^|\n)\s*(?:readonly\s+)?["']?([A-Za-z0-9_-]+)["']?\s*\??\s*:/g),
    ]
      .map((member) => member[1] ?? "")
      .filter((member) => member !== "");
    found.push({ file, name, members });
  }
  return found;
}

/** Rule (a): secret-shaped members on a rendering layer's props. */
export function secretPropViolations(interfaces: readonly PropsInterface[]): SecretPropViolation[] {
  const violations: SecretPropViolation[] = [];
  for (const declared of interfaces) {
    for (const member of declared.members) {
      if (SECRET_PROP_PATTERN.test(member)) {
        violations.push({ file: declared.file, interfaceName: declared.name, member });
      }
    }
  }
  return violations;
}

/**
 * Rule (b): the secret identifier handed to another component as a prop.
 *
 * Three spellings, all of which put the value into a child's props object:
 *
 *   <Child secret={secret} />        an explicit prop named for the secret
 *   <Child value={secret} />         the secret under any prop name
 *   <Child {...{ secret }} />        spread of an object holding it
 */
export function secretFlowViolations(rawSource: string, identifier = "secret"): string[] {
  const source = stripComments(rawSource);
  const escaped = identifier.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const patterns: ReadonlyArray<{ readonly label: string; readonly pattern: RegExp }> = [
    {
      label: `a child component receives a \`${identifier}\` prop`,
      pattern: new RegExp(`<[A-Z][A-Za-z0-9_]*(?:\\s[^>]*)?\\s${escaped}\\s*=`, "s"),
    },
    {
      label: `a child component receives {${identifier}} under another prop name`,
      // The negative lookahead keeps `<X secret={secret} />` from being reported
      // twice — it is one violation, and a count assertion in the test would
      // otherwise have to encode the double-report as if it were meaningful.
      pattern: new RegExp(
        `<[A-Z][A-Za-z0-9_]*(?:\\s[^>]*?)?\\s(?!${escaped}\\s*=)[A-Za-z0-9_-]+\\s*=\\s*\\{\\s*${escaped}\\s*\\}`,
        "s",
      ),
    },
    {
      label: `a child component receives \`${identifier}\` through a spread`,
      pattern: new RegExp(`<[A-Z][A-Za-z0-9_]*(?:\\s[^>]*?)?\\s\\{\\.\\.\\.[^}]*${escaped}`, "s"),
    },
  ];
  return patterns.filter(({ pattern }) => pattern.test(source)).map(({ label }) => label);
}

/** Read a console-relative file. */
export function readConsoleFile(path: string): string {
  return readFileSync(join(CONSOLE_ROOT, path), "utf8");
}

/** Does a console-relative path exist? */
export function consoleFileExists(path: string): boolean {
  try {
    statSync(join(CONSOLE_ROOT, path));
    return true;
  } catch {
    return false;
  }
}
