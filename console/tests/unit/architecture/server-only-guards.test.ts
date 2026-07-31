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

import { extractInterfaces } from "../../support/secret-props-scan";
import {
  allSourceFiles,
  containedModulePaths,
  deriveCredentialModulePaths,
  importsAnyOf,
  isClientComponent,
} from "../../support/server-only-derivation";

const CONSOLE_ROOT = resolve(import.meta.dir, "../../..");

/** Derived, never listed. What MUST carry the marker. */
const SERVER_ONLY_MODULE_PATHS = deriveCredentialModulePaths();

/** What must not be reachable from the client: derived UNION declared marker. */
const CONTAINED_MODULE_PATHS = containedModulePaths();

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

/**
 * Field names on a Moira DTO that would mean the console models a shape carrying
 * a credential. Same vocabulary as `tests/support/secret-props-scan.ts`.
 */
const SECRET_DTO_FIELD_PATTERN =
  /(secret|masked|fingerprint|token|password|api_?key|private_?key|credential)/i;

/**
 * The interfaces permitted to model a raw secret, each with its member set PINNED.
 *
 * ============================================================================
 * WHY THE PINNING IS THE WHOLE MECHANISM (plan 09 wave 5, decision W5-D4)
 * ============================================================================
 *
 * `SECRET_DTO_FIELD_PATTERN` matches `token`, and the redemption path needs two
 * DTOs whose only field of interest IS a token: `AdminInvitePreviewRequest` and
 * `AdminInviteRedeemRequest`. There were two ways to land them and only one is
 * honest.
 *
 *   REJECTED — drop `token` from the pattern. That un-guards `token_prefix`,
 *   `refresh_token`, `access_token` and every future DTO field with `token` in
 *   its name, across the whole file, forever, in exchange for two interfaces.
 *
 *   TAKEN — exempt the two interfaces BY NAME with their member sets pinned
 *   exactly, so a later field cannot ride in on the exemption. Adding
 *   `client_secret` to `AdminInviteRedeemRequest` fails the pin even though the
 *   interface is exempt.
 *
 * Every exemption is checked in REVERSE as well ("still NEEDED"), so an
 * exemption that carves out nothing is itself a failure.
 *
 * AT THREE ENTRIES, STOP. The reversal condition recorded with W5-D4: if a third
 * DTO needs a `token` field, introduce a typed `InviteToken` newtype whose name
 * does not match the pattern instead — at three exemptions the list has become
 * the thing it was guarding against.
 */
const EXEMPT_DTO_INTERFACES: ReadonlyArray<{
  readonly name: string;
  /** Sorted member set. A new field on an exempt interface fails here. */
  readonly members: readonly string[];
}> = [
  {
    name: "AdminInviteSecretResponse",
    members: ["notice", "resource", "secret", "secret_retrievable"],
  },
  { name: "AdminInvitePreviewRequest", members: ["token"] },
  { name: "AdminInviteRedeemRequest", members: ["email", "email_verified", "token"] },
];

const EXEMPT_DTO_INTERFACE_NAMES = EXEMPT_DTO_INTERFACES.map((entry) => entry.name);

