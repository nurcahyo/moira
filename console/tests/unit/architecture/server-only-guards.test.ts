// Static guards for the modules that carry Moira credentials.
//
// WHY THIS IS NOT `import "server-only"` ALONE. That package's non-`react-server`
// export throws on import by design, which is exactly what makes it a build
// guard — and exactly what makes it unimportable under `bun test` without the
// `bunfig` preload shim. This file is the belt to its braces: a `"use client"`
// file importing a credential-carrying module fails here, statically, with no
// runtime needed.
//
// ============================================================================
// THE MODULE LIST IS NO LONGER A LIST (plan 09 Wave 3)
// ============================================================================
//
// `SERVER_ONLY_MODULES` used to be four hand-written entries here, while
// `server-only-import.test.ts` held ten. They disagreed on six of ten, nothing
// asserted they agreed, and the four in THIS file were the ones the reachability
// rules below actually used — so `lib/console-secrets.ts` and
// `lib/auth-config.ts`, the two modules that hold plaintext, were outside every
// reachability rule.
//
// Both files now consume `deriveCredentialModulePaths()`, which derives the set
// from credential SHAPE (see `tests/support/server-only-derivation.ts`). A new
// credential module is covered on the commit that adds it.
//
// `importsAny` is gone too: it matched only `@/lib/<stem>` and same-directory
// relatives, so `from "../../lib/console-db"` slipped through it.
// `importsAnyOf` resolves both forms, and `layer-dependencies.test.ts` carries a
// positive control for the deep-relative case.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

import {
  allSourceFiles,
  deriveCredentialModulePaths,
  importsAnyOf,
  isClientComponent,
} from "../../support/server-only-derivation";

const CONSOLE_ROOT = resolve(import.meta.dir, "../../..");

/** Derived, never listed. */
const SERVER_ONLY_MODULE_PATHS = deriveCredentialModulePaths();

/** Modules that must stay importable from a client component. */
const CLIENT_SAFE_MODULES = [
  "lib/errors.ts",
  "lib/types.ts",
  "lib/moira-keys.ts",
  // The console catalog. It must be reachable from an atom — `Spinner` and
  // `Label` render their a11y strings through it — so it is asserted
  // credential-free rather than merely assumed to be.
  "lib/i18n/keys.ts",
  "lib/i18n/catalog.en.ts",
  "lib/i18n/index.ts",
] as const;

/**
 * Every shipped source file, INCLUDING `db/**`.
 *
 * `db/` was in no scan set before this commit: it imports `pg`, reads the DSN,
 * and carries no `import "server-only"` (deliberately — it runs under a plain
 * `bun run`). Adding it is what makes the `CONSOLE_DATABASE_URL` reader count
 * below honest.
 */
const sourceFiles = allSourceFiles();

/**
 * The only modules permitted to name the connection string.
 *
 * Three, not two — see the test that consumes this.
 */
const CONNECTION_STRING_READERS = ["db/dsn.ts", "lib/console-db.ts", "lib/env.ts"] as const;

describe("credential-carrying modules are marked and contained", () => {
  test("the derived set is non-empty and covers the plaintext holders", () => {
    expect(SERVER_ONLY_MODULE_PATHS.length).toBeGreaterThanOrEqual(10);
    expect(SERVER_ONLY_MODULE_PATHS).toContain("lib/console-secrets.ts");
    expect(SERVER_ONLY_MODULE_PATHS).toContain("lib/auth-config.ts");
  });

  for (const moduleName of SERVER_ONLY_MODULE_PATHS) {
    test(`${moduleName} declares the @server-only marker`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, moduleName), "utf8");
      expect(source.startsWith("// @server-only")).toBe(true);
    });
  }

  test('no "use client" file imports a credential-carrying module', () => {
    const violations = sourceFiles
      .filter((file) => isClientComponent(file.source))
      .filter((file) => importsAnyOf(sourceFiles, file, SERVER_ONLY_MODULE_PATHS).length > 0)
      .map((file) => file.path);
    expect(violations).toEqual([]);
  });

  test("nothing under components/** imports a credential-carrying module", () => {
    const violations = sourceFiles
      .filter((file) => file.path.startsWith("components/"))
      .filter((file) => importsAnyOf(sourceFiles, file, SERVER_ONLY_MODULE_PATHS).length > 0)
      .map((file) => file.path);
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
    const offenders = sourceFiles
      .filter((file) => file.path !== "lib/errors.ts" && file.path !== "lib/errors-server.ts")
      .filter((file) => /\.request_id\b/.test(file.code) || /\.details\b/.test(file.code))
      .map((file) => file.path);
    // Every consumer goes through `toMoiraError` / `serverDiagnostics`. A second
    // reader is a second place the boundary can be broken.
    expect(offenders).toEqual([]);
  });

  test("nothing outside lib/errors.ts constructs a client-facing error by spreading the envelope", () => {
    const offenders = sourceFiles
      .filter((file) => file.path !== "lib/errors.ts")
      .filter((file) => /\.\.\.\s*\w*[eE]rror\.error\b/.test(file.source))
      .map((file) => file.path);
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
    const offenders = sourceFiles
      .filter(
        (file) =>
          (isClientComponent(file.source) || file.path.startsWith("components/")) &&
          driverImport.test(file.source),
      )
      .map((file) => file.path);
    expect(offenders).toEqual([]);
  });

  test("the connection string is read in exactly three places", () => {
    // `lib/env.ts` validates it and `lib/console-db.ts` consumes it — the latter
    // also exports `hasConsoleDatabase` so that "am I durable?" can be asked
    // without a third module touching the value. A further reader is a further
    // chance to log it, embed it in an error, or pass it as a prop, and it
    // carries the database password inline.
    //
    // THE THIRD IS NEW INFORMATION, NOT A RELAXATION. This test asserted "exactly
    // two" while scanning a set that did not include `db/**` at all, and
    // `db/dsn.ts:12` defines `DATABASE_URL_ENV = "CONSOLE_DATABASE_URL"`. The
    // claim was false the moment plan 09 wave 1 landed and nothing noticed,
    // because the file was outside every scan set. Widening `allSourceFiles` to
    // include `db/` is what surfaced it.
    const readers = sourceFiles
      .filter(
        (file) =>
          file.code.includes("CONSOLE_DATABASE_URL") || /\bconsoleDatabaseUrl\b/.test(file.code),
      )
      .map((file) => file.path);
    expect(readers.sort()).toEqual([...CONNECTION_STRING_READERS]);
    // Asserted as a COUNT as well as a membership: the old form asserted only
    // that the offender list was empty, which stays true if the allowlist grows.
    expect(readers.length, "exactly three modules may name the connection string").toBe(3);
  });

  test("the durable store is the only SQL against the secret table", () => {
    // One place where a client secret is read out of storage. A second query
    // would be a second decrypt path, and the AAD binding only protects the
    // path that uses it.
    const offenders = sourceFiles
      .filter((file) => file.path !== "lib/console-secrets-postgres.ts")
      .filter((file) => file.code.includes("console_provider_secret"))
      .map((file) => file.path);
    expect(offenders).toEqual([]);
  });
});

describe("no rotate-secret anywhere", () => {
  test("the literal `rotate-secret` appears in no source file", () => {
    const offenders = sourceFiles
      .filter((file) => file.source.includes("rotate-secret"))
      .map((file) => file.path);
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
