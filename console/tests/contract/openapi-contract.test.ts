// Contract tests against `docs/openapi.json` — the committed, frozen spec.
//
// The console's DTOs and its operation registry are hand-written. That is a
// deliberate choice (there is no generator wired up yet), and the cost of it is
// silent drift: a Moira-side schema change lands, the console keeps compiling,
// and the first sign of trouble is a 400 in front of an operator mid-setup.
//
// These tests remove that failure mode. Every `*_CONTRACT` descriptor in
// `lib/types.ts` and every entry in `MOIRA_OPERATIONS` is re-derived from the
// committed spec on each run. Neither plan 08's body nor its §0 audit is
// consulted — the spec file is the only authority.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { CREDENTIAL_SCHEMA_CONTRACTS } from "@/lib/moira-credential-types";
import {
  AUTH_PROVIDER_OPERATION_NAMES,
  LLM_CONFIG_OPERATION_NAMES,
  MOIRA_OPERATIONS,
  type MoiraOperation,
  type MoiraOperationName,
} from "@/lib/moira-client";
import { SCHEMA_CONTRACTS, type SchemaContract } from "@/lib/types";

/**
 * Every descriptor the console declares, from BOTH modules that declare any.
 *
 * The credential DTOs are in `lib/moira-credential-types.ts` rather than
 * `lib/types.ts` because they model a raw secret and `lib/types.ts` is asserted
 * client-safe — see that module's header. Splitting them must not create a
 * second way to fall out of this gate, so the completeness scan below runs over
 * both files and the shape check runs over the concatenation.
 */
const ALL_SCHEMA_CONTRACTS: readonly SchemaContract[] = [
  ...SCHEMA_CONTRACTS,
  ...CREDENTIAL_SCHEMA_CONTRACTS,
];

const SPEC_PATH = resolve(import.meta.dir, "../../../docs/openapi.json");

interface OpenApiSchema {
  properties?: Record<string, unknown>;
  required?: string[];
}

interface OpenApiParameter {
  name: string;
  in: string;
  required?: boolean;
}

interface OpenApiOperation {
  operationId?: string;
  parameters?: OpenApiParameter[];
  security?: Record<string, string[]>[];
  responses?: Record<string, unknown>;
}

interface OpenApiDocument {
  paths: Record<string, Record<string, OpenApiOperation>>;
  components: { schemas: Record<string, OpenApiSchema> };
}

const spec = JSON.parse(readFileSync(SPEC_PATH, "utf8")) as OpenApiDocument;

function schemaOf(name: string): OpenApiSchema {
  const schema = spec.components.schemas[name];
  if (schema === undefined) {
    throw new Error(`#/components/schemas/${name} is not in docs/openapi.json`);
  }
  return schema;
}

function operationOf(operation: MoiraOperation): OpenApiOperation {
  const item = spec.paths[operation.path];
  if (item === undefined) {
    throw new Error(`${operation.path} is not a path in docs/openapi.json`);
  }
  const method = operation.method.toLowerCase();
  const declared = item[method];
  if (declared === undefined) {
    throw new Error(`${operation.method} ${operation.path} is not declared in docs/openapi.json`);
  }
  return declared;
}

function parameterNamed(declared: OpenApiOperation, name: string): OpenApiParameter | undefined {
  return (declared.parameters ?? []).find((parameter) => parameter.name === name);
}

function securitySchemes(declared: OpenApiOperation): string[] | null {
  if (declared.security === undefined) return null;
  return declared.security.flatMap((entry) => Object.keys(entry)).sort();
}

const sorted = (values: readonly string[]): string[] => [...values].sort();

/* -------------------------------------------------------------------------- */

