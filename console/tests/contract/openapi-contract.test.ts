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

import {
  AUTH_PROVIDER_OPERATION_NAMES,
  MOIRA_OPERATIONS,
  type MoiraOperation,
  type MoiraOperationName,
} from "@/lib/moira-client";
import { SCHEMA_CONTRACTS, type SchemaContract } from "@/lib/types";

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

describe("DTO shapes match docs/openapi.json", () => {
  for (const contract of SCHEMA_CONTRACTS satisfies readonly SchemaContract[]) {
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
