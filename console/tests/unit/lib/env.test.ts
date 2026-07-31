// Boot-time configuration validation. Every case here is one that would
// otherwise fail late, somewhere else, as something else.

import { describe, expect, test } from "bun:test";

import {
  AUTH_BASE_PATH,
  AUTH_JWKS_PATH,
  ConsoleConfigError,
  readConsoleEnv,
  secretFingerprint,
  type EnvSource,
} from "@/lib/env";

const VALID: EnvSource = {
  NODE_ENV: "production",
  MOIRA_API_URL: "https://moira.internal",
  CONSOLE_PUBLIC_ORIGIN: "https://console.example.com",
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-api",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 3).toString("base64"),
  CONSOLE_DATABASE_URL: "postgres://console:pw@db.internal:5432/console_auth",
};

/** The same environment with the database omitted. */
function withoutDatabase(source: EnvSource): EnvSource {
  const { CONSOLE_DATABASE_URL: _omitted, ...rest } = source;
  return rest;
}

function problemsOf(source: EnvSource): string[] {
  try {
    readConsoleEnv(source);
  } catch (error) {
    if (error instanceof ConsoleConfigError) return [...error.problems];
    throw error;
  }
  return [];
}

describe("a valid environment", () => {
  test("parses", () => {
    const env = readConsoleEnv(VALID);
    expect(env.moiraBaseUrl).toBe("https://moira.internal");
    expect(env.consoleOrigin).toBe("https://console.example.com");
    expect(env.adminApiAudience).toBe("moira-admin-api");
    expect(env.secretEncryptionKey.byteLength).toBe(32);
    expect(env.allowInsecureUrls).toBe(false);
  });

  test("trims trailing slashes so URL concatenation cannot double them", () => {
    const env = readConsoleEnv({
      ...VALID,
      MOIRA_API_URL: "https://moira.internal///",
      CONSOLE_PUBLIC_ORIGIN: "https://console.example.com/",
    });
    expect(env.moiraBaseUrl).toBe("https://moira.internal");
    expect(env.consoleOrigin).toBe("https://console.example.com");
  });

  test("the issuer defaults to the console origin but is separable", () => {
    expect(readConsoleEnv(VALID).bffIssuerUrl).toBe("https://console.example.com");
    expect(
      readConsoleEnv({ ...VALID, MOIRA_BFF_ISSUER_URL: "https://issuer.example.com" }).bffIssuerUrl,
    ).toBe("https://issuer.example.com");
  });

  test("the JWKS URL is DERIVED, never independently configurable", () => {
    // A `jwks_url` that does not point at the document the console actually
    // serves is a deployment that authenticates nothing, and the failure is
    // invisible until Moira rejects a token.
    const env = readConsoleEnv(VALID);
    expect(env.jwksUrl).toBe(`https://console.example.com${AUTH_BASE_PATH}${AUTH_JWKS_PATH}`);
  });

  test("the system key is optional — the console must work after it is removed", () => {
    expect(readConsoleEnv(VALID).moiraSystemKey).toBeUndefined();
    expect(readConsoleEnv({ ...VALID, MOIRA_SYSTEM_KEY: "sk_x" }).moiraSystemKey).toBe("sk_x");
  });
});

describe("missing values are named, not inferred", () => {
  test("every required variable is reported by name", () => {
    const problems = problemsOf({ NODE_ENV: "production" });
    const joined = problems.join("\n");
    for (const name of [
      "MOIRA_API_URL",
      "CONSOLE_PUBLIC_ORIGIN",
      "MOIRA_ADMIN_API_AUDIENCE",
      "BETTER_AUTH_SECRET",
      "CONSOLE_SECRET_ENCRYPTION_KEY",
      "CONSOLE_DATABASE_URL",
    ]) {
      expect(joined).toContain(name);
    }
  });

  test("all problems are reported at once, not one per restart", () => {
    expect(problemsOf({ NODE_ENV: "production" }).length).toBeGreaterThan(3);
  });

  test("a whitespace-only value counts as missing", () => {
    expect(problemsOf({ ...VALID, MOIRA_ADMIN_API_AUDIENCE: "   " })).toContain(
      "MOIRA_ADMIN_API_AUDIENCE is required and was empty",
    );
  });
});

describe("URL scheme policy mirrors Moira's own", () => {
  test("http is refused by default", () => {
    expect(problemsOf({ ...VALID, MOIRA_API_URL: "http://moira.internal" }).join()).toContain(
      "must be an https URL",
    );
  });

  test("http is permitted for a fixture, outside production", () => {
    const env = readConsoleEnv({
      ...VALID,
      NODE_ENV: "test",
      CONSOLE_ALLOW_INSECURE_URLS: "true",
      CONSOLE_PUBLIC_ORIGIN: "http://localhost:3210",
    });
    expect(env.allowInsecureUrls).toBe(true);
    expect(env.consoleOrigin).toBe("http://localhost:3210");
  });

  test("the fixture knob is a HARD failure in production", () => {
    // The same shape as Moira's `auth.jwks.allow_insecure_dev_urls`, which
    // `Settings::validate` hard-fails in production. A console that allowed
    // itself an insecure origin there would register a `jwks_url` Moira's SSRF
    // policy must refuse anyway.
    expect(problemsOf({ ...VALID, CONSOLE_ALLOW_INSECURE_URLS: "true" }).join()).toContain(
      "CONSOLE_ALLOW_INSECURE_URLS must be false in production",
    );
  });

  test("`https://localhost:PORT` is accepted — the mock-IdP case", () => {
    // Moira's `validate_https_url` checks scheme and non-empty host only, with
    // no private-host check, so this form passes there too. Matching that
    // exactly is what makes a TLS fixture usable end to end.
    const env = readConsoleEnv({
      ...VALID,
      NODE_ENV: "test",
      MOIRA_API_URL: "https://localhost:8443",
    });
    expect(env.moiraBaseUrl).toBe("https://localhost:8443");
  });

  test("a non-absolute URL is refused", () => {
    expect(problemsOf({ ...VALID, MOIRA_API_URL: "moira.internal" }).join()).toContain(
      "is not an absolute URL",
    );
  });
});

