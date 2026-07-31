// `t()` — the console's message resolver.
//
// ============================================================================
// RESOLUTION ORDER IS MOIRA'S, UNCHANGED
// ============================================================================
//
//   catalog entry  →  supplied fallback  →  the key itself
//
// which is exactly `catalog_message` in `src/lib.rs:120-124`
// (`default_message_for_key(key).unwrap_or(fallback)`), with the key surviving
// as the last resort so a missing entry renders something an operator can quote
// in a bug report rather than an empty string.
//
// The middle slot is the SERVER's `message`. Moira has already interpolated it
// (`ResponseText.message` arrives fully formed), so it is rendered VERBATIM —
// running it through the placeholder substituter would be a second interpolation
// over text that may legitimately contain braces.
//
// Placeholder substitution therefore applies to CATALOG messages only. That
// asymmetry is the point: the console owns the catalog's placeholder vocabulary
// and can guarantee `{status}` means what `messageArgs` says it means; it owns
// nothing about a string Moira formatted.
//
// ============================================================================
// WHY THIS MODULE IS CLIENT-SAFE
// ============================================================================
//
// It imports `./keys` and `./catalog.en` and nothing else. No `process.env`, no
// `server-only`, no credential-carrying module. `components/atoms/Spinner.tsx`
// and `components/atoms/Label.tsx` import it, which is legal under
// `architecture.test.ts` (`classifyLayer` returns "lib", and atoms' forbidden
// targets are molecules/modules/app) — but note that
// `AUTH_SPECIFIER_PATTERN = /(^|[/-])auth([/-]|$)|better-auth|next-auth/i` is
// tested against every specifier BEFORE local resolution, so an atom-facing key
// must never live in a file whose path contains an `auth` segment. That is why
// this stays at `lib/i18n/` and not, say, `lib/auth-copy/`.

import { CONSOLE_CATALOG, type CatalogEntry } from "./catalog.en";
import { isConsoleMessageKey, type ConsoleMessageKey } from "./keys";

export { CONSOLE_CATALOG, type CatalogEntry } from "./catalog.en";
export {
  ALL_CONSOLE_MESSAGE_KEYS,
  CONSOLE_MESSAGE_KEYS,
  isConsoleMessageKey,
  type ConsoleMessageKey,
  type ConsoleMessageKeyName,
} from "./keys";

/**
 * Arguments for placeholder substitution.
 *
 * Deliberately as wide as Moira's `message_args` (`JsonValue`), because that is
 * what arrives on the wire. Only string/number/boolean members are substituted;
 * anything else is left as its placeholder rather than rendered as
 * `[object Object]`.
 */
export type MessageArgs = unknown;

/** Is this key one the console's catalog knows? */
export function hasKey(key: string): key is ConsoleMessageKey {
  return isConsoleMessageKey(key) && CONSOLE_CATALOG[key] !== undefined;
}

/** The catalog entry for a key, or `undefined`. */
export function entryFor(key: string): CatalogEntry | undefined {
  return isConsoleMessageKey(key) ? CONSOLE_CATALOG[key] : undefined;
}

/**
 * The catalog's English for a key, uninterpolated, or `undefined`.
 *
 * The PUBLIC lookup — `t()` resolves through this, and the coverage test asserts
 * `defaultMessageFor(key) === entry.message` so that a private shortcut cannot
 * make the two disagree. Mirrors `default_message_for_key`
 * (`src/i18n/catalog/mod.rs:404-434`).
 */
export function defaultMessageFor(key: string): string | undefined {
  return entryFor(key)?.message;
}

const PLACEHOLDER = /\{([a-zA-Z0-9_]+)\}/g;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Substitute `{name}` tokens from `args`.
 *
 * An unmatched placeholder is left intact on purpose. The alternatives are worse:
 * dropping it silently loses information the message was written to carry, and
 * substituting `undefined` renders the word "undefined" at an operator.
 */
export function interpolate(message: string, args: MessageArgs): string {
  if (!isRecord(args)) return message;
  return message.replace(PLACEHOLDER, (whole, name: string) => {
    const value = args[name];
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    return whole;
  });
}

/**
 * Resolve a message key to displayable text.
 *
 * @param key      a `console.*` key, or a Moira `moira.*` key the console does
 *                 not catalogue (in which case `fallback` carries the copy).
 * @param args     structured arguments; interpolated into the CATALOG message only.
 * @param fallback the server-supplied `message`, already interpolated by Moira.
 */
export function t(key: string, args?: MessageArgs, fallback?: string): string {
  const message = defaultMessageFor(key);
  if (message !== undefined) return interpolate(message, args);
  if (fallback !== undefined && fallback !== "") return fallback;
  return key;
}
