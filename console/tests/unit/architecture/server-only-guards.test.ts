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
  // Plan 09 wave 5. `ExpiryPicker` is a MOLECULE and may not import
  // `lib/invites.ts`, which is server-only — so the two invitation bounds and
  // the public invite path live here instead of being re-declared inside the
  // component, which is how a UI drifts from the rule it claims to respect.
  // Asserted client-safe rather than believed to be.
  "lib/invite-bounds.ts",
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

/**
 * Field names that match the pattern but are not credentials. Asserted live.
 *
 * `token_url` is an OAuth endpoint. `setup_token` is the reserved-and-rejected
 * field modelled only so the shape matches the committed schema.
 *
 * `maximum_input_tokens` and `maximum_output_tokens` (issue #73) are LLM token
 * BUDGETS on `RoutingPolicy*` — integer counts, nullable, with no credential
 * meaning available to them under any reading. They are exempted BY FULL NAME
 * rather than by relaxing `SECRET_DTO_FIELD_PATTERN`'s `token` alternative,
 * which is the same trade W5-D4 recorded: dropping `token` from the pattern to
 * accommodate two fields would un-guard `token_prefix`, `refresh_token` and
 * `access_token` on every DTO in the console, forever.
 *
 * `credential_id` (issue #275/#272 workstream R3) is `ClaudeRunnerRecord`'s
 * reference to the provider-credential row a runner's token became. It is a
 * bare UUID, never the token or anything derived from it — the same shape as
 * `provider_id` on the same record, which does not trip the pattern only
 * because it does not contain the substring `credential`. Exempting the FIELD
 * NAME, not the interface, follows the same reasoning `token_url` did: the
 * alternative (a fourth `EXEMPT_DTO_INTERFACES` entry) was rejected there for
 * being a cap already at three, and moving `ClaudeRunnerRecord` to a
 * server-only module the way the credential family did would make it
 * unreachable from the client components CONVENTIONS requires it for — the
 * whole point of publishing `scope` on this record is that an operator must be
 * able to tell a tenant's runner from the platform's IN THE BROWSER.
 *
 * The reverse test below asserts each entry still matches something, so an
 * exemption cannot outlive the field it was granted for.
 */
const EXEMPT_DTO_FIELDS = [
  "token_url",
  "setup_token",
  "maximum_input_tokens",
  "maximum_output_tokens",
  // Issue #237 / plan 12 §5, AND issue #275/#272 workstream R3. A row
  // REFERENCE, not a secret value, on two unrelated DTOs that happen to share
  // the field name: `SkillHttpExecutorRecord`/`SkillHttpExecutorPatchRequest`
  // never carry a `provider_credentials` row's contents, only the id of the
  // row that holds it, and `ClaudeRunnerRecord` (this section's own header
  // above) carries the same shape for the credential a runner's token became.
  // Neither is a credential itself, matched by name against
  // `SECRET_DTO_FIELD_PATTERN`'s `credential` alternative anyway. Same trade
  // as the two above: `lib/llm-view.ts`'s `LlmKeyRowView` already treats the
  // LLM surface's own credential rows the same way.
  "credential_id",
  // Plan 12 §6. An integer token BUDGET on `AgentProfileRecord`, the same shape
  // as `maximum_input_tokens`/`maximum_output_tokens` above — matched by the
  // pattern's `token` alternative with no credential meaning under any reading.
  "max_tokens",
  // Issue #261 (the playground). `max_output_tokens` (`PublicResponseRequest`)
  // is the same integer token BUDGET as `max_tokens`/`maximum_output_tokens`
  // above, just spelled differently on this schema. `cached_input_tokens`,
  // `input_tokens`, `output_tokens`, `reasoning_tokens` and `total_tokens`
  // (`PublicUsageSummary` and the distinct-but-field-identical `UsageSummary`
  // — see that interface's own doc comment on why there are two) are all
  // integer USAGE COUNTS a provider reports back, not credentials — the same
  // trade as every entry above.
  "max_output_tokens",
  "cached_input_tokens",
  "input_tokens",
  "output_tokens",
  "reasoning_tokens",
  "total_tokens",
] as const;

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