describe("the encryption key", () => {
  test("must decode to exactly 32 bytes", () => {
    expect(
      problemsOf({
        ...VALID,
        CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(16, 1).toString("base64"),
      }).join(),
    ).toContain("exactly 32 bytes");
  });

  test("the error tells the operator how to make one", () => {
    expect(problemsOf({ ...VALID, CONSOLE_SECRET_ENCRYPTION_KEY: "short" }).join()).toContain(
      "openssl rand -base64 32",
    );
  });
});

describe("the BETTER_AUTH_SECRET length floor", () => {
  test("a short secret is refused", () => {
    expect(problemsOf({ ...VALID, BETTER_AUTH_SECRET: "too-short" }).join()).toContain(
      "at least 32 characters",
    );
  });
});

describe("the console's own database", () => {
  test("is parsed and kept verbatim when valid", () => {
    expect(readConsoleEnv(VALID).consoleDatabaseUrl).toBe(
      "postgres://console:pw@db.internal:5432/console_auth",
    );
    expect(
      readConsoleEnv({
        ...VALID,
        CONSOLE_DATABASE_URL: "postgresql://console@db/console_auth?sslmode=require",
      }).consoleDatabaseUrl,
    ).toBe("postgresql://console@db/console_auth?sslmode=require");
  });

  test("is a HARD boot failure in production", () => {
    // Without it the console silently runs on in-memory storage: the OAuth
    // client secret and the ES256 signing key are lost on every restart, and a
    // second replica publishes a different JWKS. None of that announces itself,
    // which is exactly why omission must not be a valid production state.
    const problems = problemsOf(withoutDatabase(VALID)).join("\n");
    expect(problems).toContain("CONSOLE_DATABASE_URL is required in production");
    expect(problems).toContain("in-memory storage");
  });

  test("is optional outside production, which is what keeps `bun test` runnable", () => {
    const env = readConsoleEnv({ ...withoutDatabase(VALID), NODE_ENV: "test" });
    expect(env.consoleDatabaseUrl).toBeUndefined();
  });

  test("a non-postgres scheme is refused rather than handed to the driver", () => {
    expect(
      problemsOf({ ...VALID, CONSOLE_DATABASE_URL: "mysql://c:p@db/console" }).join(),
    ).toContain("must be a postgres:// or postgresql:// URL");
  });

  test("a DSN naming no database is refused", () => {
    // The single most likely paste error, and the one that would silently put
    // the console in the `postgres` maintenance database next to nothing.
    expect(
      problemsOf({ ...VALID, CONSOLE_DATABASE_URL: "postgres://c:p@db:5432" }).join(),
    ).toContain("names no database");
  });

  test("an unparseable value is refused", () => {
    expect(problemsOf({ ...VALID, CONSOLE_DATABASE_URL: "not a url" }).join()).toContain(
      "is not a valid connection string URL",
    );
  });

  test("no problem message ever contains the DSN or its password", () => {
    // A `ConsoleConfigError` is printed to a log. A connection string is the one
    // configuration value that carries a credential inline.
    const dsn = "postgres://console:sup3r-s3cret-pw@db.internal:5432";
    const problems = problemsOf({ ...VALID, CONSOLE_DATABASE_URL: dsn }).join("\n");
    expect(problems).toContain("CONSOLE_DATABASE_URL");
    expect(problems).not.toContain("sup3r-s3cret-pw");
    expect(problems).not.toContain(dsn);
  });
});

describe("nothing server-side may be NEXT_PUBLIC_", () => {
  test.each([
    "NEXT_PUBLIC_MOIRA_SYSTEM_KEY",
    "NEXT_PUBLIC_BETTER_AUTH_SECRET",
    "NEXT_PUBLIC_CONSOLE_SECRET_ENCRYPTION_KEY",
    "NEXT_PUBLIC_CONSOLE_OAUTH_CLIENT_SECRET",
    // A DSN carries the database password inline; the name matches none of the
    // leak harness's SECRET/KEY/TOKEN patterns, so it is named explicitly here
    // and in `NEVER_PUBLIC_ENV_NAMES`.
    "NEXT_PUBLIC_CONSOLE_DATABASE_URL",
  ])("%s is a boot failure", (name) => {
    // Next.js inlines NEXT_PUBLIC_* into the browser bundle at build time. The
    // check fires on the prefixed NAME, so it catches the mistake itself —
    // plausibly made while debugging why a value "isn't visible" — rather than
    // its consequences.
    expect(problemsOf({ ...VALID, [name]: "anything" }).join()).toContain(name);
  });
});

describe("secretFingerprint", () => {
  test("is stable, short, and not the secret", () => {
    const value = "an-oauth-client-secret";
    const fingerprint = secretFingerprint(value);
    expect(fingerprint).toBe(secretFingerprint(value));
    expect(fingerprint).toHaveLength(12);
    expect(fingerprint).not.toContain(value);
  });

  test("distinguishes different secrets", () => {
    expect(secretFingerprint("a")).not.toBe(secretFingerprint("b"));
  });
});