// `SCHEMA_CONTRACTS` and `CREDENTIAL_SCHEMA_CONTRACTS` are hand-maintained
// arrays. A `*_CONTRACT` declared in one of these modules but left out of its
// array is checked by NOTHING: the interface still type-checks against its own
// descriptor via `assertKeyContract`, so the omission is invisible, and the claim
// this whole file rests on — "every descriptor is re-derived from the committed
// spec" — silently stops holding for that DTO.
//
// The probe that produced the original brief made exactly that claim about a
// newly added DTO ("gated automatically once added"). It is true only once the
// descriptor reaches the array.
//
// RUN OVER BOTH MODULES, not one. Issue #73 moved the credential DTOs into
// `lib/moira-credential-types.ts` (they model a raw secret; `lib/types.ts` is
// asserted client-safe), and a scan that still read one file would have declared
// the split complete while three descriptors sat in no gate at all.
const CONTRACT_MODULES: ReadonlyArray<{
  readonly path: string;
  readonly array: string;
  readonly runtime: readonly SchemaContract[];
  readonly floor: number;
}> = [
  { path: "../../lib/types.ts", array: "SCHEMA_CONTRACTS", runtime: SCHEMA_CONTRACTS, floor: 13 },
  {
    path: "../../lib/moira-credential-types.ts",
    array: "CREDENTIAL_SCHEMA_CONTRACTS",
    runtime: CREDENTIAL_SCHEMA_CONTRACTS,
    floor: 3,
  },
];

for (const contractModule of CONTRACT_MODULES) {
  describe(`${contractModule.array} is complete`, () => {
    const source = readFileSync(resolve(import.meta.dir, contractModule.path), "utf8");
    const declared = [...source.matchAll(/^export const ([A-Z0-9_]+_CONTRACT)\b/gm)]
      .map((match) => match[1] ?? "")
      .sort();
    const registered = [...source.matchAll(/^ {2}([A-Z0-9_]+_CONTRACT),$/gm)]
      .map((match) => match[1] ?? "")
      .sort();

    test("the scanner found the declarations at all", () => {
      // Zero declarations found means the regex stopped matching, at which point
      // "every declaration is registered" is vacuously true.
      expect(declared.length, `found: ${declared.join(", ")}`).toBeGreaterThanOrEqual(contractModule.floor);
    });

    test(`every declared *_CONTRACT appears in ${contractModule.array}`, () => {
      const missing = declared.filter((name) => !registered.includes(name));
      expect(
        missing,
        `these descriptors exist in ${contractModule.path} but are in no gate — add them to ${contractModule.array}`,
      ).toEqual([]);
    });

    test(`${contractModule.array} names nothing that is not declared`, () => {
      const phantom = registered.filter((name) => !declared.includes(name));
      expect(phantom).toEqual([]);
    });

    test("the array's length matches both the source scan and the runtime value", () => {
      expect(registered.length).toBe(declared.length);
      expect(contractModule.runtime.length).toBe(declared.length);
    });
  });
}

describe("the two descriptor arrays do not overlap", () => {
  test("no two descriptors name the same schema, across both modules", () => {
    // Across BOTH arrays: a schema described twice means two interfaces claiming
    // one wire shape, and only one of them can be right.
    const schemas = ALL_SCHEMA_CONTRACTS.map((contract) => contract.schema);
    expect(new Set(schemas).size).toBe(schemas.length);
  });

  test("the credential descriptors really are the ones with a secret-shaped field", () => {
    // The split is only worth its cost if it lands the right DTOs. Asserted so a
    // later edit cannot drift a credential DTO back into the client-safe module
    // while this file keeps reporting green.
    expect(CREDENTIAL_SCHEMA_CONTRACTS.map((contract) => contract.schema).sort()).toEqual([
      "CredentialCreateRequest",
      "CredentialRecord",
      "RotateCredentialRequest",
    ]);
    const secretShaped = /(secret|masked|fingerprint|api_?key|password)/i;
    // The one pre-existing carve-out, and it is the SAME one
    // `server-only-guards.test.ts` grants by name under decision W5-D4: the
    // once-only invitation envelope, which genuinely returns a raw token exactly
    // once. Named here rather than pattern-matched, so a second schema cannot
    // acquire the exemption by resembling it.
    const alreadyExempt = ["AdminInviteSecretResponse"];
    for (const contract of SCHEMA_CONTRACTS) {
      if (alreadyExempt.includes(contract.schema)) continue;
      const fields = [...contract.required, ...contract.optional].filter((field) =>
        secretShaped.test(field),
      );
      expect(
        fields,
        `${contract.schema} is described in the CLIENT-SAFE lib/types.ts but names a ` +
          "secret-shaped field — it belongs in lib/moira-credential-types.ts",
      ).toEqual([]);
    }
  });
});

