/**
 * ============================================================================
 * WHY THIS FILE EXISTS
 * ============================================================================
 *
 * CONVENTIONS.md §3 ("Secret-leak test") and §8 ("No secret-leak: verified by
 * test") make it a merge gate that no Moira system key, admin key, OAuth client
 * secret, console encryption key, or decrypted provider credential ever reaches
 * a client bundle, an HTML payload, or any browser-visible response.
 * §7.5 states the same rule as a non-negotiable security invariant, and §6
 * rule 5 forbids passing such a value as a prop past the page/server boundary.
 *
 * The failure mode this guards against is *silent*: `process.env.MOIRA_SYSTEM_KEY`
 * referenced from a component that turns out to be a client component, a
 * server-only value serialised into RSC flight data or a `<script>` payload, a
 * value accidentally exposed as `NEXT_PUBLIC_*`, or a debug `console.log`. None
 * of those break any feature — the console keeps working perfectly while
 * leaking the key. Only a test catches it.
 *
 * ----------------------------------------------------------------------------
 * HOW IT KEEPS WORKING ONCE PLAN 08 INTRODUCES REAL SECRETS
 * ----------------------------------------------------------------------------
 *
 * The scaffold has no real secrets yet, so the gate seeds its own: SENTINEL_ENV
 * below is injected into the console server's environment by
 * `playwright.config.ts` (server-side only — never `NEXT_PUBLIC_*`). Those
 * values are high-entropy and unmistakable, so if any of them ever surfaces in
 * browser-visible output, it can only have got there by the exact mechanism the
 * gate exists to catch.
 *
 * The needle set is NOT just the sentinels, though. `forbiddenValues()` also
 * harvests every ambient environment variable whose *name* looks like a secret
 * (SECRET / PASSWORD / CREDENTIAL / TOKEN / KEY) and is not `NEXT_PUBLIC_`.
 * The console server inherits that same ambient environment, so the moment plan
 * 08 sets `MOIRA_SYSTEM_KEY`, `BETTER_AUTH_SECRET`, `GOOGLE_CLIENT_SECRET` or
 * `CONSOLE_SECRET_ENCRYPTION_KEY` in the e2e environment, those real values
 * become forbidden needles automatically, with no edit to any spec.
 *
 * On top of value matching there are two structural rules that need no known
 * value at all, so they catch secrets that were never in our environment:
 *   - any `-----BEGIN ... PRIVATE KEY-----` PEM header in client-visible output;
 *   - any `NEXT_PUBLIC_*` identifier whose name contains SECRET/KEY/TOKEN/
 *     PASSWORD/CREDENTIAL (Next.js inlines those into the client bundle).
 *
 * ----------------------------------------------------------------------------
 * WHAT WOULD MAKE THIS TEST FAIL
 * ----------------------------------------------------------------------------
 *
 *   - a server-only env value rendered into HTML, RSC flight data, or any
 *     browser-observed response body;
 *   - a secret inlined into a `.next/static/**` chunk or leaked via a sourcemap;
 *   - a secret written to the browser console;
 *   - re-exporting a secret through `NEXT_PUBLIC_*`;
 *   - a private key (PEM) served to the browser — e.g. publishing the Better
 *     Auth JWT signing key instead of the JWKS public half.
 *
 * The assertion is `violations === []`, not "the sentinel was absent". That
 * distinction matters: the gate reports *every* offending value/location it
 * finds, so it keeps its teeth as new secrets appear rather than only proving
 * that this run's fixture happened not to leak.
 */

/**
 * Server-only sentinel values injected into the console server's environment.
 *
 * Deliberately named `MOIRA_E2E_SENTINEL_*` rather than `MOIRA_SYSTEM_KEY` etc.
 * so no application code can mistake them for real configuration. None of these
 * names is prefixed `NEXT_PUBLIC_`, which is itself part of what is asserted.
 */
