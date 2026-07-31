// Layer dependencies, enforced from the source rather than from a list.
//
// ============================================================================
// WHAT THIS FILE OWNS
// ============================================================================
//
//   1. `components/**` imports no module that carries a credential.
//   2. The credential set is DERIVED (`tests/support/server-only-derivation.ts`)
//      and both older guard files consume that derivation instead of their own
//      hand-maintained arrays.
//   3. `db/**` is inside the scan set and outside the application graph.
//
// Rule (1) is deliberately spelled "no credential-carrying module" and not
// "nothing from lib/". Those are different rules and the probes that produced
// this wave's brief prescribed opposite ones:
//
//   * GUARDS said `components/** -> lib/` is clean today and worth locking.
//   * I18N prescribed `Spinner.tsx`/`Label.tsx` importing `@/lib/i18n`.
//
// Both are accurate about the tree. The property that matters is CREDENTIAL
// REACHABILITY, and `lib/i18n/**` has none — it is a string table. The narrow
// rule would also have had to be repealed on the first component that renders a
// catalog string, which is this very wave.
//
// ============================================================================
// WHAT THIS FILE DOES *NOT* OWN YET, AND WHY
// ============================================================================
//
// `modules/** must not import app/**` is NOT here. `console/modules/` holds only
// `.gitkeep` and `README.md` at this commit, and `listSourceFiles` returns `[]`
// for a directory with no sources — the rule would assert `[] === []` and pass
// vacuously. `architecture.test.ts:21-23` says exactly that about itself: "on a
// clean scaffold this test passes vacuously".
//
// A rule over an empty directory is worse than no rule, because it reads as
// coverage. It lands in the same commit as the first organism.
//
// ============================================================================
// WHY THE DERIVATION IS NOT "the modules that carry the marker"
// ============================================================================
//
// See the header of `tests/support/server-only-derivation.ts`. In one line:
// deriving the set from `import "server-only"` means deleting the marker removes
// the module from the set, both consumers still agree, and every gate stays
// green while the guard evaporates.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import {
  allSourceFiles,
  CONSOLE_ROOT,
  CREDENTIAL_SEEDS,
  deriveCredentialModulePaths,
  deriveCredentialModules,
  directImports,
  hasServerOnlyImport,
  importsAnyOf,
  isClientComponent,
  resolveLocalSpecifier,
  seedEvidence,
  stripComments,
  type SourceFile,
} from "../../support/server-only-derivation";

const files = allSourceFiles();
const credentialModules = deriveCredentialModules(files);
const credentialPaths = deriveCredentialModulePaths(files);

const byRoot = (root: string) => files.filter((file) => file.path.startsWith(`${root}/`));

function synthetic(path: string, source: string): SourceFile {
  return { path, source, code: stripComments(source) };
}

/* -------------------------------------------------------------------------- */

