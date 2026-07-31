// Static guards for the modules that carry Moira credentials.
//
// WHY THIS IS NOT `import "server-only"` YET. That package's non-`react-server`
// export throws on import by design, which is exactly what makes it a build
// guard — and exactly what makes it unimportable under `bun test`. It is also
// not a dependency of this workspace yet. It lands with the Next.js app wiring
// (together with a `bunfig` preload shim), at which point these tests stay as
// the belt to its braces. Until then this is the whole of the enforcement, so it
// is deliberately strict: a `"use client"` file importing a credential-carrying
// module fails here, statically, with no runtime needed.

import { describe, expect, test } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { extname, join, relative, resolve } from "node:path";

const CONSOLE_ROOT = resolve(import.meta.dir, "../../..");

/** Modules that hold or forward a Moira credential. */
const SERVER_ONLY_MODULES = [
  "lib/moira-client.ts",
  "lib/setup-flow.ts",
  // Plan 09 Wave 1: the durable store and the pool that reaches it. A client
  // component importing either would drag the connection string — password
  // included — and the plaintext-revealing `reveal()` into the browser bundle.
  "lib/console-db.ts",
  "lib/console-secrets-postgres.ts",
] as const;

/** Modules that must stay importable from a client component. */
const CLIENT_SAFE_MODULES = ["lib/errors.ts", "lib/types.ts", "lib/moira-keys.ts"] as const;

function listSourceFiles(absDir: string): string[] {
  let entries: string[];
  try {
    entries = readdirSync(absDir);
  } catch {
    return [];
  }
  const files: string[] = [];
  for (const entry of entries) {
    if (entry === "node_modules" || entry === ".next") continue;
    const abs = join(absDir, entry);
    if (statSync(abs).isDirectory()) {
      files.push(...listSourceFiles(abs));
      continue;
    }
    const ext = extname(entry);
    if (ext === ".ts" || ext === ".tsx") files.push(abs);
  }
  return files;
}

const allSourceFiles = [
  ...listSourceFiles(join(CONSOLE_ROOT, "lib")),
  ...listSourceFiles(join(CONSOLE_ROOT, "app")),
  ...listSourceFiles(join(CONSOLE_ROOT, "modules")),
  ...listSourceFiles(join(CONSOLE_ROOT, "components")),
];

const rel = (abs: string) => relative(CONSOLE_ROOT, abs).replace(/\\/g, "/");

/**
 * Drop comments before scanning.
 *
 * Every rule in this file is about what the CODE does. A guard that also fires
 * on prose makes the prose unwritable — and the prose is where the reasons for
 * these rules live, so a guard that punishes it is working against itself.
 */
function stripComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
}

function importsAny(source: string, moduleNames: readonly string[]): boolean {
  return moduleNames.some((moduleName) => {
    const stem = moduleName.replace(/^lib\//, "").replace(/\.tsx?$/, "");
    return (
      source.includes(`@/lib/${stem}`) ||
      new RegExp(`from\\s+["'][./]+${stem}["']`).test(source) ||
      new RegExp(`import\\s*\\(\\s*["'][./]+${stem}["']`).test(source)
    );
  });
}

describe("credential-carrying modules are marked and contained", () => {
  for (const moduleName of SERVER_ONLY_MODULES) {
    test(`${moduleName} declares the @server-only marker`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, moduleName), "utf8");
      expect(source.startsWith("// @server-only")).toBe(true);
    });
  }

  test('no "use client" file imports a credential-carrying module', () => {
    const violations = allSourceFiles
      .filter((abs) => {
        const source = readFileSync(abs, "utf8");
        const isClient = /^\s*["']use client["']/m.test(source);
        return isClient && importsAny(source, SERVER_ONLY_MODULES);
      })
      .map(rel);
    expect(violations).toEqual([]);
  });

  test("nothing under components/** imports a credential-carrying module", () => {
    const violations = listSourceFiles(join(CONSOLE_ROOT, "components"))
      .filter((abs) => importsAny(readFileSync(abs, "utf8"), SERVER_ONLY_MODULES))
      .map(rel);
    expect(violations).toEqual([]);
  });
});