describe("DTO shapes match docs/openapi.json", () => {
  for (const contract of ALL_SCHEMA_CONTRACTS satisfies readonly SchemaContract[]) {
    test(`${contract.schema} required and optional fields match the committed schema`, () => {
      const schema = schemaOf(contract.schema);
      const specProperties = sorted(Object.keys(schema.properties ?? {}));
      const specRequired = sorted(schema.required ?? []);

      expect(sorted(contract.required)).toEqual(specRequired);
      expect(sorted([...contract.required, ...contract.optional])).toEqual(specProperties);
    });
  }
});

describe("ClaimAdminIdentityRequest — the fields §0 says the plan gets wrong", () => {
  const schema = schemaOf("ClaimAdminIdentityRequest");

  test("email is a required, non-nullable string", () => {
    expect(schema.required).toContain("email");
    expect(schema.properties?.["email"]).toMatchObject({ type: "string" });
  });

  test("email_verified is a required boolean with no default", () => {
    expect(schema.required).toContain("email_verified");
    expect(schema.properties?.["email_verified"]).toMatchObject({ type: "boolean" });
    expect(JSON.stringify(schema.properties?.["email_verified"])).not.toContain('"default"');
  });

  test("scopes is optional — omitted and [] are different requests", () => {
    expect(schema.required).not.toContain("scopes");
    expect(schema.properties).toHaveProperty("scopes");
  });

  test("setup_token exists, is nullable, and is not required", () => {
    expect(schema.required).not.toContain("setup_token");
    expect(schema.properties?.["setup_token"]).toMatchObject({ type: ["string", "null"] });
  });

  test("unknown fields are rejected, not dropped", () => {
    expect((schema as { additionalProperties?: unknown }).additionalProperties).toBe(false);
  });
});

describe("AdminIdentityRecord — notice and version are required", () => {
  const schema = schemaOf("AdminIdentityRecord");

  test("notice is required and is a ResponseText envelope", () => {
    expect(schema.required).toContain("notice");
    expect(JSON.stringify(schema.properties?.["notice"])).toContain("ResponseText");
  });

  test("version is required", () => {
    expect(schema.required).toContain("version");
  });
});

describe("AuthProviderSettingsCreateRequest", () => {
  const schema = schemaOf("AuthProviderSettingsCreateRequest");

  test("display_name is required — omitting it is a 400", () => {
    expect(schema.required).toContain("display_name");
  });

  test("enabled is a plain writable boolean, not server-controlled", () => {
    expect(schema.properties?.["enabled"]).toMatchObject({ type: "boolean" });
    expect(schema.required).not.toContain("enabled");
  });

  test("the fields the plan's field list omits all exist and are writable", () => {
    for (const field of [
      "redirect_uris",
      "expected_audiences",
      "allowed_algorithms",
      "trusted_jwt_issuer_id",
    ]) {
      expect(schema.properties).toHaveProperty(field);
    }
  });
});

/* -------------------------------------------------------------------------- */

describe("operation registry matches docs/openapi.json", () => {
  const names = Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[];

  for (const name of names) {
    const operation: MoiraOperation = MOIRA_OPERATIONS[name];

    test(`${name}: path, method and operationId exist in the spec`, () => {
      const declared = operationOf(operation);
      expect(declared.operationId).toBe(operation.id);
    });

    test(`${name}: declared security matches the registry's credential requirement`, () => {
      const declared = operationOf(operation);
      const schemes = securitySchemes(declared);

      if (operation.credential === "none") {
        expect(schemes).toBeNull();
      } else if (operation.credential === "system_key_only") {
        // The load-bearing distinction: a bearer JWT is refused here even if it
        // verifies. Anything else in this list would be a contract change.
        expect(schemes).toEqual(["systemKeyAuth"]);
      } else if (operation.credential === "bearer_only") {
        // The mirror image, and the reason the variant exists (plan 09 W5-D3).
        // `redeem_admin_invite` declares `bearerAuth` and nothing else so that
        // no token-asserted scope and no bootstrap credential can reach a path
        // that mints an `admin_identities` grant. If `systemKeyAuth` ever
        // appears here, this fails — and that spec change is itself a decision
        // needing its own argument about why the console should be able to
        // self-grant.
        expect(schemes).toEqual(["bearerAuth"]);
      } else {
        expect(schemes).not.toBeNull();
        expect(schemes).toContain("systemKeyAuth");
        expect(schemes).toContain("bearerAuth");
      }
    });

    test(`${name}: Idempotency-Key declaration matches the registry`, () => {
      const declared = operationOf(operation);
      const parameter = parameterNamed(declared, "Idempotency-Key");
      expect(parameter !== undefined).toBe(operation.declaresIdempotencyKey);
    });

    test(`${name}: If-Match requirement matches the registry`, () => {
      const declared = operationOf(operation);
      const parameter = parameterNamed(declared, "If-Match");
      expect(parameter?.required === true).toBe(operation.requiresIfMatch);
    });
  }
});