describe("the derivation is alive", () => {
  test("the walk reached every shipped source root", () => {
    const roots = new Set(files.map((file) => file.path.split("/")[0]));
    for (const expected of ["lib", "app", "components", "db"]) {
      expect(roots, `the walk never entered ${expected}/ — every rule below is vacuous`).toContain(
        expected,
      );
    }
    expect(files.length).toBeGreaterThanOrEqual(25);
  });

  test("per-layer file-count floors", () => {
    // `listSourceFiles` returning `[]` for a missing directory is how a layering
    // rule passes while scanning nothing. Floors turn that into a failure.
    expect(byRoot("components/atoms").length, "atoms").toBeGreaterThanOrEqual(5);
    expect(byRoot("components/molecules").length, "molecules").toBeGreaterThanOrEqual(2);
    expect(byRoot("lib").length, "lib").toBeGreaterThanOrEqual(10);
    expect(byRoot("db").length, "db").toBeGreaterThanOrEqual(3);
  });

  test("every credential seed still matches something", () => {
    // A seed that matches nothing shrinks the derived set silently. Same rule as
    // the allowlist reverse check, and as `src/i18n/catalog/mod.rs:989-995`.
    const dead = CREDENTIAL_SEEDS.filter(
      (seed) => !files.some((file) => seed.pattern.test(file.code)),
    ).map((seed) => seed.label);
    expect(dead, "these credential seeds match nothing in the tree — remove them").toEqual([]);
  });

  test("POSITIVE CONTROL — a synthetic module reading process.env is seeded", () => {
    const evidence = seedEvidence(synthetic("lib/fixture.ts", "const k = process.env.SECRET;"));
    expect(evidence).toContain("reads process.env");
  });

  test("POSITIVE CONTROL — the closure reaches a module that only imports a seed", () => {
    const set = [
      synthetic("lib/fixture-seed.ts", 'import { Pool } from "pg";\nexport const p = Pool;'),
      synthetic("lib/fixture-user.ts", 'import { p } from "./fixture-seed";\nexport const q = p;'),
      synthetic("lib/fixture-clean.ts", "export const n = 1;"),
    ];
    const derived = deriveCredentialModules(set).map((entry) => entry.path);
    expect(derived).toEqual(["lib/fixture-seed.ts", "lib/fixture-user.ts"]);
  });

  test("NEGATIVE CONTROL — a type-only import of a seed does not make a module credential-carrying", () => {
    const set = [
      synthetic("lib/fixture-seed.ts", 'import { Pool } from "pg";\nexport type P = Pool;'),
      synthetic(
        "lib/fixture-type.ts",
        'import type { P } from "./fixture-seed";\nexport type Q = P;',
      ),
    ];
    const derived = deriveCredentialModules(set).map((entry) => entry.path);
    expect(derived).toEqual(["lib/fixture-seed.ts"]);
  });

  test("the derived set has the size and membership the tree warrants", () => {
    expect(credentialPaths.length, `derived: ${credentialPaths.join(", ")}`).toBeGreaterThanOrEqual(
      10,
    );

    // Named, not counted. The two that the old 4-entry array omitted are called
    // out because they are the ones that actually hold plaintext:
    // `console-secrets.ts:185` reveal(), `auth-config.ts:118` clientSecret.
    for (const path of [
      "lib/moira-client.ts",
      "lib/setup-flow.ts",
      "lib/console-db.ts",
      "lib/console-secrets-postgres.ts",
      "lib/console-secrets.ts",
      "lib/auth-config.ts",
      "lib/auth.ts",
      "lib/env.ts",
      "lib/moira-session.ts",
      "lib/auth-runtime.ts",
    ]) {
      expect(credentialPaths, `${path} is not in the derived credential set`).toContain(path);
    }
  });

  test("the client-safe modules are NOT in the derived set", () => {
    // If any of these were, `components/**` could not render a catalog string
    // and the rule below would be unsatisfiable.
    for (const path of [
      "lib/errors.ts",
      "lib/types.ts",
      "lib/moira-keys.ts",
      "lib/i18n/keys.ts",
      "lib/i18n/catalog.en.ts",
      "lib/i18n/index.ts",
    ]) {
      expect(credentialPaths, `${path} was derived as credential-carrying`).not.toContain(path);
    }
  });
});

/* -------------------------------------------------------------------------- */