describe("the client-safe modules really are client-safe", () => {
  for (const moduleName of CLIENT_SAFE_MODULES) {
    test(`${moduleName} names no credential header and reads no credential`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, moduleName), "utf8");
      // A credential-shaped literal in a module the browser may load is the
      // whole failure mode; the doc comment in moira-client.ts is allowed to
      // mention the header name because that module is server-only.
      const codeOnly = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
      expect(codeOnly).not.toContain("X-Moira-System-Key");
      expect(codeOnly).not.toContain("Authorization");
      expect(codeOnly).not.toMatch(/process\.env/);
    });
  }
});

describe("the error boundary is enforced in one place", () => {
  test("request_id and details are read only inside lib/errors.ts", () => {
    const offenders = allSourceFiles
      .filter((abs) => rel(abs) !== "lib/errors.ts")
      .filter((abs) => {
        const source = readFileSync(abs, "utf8").replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
        return /\.request_id\b/.test(source) || /\.details\b/.test(source);
      })
      .map(rel);
    // Every consumer goes through `toMoiraError` / `serverDiagnostics`. A second
    // reader is a second place the boundary can be broken.
    expect(offenders).toEqual([]);
  });

  test("nothing outside lib/errors.ts constructs a client-facing error by spreading the envelope", () => {
    const offenders = allSourceFiles
      .filter((abs) => rel(abs) !== "lib/errors.ts")
      .filter((abs) => /\.\.\.\s*\w*[eE]rror\.error\b/.test(readFileSync(abs, "utf8")))
      .map(rel);
    expect(offenders).toEqual([]);
  });
});

describe("the database layer stays on the server", () => {
  test("no client component and nothing under components/** imports a database driver", () => {
    // Distinct from the module rule above: a component could import `pg`
    // directly rather than through `lib/console-db.ts`, and Next would then
    // attempt to bundle a database driver — and the connection string it reads
    // — for the browser.
    const driverImport = /from\s+["']pg["']|require\(\s*["']pg["']\s*\)|import\(\s*["']pg["']\s*\)/;
    const offenders = allSourceFiles
      .filter((abs) => {
        const source = readFileSync(abs, "utf8");
        const isClient = /^\s*["']use client["']/m.test(source);
        const isComponent = rel(abs).startsWith("components/");
        return (isClient || isComponent) && driverImport.test(source);
      })
      .map(rel);
    expect(offenders).toEqual([]);
  });

  test("the connection string is read in exactly two places", () => {
    // `lib/env.ts` validates it and `lib/console-db.ts` consumes it — the
    // latter also exports `hasConsoleDatabase` so that "am I durable?" can be
    // asked without a third module touching the value. A third reader is a
    // third chance to log it, embed it in an error, or pass it as a prop, and
    // it carries the database password inline.
    const offenders = allSourceFiles
      .filter((abs) => !["lib/env.ts", "lib/console-db.ts"].includes(rel(abs)))
      .filter((abs) => {
        const codeOnly = stripComments(readFileSync(abs, "utf8"));
        return codeOnly.includes("CONSOLE_DATABASE_URL") || /\bconsoleDatabaseUrl\b/.test(codeOnly);
      })
      .map(rel);
    expect(offenders).toEqual([]);
  });

  test("the durable store is the only SQL against the secret table", () => {
    // One place where a client secret is read out of storage. A second query
    // would be a second decrypt path, and the AAD binding only protects the
    // path that uses it.
    const offenders = allSourceFiles
      .filter((abs) => rel(abs) !== "lib/console-secrets-postgres.ts")
      .filter((abs) => stripComments(readFileSync(abs, "utf8")).includes("console_provider_secret"))
      .map(rel);
    expect(offenders).toEqual([]);
  });
});

describe("no rotate-secret anywhere", () => {
  test("the literal `rotate-secret` appears in no source file", () => {
    const offenders = allSourceFiles
      .filter((abs) => readFileSync(abs, "utf8").includes("rotate-secret"))
      .map(rel);
    expect(offenders).toEqual([]);
  });

  test("no Moira DTO in lib/types.ts declares a secret-shaped field", () => {
    const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
    const codeOnly = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
    const fieldNames = [...codeOnly.matchAll(/^\s{2}(\w+)\??:/gm)].map((match) => match[1] ?? "");
    const offenders = fieldNames.filter((name) => /(secret|masked|fingerprint)/i.test(name));
    expect(offenders).toEqual([]);
  });
});
