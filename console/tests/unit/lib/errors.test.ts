import { describe, expect, test } from "bun:test";

import {
  CONSOLE_MALFORMED_ERROR_KEY,
  CONSOLE_TRANSPORT_ERROR_KEY,
  MOIRA_CODE_REMEDIES,
  isActionableSetupCondition,
  isSessionExpired,
  remedyForStatus,
  toMoiraError,
  toTransportError,
  type MoiraError,
} from "@/lib/errors";
// Moved behind `import "server-only"` in plan 09 wave 3: it is the only function in
// the error surface that returns unfiltered `details`, and `lib/errors.ts` is
// deliberately client-safe. `bunfig.toml` preloads the shim that makes the marker
// package importable under `bun test`.
import { serverDiagnostics } from "@/lib/errors-server";
import { MOIRA_SETUP_ERROR_CODES } from "@/lib/moira-keys";
import { errorEnvelope } from "../../support/moira-stub";

/** Every string anywhere inside a value, however nested. */
function deepStrings(value: unknown, acc: string[] = []): string[] {
  if (typeof value === "string") acc.push(value);
  else if (Array.isArray(value)) for (const item of value) deepStrings(item, acc);
  else if (value !== null && typeof value === "object") {
    for (const [key, nested] of Object.entries(value)) {
      acc.push(key);
      deepStrings(nested, acc);
    }
  }
  return acc;
}

describe("the server/client boundary", () => {
  const envelope = errorEnvelope("admin_claim_domain_not_allowed", {
    message: "This email domain is not allowed to claim an admin identity.",
    messageArgs: { domain: "example.com" },
    requestId: "req_0123456789",
    details: { sql_state: "23505", internal_hint: "operator-only diagnostic" },
  });

  const mapped = toMoiraError(403, envelope);

  test("message_key, message and message_args cross", () => {
    expect(mapped.text.messageKey).toBe("moira.error.admin_claim_domain_not_allowed");
    expect(mapped.text.message).toBe(
      "This email domain is not allowed to claim an admin identity.",
    );
    expect(mapped.text.messageArgs).toEqual({ domain: "example.com" });
  });

  test("details does NOT cross", () => {
    const strings = deepStrings(mapped);
    expect(strings).not.toContain("details");
    expect(strings).not.toContain("23505");
    expect(strings).not.toContain("operator-only diagnostic");
    expect(JSON.stringify(mapped)).not.toContain("sql_state");
  });

  test("request_id does NOT cross", () => {
    expect(JSON.stringify(mapped)).not.toContain("req_0123456789");
    expect(deepStrings(mapped)).not.toContain("request_id");
  });

  test("but request_id and details ARE available server-side for logging", () => {
    const diagnostics = serverDiagnostics(envelope);
    expect(diagnostics.requestId).toBe("req_0123456789");
    expect(diagnostics.details).toEqual({
      sql_state: "23505",
      internal_hint: "operator-only diagnostic",
    });
  });

  test("the client-safe object has exactly the fields it declares — no spread leakage", () => {
    expect(Object.keys(mapped).sort()).toEqual([
      "code",
      "kind",
      "remedy",
      "retryable",
      "status",
      "text",
    ]);
    expect(Object.keys(mapped.text).sort()).toEqual(["message", "messageArgs", "messageKey"]);
  });

  test("a field Moira adds to ErrorDetail tomorrow does not leak today", () => {
    const withNewField = errorEnvelope("forbidden");
    (withNewField.error as unknown as Record<string, unknown>)["operator_stack_trace"] =
      "at moira::secret::decrypt";
    const result = toMoiraError(403, withNewField);
    expect(JSON.stringify(result)).not.toContain("operator_stack_trace");
    expect(JSON.stringify(result)).not.toContain("moira::secret::decrypt");
  });
});

describe("401 disambiguation — the rule that must not regress", () => {
  const badSystemKey = toMoiraError(401, errorEnvelope("setup_claim_credential_required"));
  const staleSession = toMoiraError(401, errorEnvelope("unauthorized"));

  test("setup_claim_credential_required does NOT route into the sign-out flow", () => {
    expect(badSystemKey.remedy).toBe("fix_system_key");
    expect(isSessionExpired(badSystemKey)).toBe(false);
  });

  test("an ordinary 401 does", () => {
    expect(staleSession.remedy).toBe("reauthenticate");
    expect(isSessionExpired(staleSession)).toBe(true);
  });

  test("the code table wins over the status default", () => {
    // The whole mechanism: `remedyForStatus(401)` would sign the operator out.
    expect(remedyForStatus(401)).toBe("reauthenticate");
    expect(MOIRA_CODE_REMEDIES["setup_claim_credential_required"]).toBe("fix_system_key");
    expect(badSystemKey.remedy).not.toBe(remedyForStatus(401));
  });

  test("an unknown 401 code still signs out", () => {
    const unknown = toMoiraError(401, errorEnvelope("some_future_auth_code"));
    expect(isSessionExpired(unknown)).toBe(true);
  });
});