/** Field names that match the pattern but are not credentials. Asserted live. */
const EXEMPT_DTO_FIELDS = ["token_url", "setup_token"] as const;

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
      .filter((file) => importsAnyOf(sourceFiles, file, CONTAINED_MODULE_PATHS).length > 0)
      .map((file) => file.path);
    expect(violations).toEqual([]);
  });

  test("nothing under components/** imports a credential-carrying module", () => {
    const violations = sourceFiles
      .filter((file) => file.path.startsWith("components/"))
      .filter((file) => importsAnyOf(sourceFiles, file, CONTAINED_MODULE_PATHS).length > 0)
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
  test("request_id and details are read only inside lib/errors-server.ts", () => {
    // ONE module, and it is the SERVER-ONLY one (plan 09 wave 3). `lib/errors.ts`
    // used to be exempt too, because `serverDiagnostics()` lived there — and that
    // exemption was what let the unfiltered `details` pass-through sit in a
    // module a client component may import. It is gone, and its absence is now
    // load-bearing: moving that function back into the client-safe module fails
    // this test.
    const offenders = sourceFiles
      .filter((file) => file.path !== "lib/errors-server.ts")
      .filter((file) => /\.request_id\b/.test(file.code) || /\.details\b/.test(file.code))
      .map((file) => file.path);
    expect(offenders).toEqual([]);
  });

  test("lib/errors.ts genuinely no longer reads either field", () => {
    // Asserted directly as well as by exclusion above: the rule reads better as
    // "one module may see these", and this is the half that says WHICH module
    // stopped being able to.
    const clientSafe = sourceFiles.find((file) => file.path === "lib/errors.ts");
    expect(clientSafe).toBeDefined();
    expect(/\.request_id\b/.test(clientSafe!.code)).toBe(false);
    expect(/\.details\b/.test(clientSafe!.code)).toBe(false);
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
    // THE VOCABULARY IS WIDER THAN IT WAS, because the narrow version did not
    // catch its own mutation test. `(secret|masked|fingerprint)` let
    // `AdminInviteRecord.token_prefix` through — a field name that would be a
    // real leak on a record DTO, and one that CONVENTIONS §6 rule 5 already
    // names. It now matches the same vocabulary as
    // `tests/support/secret-props-scan.ts`.
    //
    // ONE INTERFACE IS EXEMPT, AND IT IS NAMED.
    // `AdminInviteSecretResponse` is the once-only envelope: Moira genuinely
    // returns a raw invitation token, exactly once, at creation. Modelling it is
    // not a relaxation of the D7 rule — D7 removed the OAuth CLIENT SECRET from
    // Moira, and `rotate-secret` still does not exist (asserted above). Refusing
    // to model the field would mean typing the modal against
    // `ApiKeySecretResponse`, which compiles and silently drops both the token
    // and the required `notice`. The exemption is per-INTERFACE, so a `secret`
    // on any other DTO still fails.
    //
    // TWO FIELD NAMES ARE EXEMPT, AND BOTH ARE ASSERTED TO STILL EXIST below.
    // `token_url` is an OAuth endpoint, not a token. `setup_token` is the
    // reserved-and-rejected field modelled solely so the shape matches the
    // committed schema; `assertClaimRequestIsSafe` refuses to send it.
    //
    // TWO MORE INTERFACES ARE EXEMPT from plan 09 wave 5 — the redeem/preview
    // request bodies, whose member sets are pinned. See EXEMPT_DTO_INTERFACES.
    const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
    const interfaces = extractInterfaces("lib/types.ts", source, "[A-Za-z0-9_]+");

    const offenders = interfaces
      .filter((declared) => !EXEMPT_DTO_INTERFACE_NAMES.includes(declared.name))
      .flatMap((declared) =>
        declared.members
          .filter((member) => SECRET_DTO_FIELD_PATTERN.test(member))
          .filter((member) => !(EXEMPT_DTO_FIELDS as readonly string[]).includes(member))
          .map((member) => `${declared.name}.${member}`),
      );
    expect(offenders).toEqual([]);
  });

  test("every DTO field exemption is still NEEDED", () => {
    // Reverse direction. An exemption for a field that no longer exists is a
    // carve-out waiting to cover the next field that takes the name.
    const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
    const members = new Set(
      extractInterfaces("lib/types.ts", source, "[A-Za-z0-9_]+").flatMap(
        (declared) => declared.members,
      ),
    );
    const dead = EXEMPT_DTO_FIELDS.filter((field) => !members.has(field));
    expect(dead, "these field exemptions match nothing in lib/types.ts — remove them").toEqual([]);
  });

  test("the DTO pattern covers the CONVENTIONS §6 rule 5 vocabulary", () => {
    for (const name of ["secret", "token_prefix", "password", "api_key", "private_key"]) {
      expect(SECRET_DTO_FIELD_PATTERN.test(name), `${name} is not matched`).toBe(true);
    }
    for (const name of ["value", "version", "expires_at", "constraint", "status"]) {
      expect(SECRET_DTO_FIELD_PATTERN.test(name), `${name} is falsely matched`).toBe(false);
    }
  });

  test("the parser reached lib/types.ts's interfaces at all", () => {
    // The exemption above is only meaningful if the interfaces parsed. A parser
    // that found nothing would exempt everything.
    const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
    const interfaces = extractInterfaces("lib/types.ts", source, "[A-Za-z0-9_]+");
    expect(interfaces.length, "no interfaces parsed out of lib/types.ts").toBeGreaterThanOrEqual(
      15,
    );
    const withMembers = interfaces.filter((declared) => declared.members.length > 0);
    expect(withMembers.length).toBeGreaterThanOrEqual(15);
  });

  for (const exemption of EXEMPT_DTO_INTERFACES) {
    test(`the ${exemption.name} exemption is still NEEDED, and its members are pinned`, () => {
      // Two directions in one test, because they fail for different reasons and
      // both messages matter.
      //
      //   NEEDED   an exemption whose interface no longer declares a
      //            secret-shaped field is a stale carve-out that will silently
      //            cover the next interface to take that name.
      //   PINNED   an exemption is per-INTERFACE, so without the pin a later
      //            field on an exempt interface inherits the carve-out. This is
      //            what stops `AdminInviteRedeemRequest` growing a
      //            `client_secret` under cover of its `token`.
      const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
      const declared = extractInterfaces("lib/types.ts", source, "[A-Za-z0-9_]+").find(
        (candidate) => candidate.name === exemption.name,
      );
      expect(declared, `${exemption.name} is exempt but was not found`).toBeDefined();
      expect([...declared!.members].sort()).toEqual([...exemption.members].sort());
      expect(
        declared!.members.some((member) => SECRET_DTO_FIELD_PATTERN.test(member)),
        `${exemption.name} declares no secret-shaped field — the exemption carves out nothing`,
      ).toBe(true);
    });
  }

  test("the exemption list has not grown past the point where it IS the hazard", () => {
    // W5-D4's reversal condition, asserted rather than left in prose. Three is
    // the recorded ceiling: at four, the fix is a typed `InviteToken` newtype
    // whose name does not match the pattern, not a fourth carve-out.
    expect(
      EXEMPT_DTO_INTERFACES.length,
      "a fourth exempt interface means the allow-list has become the thing it was guarding " +
        "against — introduce a newtype instead (plan 09 §0.8.3 W5-D4)",
    ).toBeLessThanOrEqual(3);
  });
});