export const SENTINEL_ENV: Readonly<Record<string, string>> = {
  MOIRA_E2E_SENTINEL_SYSTEM_KEY: "moira-e2e-sentinel-system-key-9f2c47ab13e64d05b8ce7a10df365b92",
  MOIRA_E2E_SENTINEL_ADMIN_KEY: "moira-e2e-sentinel-admin-key-5a08d3f16b7c42e9803fbb1c6d4e7a25",
  MOIRA_E2E_SENTINEL_OAUTH_CLIENT_SECRET:
    "moira-e2e-sentinel-oauth-client-secret-4b71ce09a2d84f36951e2c807bd6a4f1",
  MOIRA_E2E_SENTINEL_ENCRYPTION_KEY:
    "moira-e2e-sentinel-encryption-key-6d5e8a2c0b9147f3a72d09e5c418b6f0",
  MOIRA_E2E_SENTINEL_DECRYPTED_CREDENTIAL:
    "moira-e2e-sentinel-decrypted-credential-1c8f0b6a45d29e7360b4af92d1053e8c",
};

/** A value we must never find in client-visible output, plus a safe label. */
export interface Needle {
  /** Human-readable origin, safe to print in a failure message. */
  readonly label: string;
  readonly value: string;
}

const SECRET_NAME_PATTERN = /(SECRET|PASSWORD|PASSWD|CREDENTIAL|TOKEN|KEY)/i;

/**
 * Env var names that match SECRET_NAME_PATTERN but are structural, not secret.
 * Keeping this list short and explicit is intentional — a false positive here
 * is a broken build, a false negative is a leaked key.
 */
const AMBIENT_NAME_DENYLIST = new Set([
  "SSH_AUTH_SOCK",
  "GPG_TTY",
  "KEYBOARD",
  "XKB_DEFAULT_LAYOUT",
  "PLAYWRIGHT_BROWSERS_PATH",
  "npm_config_keyword",
]);

/**
 * Values too weak to match on. A 4-character value or a filesystem path would
 * collide with ordinary bundle content (paths in particular appear verbatim in
 * sourcemaps), producing a noisy gate that people would then disable.
 */
function isUsableNeedle(value: string): boolean {
  if (value.length < 12) return false;
  if (/\s/.test(value)) return false;
  if (value.includes("/") || value.includes("\\")) return false;
  return true;
}

/**
 * Env vars that carry a credential INSIDE a URL rather than as the whole value.
 *
 * A DSN is the awkward case and the console now has one. `CONSOLE_DATABASE_URL`
 * matches none of `SECRET_NAME_PATTERN`, so the ambient harvester never sees it;
 * and even if it did, the DSN contains `/` and `isUsableNeedle` would discard
 * it, because matching a whole connection string against a sourcemap full of
 * paths is how a gate becomes noise. So the PASSWORD is extracted and used as
 * the needle — that is the part whose disclosure matters.
 */
const CONNECTION_STRING_NAME_PATTERN = /(DATABASE_URL|DSN|CONNECTION_STRING)/i;

/** The password half of a connection string, if it has one worth matching. */
function connectionStringPassword(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  const password = decodeURIComponent(url.password);
  return password !== "" && isUsableNeedle(password) ? password : null;
}

/**
 * Every value that must not appear in client-visible output:
 * the seeded sentinels, any ambient secret-shaped env var that the console
 * server process also inherits, and the password half of any connection string
 * it was given.
 */