/**
 * Matches the `Authorization` HTTP header by name, in the three syntactic
 * shapes CODE that builds or reads it actually takes:
 *
 *   1. an UNQUOTED object key — `{ Authorization: value }`;
 *   2. a PROPERTY ACCESS — `headers.Authorization`;
 *   3. a QUOTED word, used exactly alone — `"Authorization"`, `'Authorization'`,
 *      `` `Authorization` `` — which covers every remaining call shape a header
 *      is set or read through: `headers["Authorization"]`,
 *      `headers.set("Authorization", …)`, `headers.get("Authorization")`, a
 *      quoted object key, and so on, because all of them quote the word with
 *      nothing else inside the quotes.
 *
 * ============================================================================
 * TWO DIFFERENT FALSE POSITIVES, TWO DIFFERENT REASONS NEITHER SIMPLER PATTERN
 * WORKS
 * ============================================================================
 *
 * A bare substring check (`.not.toContain("Authorization")`, this guard's
 * original form) flags any IDENTIFIER that merely contains the word —
 * `lib/types.ts`'s `ClaudeRunnerAuthorizationCodeRequest` (issue #275/#272
 * workstream R3, an OAuth authorization-CODE type name, not a header).
 *
 * A naive WORD-BOUNDARY check (`/(?<![\w$])Authorization(?![\w$])/`, this
 * guard's own first attempted fix) repairs that, but breaks a DIFFERENT case
 * the same wave introduced: `lib/i18n/catalog.en.ts`'s operator-facing copy
 * "Authorization URL" and "Authorization code" (the labels on the runner
 * detail page's authorization-URL field and code-paste field) — plain English
 * prose that happens to start with the same word, which a whole-word check
 * cannot tell apart from `{ Authorization: token }`.
 *
 * What actually distinguishes header-CODE from either false positive: header
 * code always quotes "Authorization" ALONE (nothing else inside the quotes) or
 * uses it as a bare property/key, and is never followed by a plain-English
 * continuation word. "Authorization URL" fails alternative 3 because the
 * quoted string is `"Authorization URL"`, not `"Authorization"` — there is no
 * quote character immediately after the word itself, only after "URL". The
 * three alternatives above are what encode that distinction; a single regex
 * covering all three could not read shorter without losing one of them.
 *
 * Exported as its own named pattern, not inlined into the assertion below, so
 * the positive- and negative-control tests in the next `describe` block
 * exercise the REAL pattern rather than a hand-copied stand-in that could
 * silently drift from it — which is exactly how this guard's PREVIOUS repair
 * shipped broken and untested.
 */
const AUTHORIZATION_HEADER_PATTERN = new RegExp(
  [
    // 1. Unquoted object key: `Authorization:` (optional surrounding
    //    whitespace before the colon), not preceded or followed by another
    //    identifier character — excludes `ClaudeRunnerAuthorizationCodeRequest`.
    String.raw`(?<![A-Za-z0-9_$])Authorization(?![A-Za-z0-9_$])\s*:`,
    // 2. Property access: `headers.Authorization`.
    String.raw`\.\s*Authorization(?![A-Za-z0-9_$])`,
    // 3. The word alone, quoted — nothing else between the quotes. Matches
    //    `"Authorization"` wherever it appears (bracket access, a `.get(...)`/
    //    `.set(...)` argument, a quoted object key) and NEVER matches
    //    `"Authorization URL"` or `"Authorization code"`, because the character
    //    immediately after the word is a space, not the closing quote.
    String.raw`(["'\`])Authorization\1`,
  ].join("|"),
);

