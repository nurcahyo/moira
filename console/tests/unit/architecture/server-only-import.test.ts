// The `import "server-only"` build guard, asserted as a property of the tree.
//
// This is the companion to `server-only-guards.test.ts`, and it exists because
// the two guards fail in opposite directions:
//
//   * `server-only-guards.test.ts` catches a module that is *reachable* from the
//     browser. It cannot see whether the build-time guard is present.
//   * this file catches a credential-carrying module that *lost* its build-time
//     guard — a refactor that drops the import, or a new server-only module
//     added without one. Neither shows up as a test failure anywhere else,
//     because `tests/support/server-only-shim.ts` deliberately makes the package
//     importable under `bun test`.
//
// Without this file the shim would be a hole: it makes `import "server-only"`
// harmless, so nothing would notice if the import simply went away.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const CONSOLE_ROOT = resolve(import.meta.dir, "../../..");

/**
 * Every module that holds, forwards, or closes over a Moira credential or the
 * OAuth client secret.
 *
 * Adding a module here without adding the import is a failing test; adding the
 * import without adding the module here is silently fine, which is the right way
 * round.
 */
const MUST_IMPORT_SERVER_ONLY = [
  "lib/moira-client.ts",
  "lib/setup-flow.ts",
  "lib/env.ts",
  "lib/console-secrets.ts",
  "lib/auth-config.ts",
  "lib/auth.ts",
  "lib/moira-session.ts",
  "lib/auth-runtime.ts",
] as const;

/** Matches a real side-effect import, not a mention in a comment. */
function hasServerOnlyImport(source: string): boolean {
  const codeOnly = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
  return /^\s*import\s+["']server-only["']\s*;?\s*$/m.test(codeOnly);
}

describe("the server-only build guard is present where it must be", () => {
  for (const moduleName of MUST_IMPORT_SERVER_ONLY) {
    test(`${moduleName} imports "server-only"`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, moduleName), "utf8");
      expect(hasServerOnlyImport(source)).toBe(true);
    });
  }

  test("the marker package is a real dependency, not an ambient assumption", () => {
    const pkg = JSON.parse(readFileSync(join(CONSOLE_ROOT, "package.json"), "utf8")) as {
      dependencies?: Record<string, string>;
    };
    // A devDependency would not be installed in the Docker build stage's
    // production install, and the guard would vanish from the shipped build.
    expect(pkg.dependencies?.["server-only"]).toBeString();
  });

  test("the package still resolves its `default` condition to a throwing module", () => {
    // If upstream ever makes `index.js` inert, the build guard silently stops
    // guarding and only this test would notice.
    const index = readFileSync(
      join(CONSOLE_ROOT, "node_modules/server-only/index.js"),
      "utf8",
    );
    expect(index).toContain("throw new Error");

    const manifest = JSON.parse(
      readFileSync(join(CONSOLE_ROOT, "node_modules/server-only/package.json"), "utf8"),
    ) as { exports?: Record<string, Record<string, string>> };
    expect(manifest.exports?.["."]?.["react-server"]).toBe("./empty.js");
    expect(manifest.exports?.["."]?.["default"]).toBe("./index.js");
  });
});

describe("the shim is scoped to the test runner only", () => {
  test("bunfig preloads the shim before the DOM registrar", () => {
    const bunfig = readFileSync(join(CONSOLE_ROOT, "bunfig.toml"), "utf8");
    // Read the `preload` ARRAY, not the file. The comment block above it names
    // every entry in prose, so a naive `indexOf` over the whole file measures
    // the documentation's order rather than the runtime's.
    const array = /preload\s*=\s*\[([\s\S]*?)\]/.exec(bunfig)?.[1] ?? "";
    const entries = [...array.matchAll(/"([^"]+)"/g)].map((match) => match[1] ?? "");
    const shimAt = entries.findIndex((entry) => entry.includes("server-only-shim"));
    const domAt = entries.findIndex((entry) => entry.includes("dom-register"));
    expect(shimAt).toBeGreaterThan(-1);
    expect(domAt).toBeGreaterThan(-1);
    expect(shimAt).toBeLessThan(domAt);

    // `native-globals` must also precede the registrar, or its capture is
    // happy-dom's implementation rather than Bun's.
    const nativeAt = entries.findIndex((entry) => entry.includes("native-globals"));
    expect(nativeAt).toBeGreaterThan(-1);
    expect(nativeAt).toBeLessThan(domAt);
  });

  test("nothing under lib/ or app/ mocks the marker package", () => {
    // The shim belongs to the test harness. A `mock.module("server-only")` in
    // shipped source would disarm the guard in the build too.
    for (const dir of ["lib", "app"]) {
      const files = new Bun.Glob("**/*.{ts,tsx}").scanSync({
        cwd: join(CONSOLE_ROOT, dir),
        onlyFiles: true,
      });
      for (const file of files) {
        const source = readFileSync(join(CONSOLE_ROOT, dir, file), "utf8");
        expect(source).not.toContain('mock.module("server-only"');
      }
    }
  });
});