describe("credential modules are marked and contained", () => {
  for (const credential of credentialModules) {
    test(`${credential.path} imports "server-only" (${credential.why}: ${credential.evidence.join(", ")})`, () => {
      const file = files.find((candidate) => candidate.path === credential.path);
      expect(hasServerOnlyImport(file!.source)).toBe(true);
    });
  }

  test("nothing under components/** imports a credential-carrying module", () => {
    const offenders = byRoot("components")
      .map((file) => ({ file: file.path, reached: importsAnyOf(files, file, credentialPaths) }))
      .filter((entry) => entry.reached.length > 0)
      .map((entry) => `${entry.file} -> ${entry.reached.join(", ")}`);
    expect(
      offenders,
      "atoms and molecules render client-side; a credential module reachable from one is a " +
        "credential in the browser bundle",
    ).toEqual([]);
  });

  test('no "use client" module imports a credential-carrying module', () => {
    const offenders = files
      .filter((file) => isClientComponent(file.source))
      .map((file) => ({ file: file.path, reached: importsAnyOf(files, file, credentialPaths) }))
      .filter((entry) => entry.reached.length > 0)
      .map((entry) => `${entry.file} -> ${entry.reached.join(", ")}`);
    expect(offenders).toEqual([]);
  });

  test("POSITIVE CONTROL — the containment rule fires on a synthetic offender", () => {
    // This rule has NEVER executed against a real client component: there are
    // zero `"use client"` directives anywhere under app/ components/ lib/
    // modules/ at this commit. Without this control it would be proving nothing.
    const offender = synthetic(
      "components/atoms/Fixture.tsx",
      '"use client";\nimport { consoleEnv } from "@/lib/env";\nexport const e = consoleEnv;',
    );
    expect(isClientComponent(offender.source)).toBe(true);
    expect(importsAnyOf([...files, offender], offender, credentialPaths)).toEqual(["lib/env.ts"]);
  });

  test("NEGATIVE CONTROL — a client component importing lib/i18n is permitted", () => {
    const allowed = synthetic(
      "components/atoms/Fixture.tsx",
      '"use client";\nimport { t } from "@/lib/i18n";\nexport const x = t;',
    );
    expect(importsAnyOf([...files, allowed], allowed, credentialPaths)).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */

describe("the two older guard files consume the derivation", () => {
  const guardSources = [
    "tests/unit/architecture/server-only-guards.test.ts",
    "tests/unit/architecture/server-only-import.test.ts",
  ];

  for (const path of guardSources) {
    test(`${path} declares no hand-maintained module array`, () => {
      // The failure this replaces: two arrays, 4 entries and 10, disagreeing on
      // six modules, with nothing asserting they agreed. Re-introducing either
      // literal list is the regression.
      const code = stripComments(readFileSync(join(CONSOLE_ROOT, path), "utf8"));
      expect(code).not.toMatch(/const\s+SERVER_ONLY_MODULES\s*=\s*\[/);
      expect(code).not.toMatch(/const\s+MUST_IMPORT_SERVER_ONLY\s*=\s*\[/);
      expect(code, `${path} must import the derivation`).toContain("server-only-derivation");
    });
  }
});

/* -------------------------------------------------------------------------- */

describe("db/** is scanned and stays outside the application graph", () => {
  test("db/** is in the scan set", () => {
    // It was in NONE before this commit: it carries `pg` and the DSN and had no
    // `import "server-only"`, so every reachability rule looked straight past it.
    expect(byRoot("db").map((file) => file.path)).toContain("db/dsn.ts");
  });

  test("nothing under lib/, app/, components/ or modules/ imports db/**", () => {
    // This is `db/`'s containment argument, and it is stronger than the marker
    // would be: the marker cannot be used there at all, because `db/migrate.ts`
    // runs under a plain `bun run` where `server-only` resolves to a bare throw.
    const dbPaths = byRoot("db").map((file) => file.path);
    const offenders = files
      .filter((file) => !file.path.startsWith("db/"))
      .map((file) => ({
        file: file.path,
        reached: directImports(files, file, { includeTypeOnly: true }).filter((target) =>
          dbPaths.includes(target),
        ),
      }))
      .filter((entry) => entry.reached.length > 0)
      .map((entry) => `${entry.file} -> ${entry.reached.join(", ")}`);
    expect(offenders).toEqual([]);
  });

  test("db/** deliberately does NOT import server-only", () => {
    // Asserted, not merely omitted. `db/dsn.ts:3-8` states the property; adding
    // the import would break `bun run db:migrate`, because `bunfig.toml`'s shim
    // preload applies to `bun test` only.
    const offenders = byRoot("db")
      .filter((file) => hasServerOnlyImport(file.source))
      .map((file) => file.path);
    expect(
      offenders,
      "these are plain `bun run` scripts; `server-only`'s default export is a bare throw",
    ).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */

describe("specifier resolution covers the forms that used to slip through", () => {
  test("`../../lib/console-db` resolves, not just `@/lib/console-db`", () => {
    // The old `importsAny` matched `@/lib/<stem>` and same-directory relatives
    // only, so a deeper relative path was invisible to it.
    expect(resolveLocalSpecifier("modules/signIn/Panel.tsx", "../../lib/console-db")).toBe(
      "lib/console-db",
    );
    expect(resolveLocalSpecifier("components/atoms/Spinner.tsx", "@/lib/env")).toBe("lib/env");
    expect(resolveLocalSpecifier("components/molecules/FormField.tsx", "../atoms/Input")).toBe(
      "components/atoms/Input",
    );
    expect(resolveLocalSpecifier("lib/errors.ts", "react")).toBeNull();
  });

  test("POSITIVE CONTROL — a deep relative import of a credential module is caught", () => {
    const offender = synthetic(
      "modules/fixture/Panel.tsx",
      'import { consoleDatabase } from "../../lib/console-db";\nexport const d = consoleDatabase;',
    );
    expect(importsAnyOf([...files, offender], offender, credentialPaths)).toEqual([
      "lib/console-db.ts",
    ]);
  });
});