describe("the client-safe modules really are client-safe", () => {
  for (const moduleName of CLIENT_SAFE_MODULES) {
    test(`${moduleName} names no credential header and reads no credential`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, moduleName), "utf8");
      // A credential-shaped literal in a module the browser may load is the
      // whole failure mode; the doc comment in moira-client.ts is allowed to
      // mention the header name because that module is server-only.
      const codeOnly = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
      expect(codeOnly).not.toContain("X-Moira-System-Key");
      // The `Authorization` HEADER — see `AUTHORIZATION_HEADER_PATTERN`'s own
      // doc comment above for what it matches, what it deliberately does not,
      // and the two different false positives (one identifier, one i18n copy)
      // that ruled out simpler patterns. Pinned by the positive AND negative
      // controls in the `describe` block below: a guard nobody tests is a
      // guard that can quietly stop guarding, which is exactly how this one's
      // first repair shipped.
      expect(codeOnly).not.toMatch(AUTHORIZATION_HEADER_PATTERN);
      expect(codeOnly).not.toMatch(/process\.env/);
    });
  }

  describe("AUTHORIZATION_HEADER_PATTERN itself", () => {
    test("POSITIVE CONTROL — an unquoted object key is caught", () => {
      // The exact defect a prior revision of this guard had: a quoted-string
      // pattern (`/(["'\`])Authorization\1/`) does not match this at all, so a
      // client-safe module could have built the header this way and the suite
      // would have stayed green.
      expect("fetch(url, { headers: { Authorization: `Bearer ${token}` } })").toMatch(
        AUTHORIZATION_HEADER_PATTERN,
      );
    });

    test("POSITIVE CONTROL — property access is caught", () => {
      expect("headers.Authorization = token;").toMatch(AUTHORIZATION_HEADER_PATTERN);
    });

    test("POSITIVE CONTROL — every quoted spelling is still caught", () => {
      for (const quoted of ['"Authorization"', "'Authorization'", "`Authorization`"]) {
        expect(quoted).toMatch(AUTHORIZATION_HEADER_PATTERN);
      }
    });

    test("POSITIVE CONTROL — bracket access and a Headers-API call are caught", () => {
      // Both are covered by alternative 3 (the word quoted alone) rather than
      // needing their own alternative — see the pattern's own doc comment.
      expect('headers["Authorization"] = value;').toMatch(AUTHORIZATION_HEADER_PATTERN);
      expect('headers.get("Authorization")').toMatch(AUTHORIZATION_HEADER_PATTERN);
      expect('headers.set("Authorization", value)').toMatch(AUTHORIZATION_HEADER_PATTERN);
    });

    test("NEGATIVE CONTROL — an identifier that merely contains the word is not caught", () => {
      // The false positive this guard exists to fix — issue #275/#272 R3's
      // `ClaudeRunnerAuthorizationCodeRequest`, an OAuth authorization CODE
      // type name, not a header.
      expect("export interface ClaudeRunnerAuthorizationCodeRequest {").not.toMatch(
        AUTHORIZATION_HEADER_PATTERN,
      );
      expect('schema: "ClaudeRunnerAuthorizationCodeRequest",').not.toMatch(
        AUTHORIZATION_HEADER_PATTERN,
      );
    });

    test("NEGATIVE CONTROL — the case-sensitive, unrelated authorization_url field is not caught", () => {
      expect("authorization_url?: string | null;").not.toMatch(AUTHORIZATION_HEADER_PATTERN);
    });

    test("NEGATIVE CONTROL — operator-facing i18n copy that starts with the word is not caught", () => {
      // The SECOND false positive, found only after fixing the first: a naive
      // whole-word check (this guard's own first attempted repair) flags these
      // too. `lib/i18n/catalog.en.ts`'s real entries, verbatim — the labels on
      // the runner detail page's authorization-URL field and code-paste field
      // (issue #275/#272 workstream R3), plain English prose that happens to
      // start with the header's name.
      expect('message: "Authorization URL",').not.toMatch(AUTHORIZATION_HEADER_PATTERN);
      expect('message: "Authorization code",').not.toMatch(AUTHORIZATION_HEADER_PATTERN);
    });
  });
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