describe("the facts the console's behaviour depends on", () => {
  test("POST /api/v1/admin/setup/claim is systemKeyAuth ONLY", () => {
    const declared = operationOf(MOIRA_OPERATIONS.claimAdminIdentity);
    expect(declared.security).toEqual([{ systemKeyAuth: [] }]);
  });

  test("GET /api/v1/admin/setup/claim-status declares no security at all", () => {
    const declared = operationOf(MOIRA_OPERATIONS.getSetupClaimStatus);
    expect(declared.security).toBeUndefined();
  });

  test("the claim declares 400, 401, 403, 409 and 422 — 422 is a real outcome", () => {
    const declared = operationOf(MOIRA_OPERATIONS.claimAdminIdentity);
    for (const status of ["400", "401", "403", "409", "422", "503"]) {
      expect(Object.keys(declared.responses ?? {})).toContain(status);
    }
  });

  test("the auth-provider surface is exactly SEVEN operations, not ten", () => {
    const httpMethods = new Set(["get", "post", "patch", "delete", "put", "head", "options"]);
    const operations: string[] = [];
    for (const [path, item] of Object.entries(spec.paths)) {
      if (!path.startsWith("/api/v1/admin/auth/providers")) continue;
      for (const method of Object.keys(item)) {
        if (httpMethods.has(method)) operations.push(`${method.toUpperCase()} ${path}`);
      }
    }
    expect(operations.sort()).toHaveLength(7);
    expect(AUTH_PROVIDER_OPERATION_NAMES).toHaveLength(7);
  });

  test("there is no rotate-secret operation anywhere in the spec", () => {
    const paths = Object.keys(spec.paths).filter((path) => path.includes("rotate-secret"));
    expect(paths).toEqual([]);
  });

  test("enable declares NO Idempotency-Key, so step ordering cannot rely on one", () => {
    const declared = operationOf(MOIRA_OPERATIONS.enableAuthProvider);
    expect(parameterNamed(declared, "Idempotency-Key")).toBeUndefined();
    expect(parameterNamed(declared, "If-Match")?.required).toBe(true);
  });

  test("every LLM-configuration operation requires a credential — none is anonymous", () => {
    // The security posture for issue #73, asserted against the SPEC rather than
    // against the registry that transcribes it. The setup path is unauthenticated
    // by design because it runs before any admin exists; LLM configuration is
    // ordinary administration and shares none of that reasoning. A `security`
    // block that went missing from one of these would make a provider, a model or
    // a credential writable by anyone who can reach Moira.
    for (const name of LLM_CONFIG_OPERATION_NAMES) {
      const operation = MOIRA_OPERATIONS[name];
      expect(operation.credential, `${name} must require a credential`).toBe("admin");
      const schemes = securitySchemes(operationOf(operation));
      expect(schemes, `${name} declares no security block in the spec`).not.toBeNull();
      expect(schemes).toContain("bearerAuth");
      expect(schemes).toContain("systemKeyAuth");
    }
  });

  test("the routes family is registered READ-ONLY even though the spec has writes", () => {
    // `POST /api/v1/admin/routes` exists in the committed spec and is
    // deliberately unregistered — the `general` route is seeded by migration
    // 0005 and the create operation documents no 409 for a duplicate
    // `route_key`, so a console that offered it could land a second `general`.
    // Asserted in BOTH directions: the spec still has the write (or this comment
    // is stale), and the registry still refuses it.
    expect(Object.keys(spec.paths["/api/v1/admin/routes"] ?? {})).toContain("post");
    const routeOperations = Object.values(MOIRA_OPERATIONS).filter((operation) =>
      operation.path.startsWith("/api/v1/admin/routes"),
    );
    expect(routeOperations.map((operation) => operation.method).sort()).toEqual(["GET", "GET"]);
  });

  test("rotate_credential is the only operation declaring If-Match AND Idempotency-Key", () => {
    // Not a curiosity: the two headers answer different failures, and a reviewer
    // reading "creates take a key, mutations take a precondition" would otherwise
    // conclude one of them is redundant here.
    const both = (Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[])
      .filter((name) => {
        const declared = operationOf(MOIRA_OPERATIONS[name]);
        return (
          parameterNamed(declared, "Idempotency-Key") !== undefined &&
          parameterNamed(declared, "If-Match")?.required === true
        );
      })
      .sort();
    // `patch_admin_identity` is the other one, and it predates this surface.
    expect(both).toEqual(["patchAdminIdentity", "rotateProviderCredential"]);
  });

  test("PUT runtime-policy is the ONLY mutation with an OPTIONAL If-Match, and is unregistered", () => {
    // The reason `LLM_CONFIG_OPERATION_NAMES` stops where it does. Registering it
    // would put an operation in the table whose `requiresIfMatch: false` means
    // "declared but optional" rather than "not declared" — two different facts
    // that `#buildHeaders` cannot tell apart, since it REFUSES an `ifMatch` on any
    // operation whose flag is false.
    const declared = spec.paths["/api/v1/admin/providers/{provider_id}/runtime-policy"]?.["put"];
    expect(declared).toBeDefined();
    const ifMatch = parameterNamed(declared!, "If-Match");
    expect(ifMatch).toBeDefined();
    expect(ifMatch?.required === true).toBe(false);
    expect(
      Object.values(MOIRA_OPERATIONS).map((operation) => operation.path),
    ).not.toContain("/api/v1/admin/providers/{provider_id}/runtime-policy");
  });

  test("CredentialSecret's api_key and azure arms are genuinely ambiguous", () => {
    // The fact `assertCredentialCreateIsSafe` exists for, read off the spec so it
    // cannot quietly stop being true. Two `oneOf` arms both require `api_key`;
    // one additionally allows a nullable `endpoint`. A body carrying both keys
    // therefore satisfies BOTH arms and `oneOf` refuses it.
    const schema = spec.components.schemas["CredentialSecret"] as unknown as {
      oneOf?: Array<{ properties?: Record<string, unknown>; required?: string[] }>;
    };
    const arms = (schema.oneOf ?? []).filter((arm) => (arm.required ?? []).includes("api_key"));
    expect(arms).toHaveLength(2);
    const withEndpoint = arms.filter((arm) => "endpoint" in (arm.properties ?? {}));
    expect(withEndpoint).toHaveLength(1);
    // And `endpoint` is NULLABLE, which is why sending `endpoint: null` does not
    // disambiguate — it is a legal value of the azure arm.
    expect(JSON.stringify(withEndpoint[0]?.properties?.["endpoint"])).toContain("null");
  });

  test("provider_type is absent from the patch schema — it is immutable", () => {
    const patch = schemaOf("ProviderPatchRequest");
    expect(Object.keys(patch.properties ?? {})).not.toContain("provider_type");
    expect((patch as { additionalProperties?: unknown }).additionalProperties).toBe(false);
    // So the refusal is a flat 400 naming nothing, which is why the console
    // refuses first — see `assertLlmProviderPatchIsSafe`.
    expect(schemaOf("ProviderCreateRequest").required).toContain("provider_type");
  });

  test("of the ten operations the console binds to, exactly two declare Idempotency-Key", () => {
    // jwt-issuers is a separate surface and is excluded from the count of ten.
    const boundNames: MoiraOperationName[] = [
      "getSetupClaimStatus",
      "getSetupAuthMethods",
      "claimAdminIdentity",
      ...AUTH_PROVIDER_OPERATION_NAMES,
    ];
    expect(boundNames).toHaveLength(10);
    const withKey = boundNames.filter(
      (name) =>
        parameterNamed(operationOf(MOIRA_OPERATIONS[name]), "Idempotency-Key") !== undefined,
    );
    expect(withKey.sort()).toEqual(["claimAdminIdentity", "createAuthProvider"]);
  });
});