export function forbiddenValues(env: NodeJS.ProcessEnv = process.env): Needle[] {
  const needles = new Map<string, Needle>();

  for (const [name, value] of Object.entries(SENTINEL_ENV)) {
    needles.set(value, { label: `${name} (seeded sentinel)`, value });
  }

  for (const [name, value] of Object.entries(env)) {
    if (typeof value !== "string") continue;
    if (name.startsWith("NEXT_PUBLIC_")) continue; // handled by the name rule
    if (AMBIENT_NAME_DENYLIST.has(name)) continue;

    if (CONNECTION_STRING_NAME_PATTERN.test(name)) {
      const password = connectionStringPassword(value);
      if (password !== null && !needles.has(password)) {
        needles.set(password, { label: `${name} password (ambient server env)`, value: password });
      }
      // Fall through: a DSN-named variable may also match the secret-name
      // pattern, and the whole value is still worth adding when it is usable.
    }

    if (!SECRET_NAME_PATTERN.test(name)) continue;
    if (!isUsableNeedle(value)) continue;
    if (needles.has(value)) continue;
    needles.set(value, { label: `${name} (ambient server env)`, value });
  }

  return [...needles.values()];
}

/**
 * Structural leak signatures that need no known value.
 * `-----BEGIN` alone is intentionally broad: a public certificate has no place
 * in a client bundle either, and being over-broad here is cheap.
 */
export const FORBIDDEN_PATTERNS: ReadonlyArray<{
  readonly label: string;
  readonly pattern: RegExp;
}> = [
  {
    label: "PEM key/certificate header",
    pattern: /-{5}BEGIN [A-Z ]*(PRIVATE KEY|CERTIFICATE|RSA|EC )/,
  },
  {
    label: "secret-shaped NEXT_PUBLIC_* identifier",
    pattern: /NEXT_PUBLIC_[A-Z0-9_]*(SECRET|KEY|TOKEN|PASSWORD|CREDENTIAL)/i,
  },
  {
    // Value-independent, like the PEM rule: any PostgreSQL connection string
    // carrying inline credentials is a leak regardless of whether this run
    // happened to know the password. It fires on a DSN from a config file, from
    // a stack trace, or from a database driver's own error message — all of
    // which are how a connection string actually reaches a browser, and none of
    // which the needle set would cover.
    label: "PostgreSQL connection string with inline credentials",
    pattern: /postgres(?:ql)?:\/\/[^\s:@/"'`]+:[^\s:@/"'`]+@/i,
  },
];

/** One concrete leak: what was found, and where. */
export interface Leak {
  readonly where: string;
  readonly what: string;
  readonly snippet: string;
}

/** Redact every needle out of a snippet so the report never leaks the secret. */
function redact(text: string, needles: readonly Needle[]): string {
  let redacted = text;
  for (const needle of needles) {
    redacted = redacted.split(needle.value).join(`«REDACTED:${needle.label}»`);
  }
  return redacted;
}

function snippetAround(
  haystack: string,
  index: number,
  length: number,
  needles: readonly Needle[],
): string {
  const start = Math.max(0, index - 80);
  const end = Math.min(haystack.length, index + length + 80);
  const raw = haystack.slice(start, end).replace(/\s+/g, " ");
  return redact(raw, needles);
}

/**
 * Scan one blob of client-visible content for every forbidden value and
 * pattern. Returns *all* hits, not the first — a report that stops at the first
 * leak hides the rest.
 */
export function scanForLeaks(where: string, content: string, needles: readonly Needle[]): Leak[] {
  const leaks: Leak[] = [];
  if (content.length === 0) return leaks;

  for (const needle of needles) {
    const index = content.indexOf(needle.value);
    if (index !== -1) {
      leaks.push({
        where,
        what: `secret value from ${needle.label}`,
        snippet: snippetAround(content, index, needle.value.length, needles),
      });
    }
  }

  for (const { label, pattern } of FORBIDDEN_PATTERNS) {
    const match = pattern.exec(content);
    if (match) {
      leaks.push({
        where,
        what: label,
        snippet: snippetAround(content, match.index, match[0].length, needles),
      });
    }
  }

  return leaks;
}

/** Render a leak list into a failure message that is actionable but redacted. */
export function describeLeaks(leaks: readonly Leak[]): string {
  return leaks
    .map((l, i) => `  ${i + 1}. ${l.what}\n     in: ${l.where}\n     …${l.snippet}…`)
    .join("\n");
}