/* -------------------------------------------------------------------------- */
/* The credential DTOs (issue #73)                                            */
/* -------------------------------------------------------------------------- */
//
// WHAT HAPPENED AT THE CAP. Issue #73 needed four more secret-shaped DTOs
// (`CredentialCreateRequest.secret`, `CredentialRecord.masked_secret` and
// `.secret_fingerprint`, `RotateCredentialRequest.secret`,
// `ApiKeyCredentialSecret.api_key`) and `EXEMPT_DTO_INTERFACES` was full. The
// exemption was NOT taken — see the reversal condition above. The family moved
// to `lib/moira-credential-types.ts`, which carries `import "server-only"`.
//
// That is strictly stronger than a fourth carve-out would have been. An
// exemption permits a raw secret to be MODELLED in `lib/types.ts`, which is
// asserted CLIENT-SAFE and which a `"use client"` component may import; moving
// the family makes the shapes unnameable from the browser and puts the file in
// `containedModulePaths()`, where the two reachability rules at the top of this
// file already cover it with no new list.
//
// The rules below are what stops the move from being a way AROUND the guard
// rather than a way to satisfy it: the same secret-shaped-member scan runs on
// the new module, with its interfaces pinned member-for-member, and `lib/types.ts`
// is asserted to have kept none of them.

const CREDENTIAL_DTO_MODULE = "lib/moira-credential-types.ts";

/**
 * The interfaces in the credential module permitted to model a secret-shaped
 * field, each with its member set PINNED — the same device as
 * `EXEMPT_DTO_INTERFACES`, applied to the module the family moved to.
 *
 * There is no cap here and there does not need to be one: this module is
 * server-only and contained, so the question it answers is "did a field ride in
 * on a shape that was already allowed", not "is a secret reachable from the
 * browser". The pin is what makes the first question answerable.
 */
const CREDENTIAL_DTO_INTERFACES: ReadonlyArray<{
  readonly name: string;
  readonly members: readonly string[];
}> = [
  { name: "ApiKeyCredentialSecret", members: ["api_key"] },
  { name: "AzureCredentialSecret", members: ["api_key", "endpoint"] },
  {
    name: "OAuth2CredentialSecret",
    members: ["access_token", "expires_at", "refresh_token", "token_type"],
  },
  {
    name: "CredentialCreateRequest",
    members: [
      "credential_type",
      "display_name",
      "expires_at",
      "metadata",
      "priority",
      "provider_id",
      "scope",
      "secret",
    ],
  },
  {
    name: "CredentialRecord",
    members: [
      "created_at",
      "credential_type",
      "deleted_at",
      "display_name",
      "expires_at",
      "id",
      "last_used_at",
      "last_validated_at",
      "masked_secret",
      "metadata",
      "priority",
      "provider_id",
      "scope",
      "secret_fingerprint",
      "status",
      "updated_at",
      "version",
    ],
  },
  { name: "RotateCredentialRequest", members: ["secret"] },
  // The narrowed console-only body. It is a `type … = Omit<…> & { … }`, and the
  // parser reads the intersection's object literal — which is exactly the two
  // fields the narrowing exists to pin: `credential_type` to the literal
  // `"api_key"`, and `secret` to the single-key newtype.
  { name: "ConsoleApiKeyCredentialCreateRequest", members: ["credential_type", "secret"] },
  // Same narrowing, for the oauth2 arm the Claude-subscription flow needs.
  { name: "ConsoleOAuth2CredentialCreateRequest", members: ["credential_type", "secret"] },
];

