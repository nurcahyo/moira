// `/setup` — the first-run wizard. Thin on purpose: guard -> fetch -> render.
//
// ============================================================================
// THE VIEW MODEL COMES FROM `GET /api/setup`, SERVER-SIDE (decision D4)
// ============================================================================
//
// The page calls the BFF route handler IN PROCESS — same module, same guard,
// same display-safe projection — rather than re-implementing the aggregation or
// round-tripping over HTTP to its own origin. Moira's raw auth-methods response
// therefore never crosses to the browser: what reaches the client component is
// the narrowed `SetupViewModel` (counts and presence flags), exactly what the
// route would have served the browser anyway.
//
// ============================================================================
// IT MUST ANSWER < 400 IN EVERY WINDOW STATE
// ============================================================================
//
// `/setup` sits outside `(console)`, so `e2e/a11y.e2e.ts` audits it directly
// and fails the gate on any status >= 400. The window being closed is a
// CONFIGURATION FACT, not an error: no bootstrap key renders the keyed
// "unavailable" state, a claimed deployment renders the done step, and a Moira
// outage renders the client-safe keyed failure — all as 200 pages.
//
// `force-dynamic` because the answer depends on Moira's claim-status on every
// request, and a prerendered setup page would freeze the one gate that must
// never be cached.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AuthMethod, JsonValue } from "@/lib/types";
import { SetupWizard } from "@/modules/setup/SetupWizard";
import type { SetupMethodSummary, SetupViewModel } from "@/modules/setup/setup-view";

import { GET as readSetupWindow } from "../api/setup/route";
import styles from "./page.module.css";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** One `SetupMethodView` row from the BFF, narrowed field by field. */
function methodSummary(value: unknown): SetupMethodSummary | null {
  const record = asRecord(value);
  if (record === null) return null;
  const id = record["id"];
  const method = record["method"];
  const displayName = record["display_name"];
  if (typeof id !== "string" || typeof method !== "string" || typeof displayName !== "string") {
    return null;
  }
  const domainCount = record["allowed_email_domain_count"];
  return {
    id,
    method: method as AuthMethod,
    displayName,
    interactive: record["interactive"] === true,
    clientIdConfigured: record["has_client_id"] === true,
    discoveryUrlConfigured: record["has_discovery_url"] === true,
    allowedEmailDomainCount: typeof domainCount === "number" ? domainCount : 0,
  };
}

async function loadViewModel(): Promise<SetupViewModel> {
  const response = await readSetupWindow();
  let payload: Record<string, unknown> | null = null;
  try {
    payload = asRecord(await response.json());
  } catch {
    payload = null;
  }

  if (response.ok && payload !== null) {
    const methodsRaw = payload["methods"];
    const methods = Array.isArray(methodsRaw)
      ? methodsRaw
          .map(methodSummary)
          .filter((summary): summary is SetupMethodSummary => summary !== null)
      : [];
    return { kind: "ready", claimed: payload["claimed"] === true, methods };
  }

  const error = asRecord(payload?.["error"]);
  if (error?.["code"] === "setup_already_claimed") return { kind: "claimed" };

  // A keyed console refusal carries `message_key`; a passed-through Moira
  // failure is the client-safe union with its key under `text`.
  const text = asRecord(error?.["text"]);
  const messageKey = error?.["message_key"] ?? text?.["messageKey"];
  const message = text?.["message"];
  const messageArgs = error?.["message_args"] ?? text?.["messageArgs"];
  return {
    kind: "unavailable",
    messageKey:
      typeof messageKey === "string"
        ? messageKey
        : CONSOLE_MESSAGE_KEYS.setup_system_key_absent,
    ...(typeof message === "string" ? { message } : {}),
    ...(messageArgs === undefined || messageArgs === null
      ? {}
      : { messageArgs: messageArgs as JsonValue }),
  };
}

export default async function SetupPage() {
  const view = await loadViewModel();

  return (
    <main className={styles.main}>
      <h1 className={styles.title}>{t(CONSOLE_MESSAGE_KEYS.setup_page_title)}</h1>
      <SetupWizard view={view} />
    </main>
  );
}
