// The console mirrors Moira's i18n keys. This is what makes a Moira-side rename
// a console test failure instead of a bare key rendered at an operator.

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import {
  MIRRORED_MOIRA_KEYS,
  MOIRA_SETUP_ERROR_CODES,
  moiraErrorKey,
  moiraNoticeKey,
} from "@/lib/moira-keys";

const CATALOG_PATH = resolve(import.meta.dir, "../../../../docs/i18n-response-catalog.json");

interface Catalog {
  namespace: string;
  default_locale: string;
  entries: Array<{ key: string; default_message: string }>;
}

const catalog = JSON.parse(readFileSync(CATALOG_PATH, "utf8")) as Catalog;
const catalogKeys = new Set(catalog.entries.map((entry) => entry.key));

describe("every mirrored key exists in docs/i18n-response-catalog.json", () => {
  test("no mirrored key is missing from the catalog", () => {
    const missing = MIRRORED_MOIRA_KEYS.filter((key) => !catalogKeys.has(key));
    expect(missing).toEqual([]);
  });

  test("every catalog entry the mirror names has a non-empty English default", () => {
    const empty = catalog.entries
      .filter((entry) => MIRRORED_MOIRA_KEYS.includes(entry.key))
      .filter((entry) => entry.default_message.trim() === "")
      .map((entry) => entry.key);
    expect(empty).toEqual([]);
  });

  test("the eight codes §0 says the plan never mapped are all mirrored", () => {
    const eight = [
      "setup_token_not_supported",
      "setup_claim_credential_required",
      "admin_claim_email_required",
      "admin_claim_email_not_verified",
      "scope_invalid",
      "console_issuer_must_not_assert_scopes",
      "auth_provider_method_config_incomplete",
      "auth_provider_url_not_allowed",
    ];
    for (const code of eight) {
      expect(MOIRA_SETUP_ERROR_CODES).toContain(code as never);
      expect(catalogKeys.has(moiraErrorKey(code))).toBe(true);
    }
  });

  test("no key is mirrored twice", () => {
    expect(new Set(MIRRORED_MOIRA_KEYS).size).toBe(MIRRORED_MOIRA_KEYS.length);
  });

  test("key helpers produce the catalog's namespace", () => {
    expect(catalog.namespace).toBe("moira");
    expect(moiraErrorKey("forbidden")).toBe("moira.error.forbidden");
    expect(moiraNoticeKey("admin_identity_claimed")).toBe("moira.notice.admin_identity_claimed");
  });

  test("no English copy is duplicated into the console mirror", () => {
    // The mirror lists keys only; copy lives in the catalog (server fallback) or
    // in the console's own catalog. Duplicating it here is how the two drift.
    const source = readFileSync(resolve(import.meta.dir, "../../../lib/moira-keys.ts"), "utf8");
    for (const entry of catalog.entries) {
      if (!MIRRORED_MOIRA_KEYS.includes(entry.key)) continue;
      expect(source).not.toContain(entry.default_message);
    }
  });
});