describe("the eight codes plan 08's body never mapped", () => {
  const cases: ReadonlyArray<readonly [string, number, string]> = [
    ["setup_token_not_supported", 400, "report_bug"],
    ["setup_claim_credential_required", 401, "fix_system_key"],
    ["admin_claim_email_required", 400, "choose_different_identity"],
    ["admin_claim_email_not_verified", 403, "choose_different_identity"],
    ["scope_invalid", 422, "report_bug"],
    ["console_issuer_must_not_assert_scopes", 400, "fix_setup_configuration"],
    ["auth_provider_method_config_incomplete", 400, "fix_form_input"],
    ["auth_provider_url_not_allowed", 400, "fix_form_input"],
  ];

  for (const [code, status, remedy] of cases) {
    test(`${code} (${status}) maps to ${remedy}`, () => {
      const mapped = toMoiraError(status, errorEnvelope(code));
      expect(mapped.kind).toBe("api");
      expect(mapped.remedy).toBe(remedy as never);
      // Never the generic status fallback dressed up as a mapping.
      expect(mapped.text.messageKey).toBe(`moira.error.${code}`);
    });
  }

  test("scope_invalid arrives as 422, not 400 — and both map identically", () => {
    // `normalize_scopes` runs before the issuer is resolved, via
    // `AppError::unprocessable`. Mapping it as a 400 would put it in the
    // form-validation bucket, where it does not belong: the console omits
    // `scopes` entirely, so this firing is a console bug.
    expect(toMoiraError(422, errorEnvelope("scope_invalid")).remedy).toBe("report_bug");
    expect(toMoiraError(400, errorEnvelope("scope_invalid")).remedy).toBe("report_bug");
  });

  test("admin_claim_email_not_verified is distinct from domain_not_allowed", () => {
    const unverified = toMoiraError(403, errorEnvelope("admin_claim_email_not_verified"));
    const domain = toMoiraError(403, errorEnvelope("admin_claim_domain_not_allowed"));
    expect(unverified.remedy).not.toBe(domain.remedy);
  });
});

describe("actionable setup conditions are not failures", () => {
  test("admin_claim_domain_not_allowed routes back to the auth-settings step", () => {
    const mapped = toMoiraError(403, errorEnvelope("admin_claim_domain_not_allowed"));
    expect(isActionableSetupCondition(mapped)).toBe(true);
    expect(mapped.remedy).toBe("fix_setup_configuration");
    // Not a denial, not a sign-out.
    expect(isSessionExpired(mapped)).toBe(false);
  });

  test("a plain 403 forbidden is a denial, not a setup instruction", () => {
    const mapped = toMoiraError(403, errorEnvelope("forbidden"));
    expect(isActionableSetupCondition(mapped)).toBe(false);
    expect(mapped.remedy).toBe("denied");
  });

  test("unregistered_trusted_issuer is also a setup instruction", () => {
    expect(
      isActionableSetupCondition(toMoiraError(400, errorEnvelope("unregistered_trusted_issuer"))),
    ).toBe(true);
  });
});

describe("non-envelope failures", () => {
  test("an unreadable body becomes a `malformed` error, never a fabricated code", () => {
    const mapped = toMoiraError(502, "<html>Bad Gateway</html>");
    expect(mapped.kind).toBe("malformed");
    expect(mapped.text.messageKey).toBe(CONSOLE_MALFORMED_ERROR_KEY);
    expect(JSON.stringify(mapped)).not.toContain("Bad Gateway");
  });

  test("an undefined body is handled", () => {
    const mapped = toMoiraError(500, undefined);
    expect(mapped.kind).toBe("malformed");
    expect(mapped.retryable).toBe(true);
  });

  test("a transport failure is its own kind and never echoes the thrown cause", () => {
    const mapped = toTransportError(new Error("connect ECONNREFUSED 10.0.0.1:443"));
    expect(mapped.kind).toBe("transport");
    expect(mapped.text.messageKey).toBe(CONSOLE_TRANSPORT_ERROR_KEY);
    expect(JSON.stringify(mapped)).not.toContain("10.0.0.1");
    expect(JSON.stringify(mapped)).not.toContain("ECONNREFUSED");
  });

  test("an abort is distinguishable from a network failure", () => {
    const abort = new Error("aborted");
    abort.name = "AbortError";
    expect(toTransportError(abort).text.messageArgs).toEqual({ reason: "aborted" });
  });

  test("serverDiagnostics on a non-envelope yields nulls rather than throwing", () => {
    expect(serverDiagnostics("<html>")).toEqual({ requestId: null, details: null });
  });
});

describe("the union is exhaustively discriminated", () => {
  const samples: MoiraError[] = [
    toMoiraError(403, errorEnvelope("forbidden")),
    toMoiraError(502, "not json"),
    toTransportError(new Error("boom")),
  ];

  test("every member carries kind, remedy, retryable and text", () => {
    for (const sample of samples) {
      expect(typeof sample.kind).toBe("string");
      expect(typeof sample.remedy).toBe("string");
      expect(typeof sample.retryable).toBe("boolean");
      expect(typeof sample.text.messageKey).toBe("string");
      expect(typeof sample.text.message).toBe("string");
    }
  });

  test("switching on kind is exhaustive", () => {
    for (const sample of samples) {
      switch (sample.kind) {
        case "api":
          expect(typeof sample.code).toBe("string");
          break;
        case "malformed":
          expect(typeof sample.status).toBe("number");
          break;
        case "transport":
          expect(sample.retryable).toBe(true);
          break;
      }
    }
  });
});

describe("coverage of the mirrored key list", () => {
  test("every mirrored error code has an explicit remedy", () => {
    const unmapped = MOIRA_SETUP_ERROR_CODES.filter(
      (code) => MOIRA_CODE_REMEDIES[code] === undefined,
    );
    // An unmapped code would silently take the status default, which is exactly
    // how `setup_claim_credential_required` would sign an operator out.
    expect(unmapped).toEqual([]);
  });
});