describe("the credential DTOs are contained rather than exempted", () => {
  test("the module declares the marker and is in the contained set", () => {
    const source = readFileSync(join(CONSOLE_ROOT, CREDENTIAL_DTO_MODULE), "utf8");
    expect(source.startsWith("// @server-only")).toBe(true);
    expect(
      CONTAINED_MODULE_PATHS,
      "without the marker the module is browser-reachable and the whole reason for the move " +
        "evaporates, while every other test here stays green",
    ).toContain(CREDENTIAL_DTO_MODULE);
  });

  test("it is NOT on the client-safe list", () => {
    // The list it must never join. `CLIENT_SAFE_MODULES` is what says "a
    // component may import this", and a credential DTO on it is the exact
    // arrangement the move exists to prevent.
    expect(CLIENT_SAFE_MODULES as readonly string[]).not.toContain(CREDENTIAL_DTO_MODULE);
  });

  test("lib/types.ts kept none of the credential DTOs", () => {
    // The other half of the move. Without this, a later edit could copy
    // `CredentialRecord` back into the client-safe module and only the
    // secret-field scan would catch it — and only for as long as nobody reached
    // for a fourth exemption.
    const source = readFileSync(join(CONSOLE_ROOT, "lib/types.ts"), "utf8");
    const names = extractInterfaces("lib/types.ts", source, "[A-Za-z0-9_]+").map(
      (declared) => declared.name,
    );
    for (const declared of CREDENTIAL_DTO_INTERFACES) {
      expect(names, `${declared.name} belongs in ${CREDENTIAL_DTO_MODULE}`).not.toContain(
        declared.name,
      );
    }
  });

  test("the parser reached the credential module's interfaces at all", () => {
    // A parser that found nothing would make every pin below vacuous.
    const source = readFileSync(join(CONSOLE_ROOT, CREDENTIAL_DTO_MODULE), "utf8");
    const interfaces = extractInterfaces(CREDENTIAL_DTO_MODULE, source, "[A-Za-z0-9_]+");
    expect(
      interfaces.filter((declared) => declared.members.length > 0).length,
    ).toBeGreaterThanOrEqual(5);
  });

  test("every secret-shaped interface in the module is pinned", () => {
    // Same rule as `lib/types.ts`, run on the module the family moved to: an
    // interface here that declares a secret-shaped member must be one this list
    // names, with its members pinned. A new `CredentialRecord.raw_secret` fails
    // the pin; a whole new secret-bearing DTO fails this.
    const source = readFileSync(join(CONSOLE_ROOT, CREDENTIAL_DTO_MODULE), "utf8");
    const pinned = CREDENTIAL_DTO_INTERFACES.map((entry) => entry.name);
    const unpinned = extractInterfaces(CREDENTIAL_DTO_MODULE, source, "[A-Za-z0-9_]+")
      .filter((declared) => !pinned.includes(declared.name))
      .flatMap((declared) =>
        declared.members
          .filter((member) => SECRET_DTO_FIELD_PATTERN.test(member))
          .map((member) => `${declared.name}.${member}`),
      );
    expect(
      unpinned,
      "a secret-shaped field on an unpinned interface — add the interface to " +
        "CREDENTIAL_DTO_INTERFACES with its full member set, or do not model the field",
    ).toEqual([]);
  });

  for (const declared of CREDENTIAL_DTO_INTERFACES) {
    test(`${declared.name} is still NEEDED and its members are pinned`, () => {
      const source = readFileSync(join(CONSOLE_ROOT, CREDENTIAL_DTO_MODULE), "utf8");
      const found = extractInterfaces(CREDENTIAL_DTO_MODULE, source, "[A-Za-z0-9_]+").find(
        (candidate) => candidate.name === declared.name,
      );
      expect(found, `${declared.name} is pinned but was not found`).toBeDefined();
      expect([...found!.members].sort()).toEqual([...declared.members].sort());
      expect(
        found!.members.some((member) => SECRET_DTO_FIELD_PATTERN.test(member)),
        `${declared.name} declares no secret-shaped field — the pin carves out nothing`,
      ).toBe(true);
    });
  }

  test("the module holds no renderer and no logging", () => {
    // What it must never grow. `masked_secret` is Moira's own redaction and is
    // the only value here an operator may be shown; it must reach the browser
    // because a route handler CHOSE to forward it, never because a component
    // could import this file and reach for a helper that formats it.
    const source = readFileSync(join(CONSOLE_ROOT, CREDENTIAL_DTO_MODULE), "utf8");
    const codeOnly = source.replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, "");
    expect(codeOnly).not.toMatch(/console\.(log|info|warn|error|debug)/);
    expect(codeOnly).not.toContain("react");
    expect(codeOnly).not.toMatch(/process\.env/);
  });
});
