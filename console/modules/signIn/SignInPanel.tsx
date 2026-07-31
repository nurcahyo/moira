"use client";

// The sign-in surface. THE FIRST `"use client"` FILE IN THIS REPOSITORY.
//
// ============================================================================
// WHY IT IS AN ORGANISM AND NOT A MOLECULE
// ============================================================================
//
// `architecture.test.ts:113,136,159` forbids `components/atoms/**` and
// `components/molecules/**` from importing anything matching
// `/(^|[/-])auth([/-]|$)|better-auth|next-auth/i`, from calling `fetch(`, and
// from importing `next/navigation`. This component does the second of those, so
// it cannot live below the organism layer. `modules/` IS the organism directory
// (`modules/README.md`); `components/organisms/` does not exist and must not be
// created.
//
// ============================================================================
// AT MOST ONE BUTTON, BY CONSTRUCTION
// ============================================================================
//
// `lib/auth-config.ts:189-198` returns `fail("ambiguous_enabled_providers")`
// whenever more than one provider is enabled, and `loadAuthConfig:260` will not
// even READ a secret in that case. A provider picker is therefore not "not built
// yet" — it is wrong in this wave, because a console that offers two buttons is
// a console whose configuration the server has already refused to resolve.
// Multi-provider is a later wave's decision, not this component's.
//
// ============================================================================
// WHY THE REFUSAL STATES ARE RESOLVED SERVER-SIDE AND PASSED IN
// ============================================================================
//
// `app/api/auth/[...all]/route.ts:61-66` returns 503 with a `message_key` and NO
// English on exactly the deployment `sign-in-methods` exists for: system key
// removed, snapshot cold. A page that renders a button purely from the anonymous
// endpoint shows a WORKING-LOOKING BUTTON THAT 503s ON CLICK.
//
// The anonymous projection (`PublicSignInMethod`) is enough to RENDER a button
// and not enough to RESOLVE the configuration behind one: it lacks `enabled`,
// `status`, `version`, `token_url`, `userinfo_url`, `allowed_email_domains` and
// `trusted_jwt_issuer_id`, and `resolveAuthConfig` refuses a row without the last
// two. So `/login` asks `consoleRuntime()` whether a sign-in can actually be
// resolved, and this component renders a button ONLY in the `ready` state. In
// every refusal state it renders zero buttons — asserted in
// `tests/unit/modules/SignInPanel.test.tsx`.
//
// ============================================================================
// NOT A SERVER ACTION
// ============================================================================
//
// `lib/auth.ts:308-328`: `nextCookies()` is deliberately absent from the plugin
// list, and with it installed the sign-in reply came back with NO `Set-Cookie`
// at all and the callback then failed `state_security_mismatch`. There is also
// no module-scope `auth` object to import — `getConsoleAuth` memoises per
// `config.cacheKey` and the instance is obtained inside the request handler.
//
// So this posts to the mounted route handler with `fetch`, exactly the wire
// format `tests/integration/oauth-flow.test.ts:110-116` drives:
//
//     POST /api/auth/sign-in/oauth2
//     content-type: application/json
//     { providerId, callbackURL: "/" }      ->  200 { url }
//
// ============================================================================
// WHAT THIS FILE MAY IMPORT
// ============================================================================
//
// `lib/errors.ts`, `lib/types.ts`, `lib/moira-keys.ts`, `lib/i18n/**`, and the
// atoms. Nothing else from `lib/` — every other module is credential-carrying or
// declares itself server-only, and
// `tests/unit/architecture/layer-dependencies.test.ts` enforces that against
// this file specifically now that it exists.
//
// There is also NO build-time channel to the browser here: `lib/env.ts:304-313`
// makes setting any `NEXT_PUBLIC_<console secret>` a hard boot failure, so a
// value cannot be smuggled in that way either.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Spinner } from "@/components/atoms/Spinner";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { JsonValue } from "@/lib/types";

import styles from "./SignInPanel.module.css";

/** Where Better Auth's sign-in endpoint is mounted. Mirrors `AUTH_BASE_PATH`. */
const SIGN_IN_ENDPOINT = "/api/auth/sign-in/oauth2";

/** Where a completed sign-in lands. Relative on purpose — never an absolute URL. */
const CALLBACK_PATH = "/";

/**
 * What the server resolved.
 *
 * `unavailable` carries a KEY, not prose. The 503 body from
 * `app/api/auth/[...all]/route.ts` has no `message` field at all, which is why
 * `message` is optional here: on that path there is nothing to degrade to except
 * the catalog, and the catalog is why this state renders English rather than a
 * bare key.
 */
export type SignInPanelState =
  | {
      readonly kind: "ready";
      /** Better Auth's `providerId`. Not the Moira row id. */
      readonly providerId: string;
      /** `display_name` from the anonymous projection, or null when unknown. */
      readonly displayName: string | null;
    }
  | {
      readonly kind: "unavailable";
      readonly messageKey: string;
      readonly message?: string;
      readonly messageArgs?: JsonValue;
    };

export interface SignInPanelProps {
  readonly state: SignInPanelState;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites navigate for real. */
  readonly navigate?: (url: string) => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly messageKey: string };

export function SignInPanel({ state, fetchImpl, navigate }: SignInPanelProps) {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  if (state.kind === "unavailable") {
    return (
      <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.sign_in_heading)}>
        <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.sign_in_unavailable_heading)}</h2>
        <p className={styles.problem} role="alert">
          {t(state.messageKey, state.messageArgs, state.message)}
        </p>
      </section>
    );
  }

  const label =
    state.displayName === null
      ? t(CONSOLE_MESSAGE_KEYS.sign_in_button_generic)
      : t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: state.displayName });

  async function start(): Promise<void> {
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;
    const go = navigate ?? ((url: string) => globalThis.location.assign(url));

    let response: Response;
    try {
      response = await send(SIGN_IN_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          providerId: (state as Extract<SignInPanelState, { kind: "ready" }>).providerId,
          callbackURL: CALLBACK_PATH,
        }),
      });
    } catch {
      // The thrown cause is deliberately not read: a fetch failure can carry a
      // URL with credentials in it, the same reason `toTransportError` refuses
      // `String(cause)`.
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }

    if (response.status === 429) {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_rate_limited });
      return;
    }
    if (!response.ok) {
      // The body is NOT echoed. It is a Better Auth error object on the happy
      // path and the console's own 503 `{error:{code,message_key}}` on the cold
      // path; neither is copy, and the second is already covered by the
      // server-resolved `unavailable` state.
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }

    let url: unknown;
    try {
      url = ((await response.json()) as { url?: unknown }).url;
    } catch {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_request_failed });
      return;
    }
    if (typeof url !== "string" || url === "") {
      setPhase({ kind: "failed", messageKey: CONSOLE_MESSAGE_KEYS.sign_in_no_redirect_url });
      return;
    }

    go(url);
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.sign_in_heading)}>
      <Button
        type="button"
        variant="primary"
        loading={phase.kind === "pending"}
        onClick={() => {
          void start();
        }}
      >
        {label}
      </Button>
      {phase.kind === "pending" && <Spinner label={t(CONSOLE_MESSAGE_KEYS.sign_in_pending)} />}
      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey)}
        </p>
      )}
    </section>
  );
}
