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
// ONE BUTTON PER RESOLVABLE PROVIDER, AND NEVER MORE (was: AT MOST ONE BUTTON,
// BY CONSTRUCTION)
// ============================================================================
//
// This header used to say a provider picker was *wrong*, because
// `resolveAuthConfig` refused any deployment with more than one enabled row and
// would not even read a secret in that case — so two buttons meant a
// configuration the server had already refused to resolve. Wave 4B changed the
// premise underneath it, and the reason it is REWRITTEN rather than deleted is
// that the invariant it protected is still real; only its arithmetic moved.
//
// The invariant, restated for N: **this component renders a button only for a
// provider the server has fully RESOLVED**, never for one it merely knows about.
// `SignInPanelState.ready.providers` comes from `consoleRuntime()`'s `configs`,
// which is the list of providers that have a client secret in the console's own
// store, a non-empty `allowed_email_domains`, usable endpoints, and a bound
// trusted JWT issuer whose string this console can derive a `providerId` from.
// An enabled Moira row missing any of those is reported as a PROBLEM and gets no
// button — which is exactly what the old "zero buttons in a refusal state" rule
// said, now applied per provider instead of per deployment.
//
// What is NOT switched on yet: `lib/auth-config.ts::ambiguityGuard` still
// refuses a deployment with more than one enabled row, until wave 4A's
// server-side invariant is deployed rather than merely merged. So on today's
// deployments this still renders one button. The component is the part that does
// not have to change again when that guard goes.
//
// ============================================================================
// WHY THE REFUSAL STATES ARE RESOLVED SERVER-SIDE AND PASSED IN
// ============================================================================
//
// `app/api/auth/[...all]/route.ts` returns 503 with a `message_key` and NO
// English on exactly the deployment `sign-in-methods` exists for: system key
// removed, snapshot cold. A page that renders a button purely from the anonymous
// endpoint shows a WORKING-LOOKING BUTTON THAT 503s ON CLICK.
//
// The anonymous projection (`PublicSignInMethod`) is enough to RENDER a button
// and not enough to RESOLVE the configuration behind one: it lacks `enabled`,
// `status`, `version`, `token_url`, `userinfo_url`, `allowed_email_domains` and
// `trusted_jwt_issuer_id`, and `resolveAuthConfigs` refuses a row without the
// last two. So `/login` asks `consoleRuntime()` which sign-ins can actually be
// resolved, and this component renders buttons ONLY from that list. In every
// refusal state it renders zero buttons — asserted in
// `tests/unit/modules/SignInPanel.test.tsx`.
//
// ============================================================================
// NOT A SERVER ACTION
// ============================================================================
//
// `lib/auth.ts:308-328`: `nextCookies()` is deliberately absent from the plugin
// list, and with it installed the sign-in reply came back with NO `Set-Cookie`
// at all and the callback then failed `state_security_mismatch`. There is also
// no module-scope `auth` object to import — `lib/auth-runtime.ts` memoises per
// resolution `cacheKey` and the instance is obtained inside the request handler.
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
/** One resolvable provider, as the server offered it. */
export interface SignInPanelProvider {
  /** Better Auth's `providerId`. Not the Moira row id. */
  readonly providerId: string;
  /** `display_name` from the anonymous projection, or null when unknown. */
  readonly displayName: string | null;
}

export type SignInPanelState =
  | {
      readonly kind: "ready";
      /**
       * Every provider the server RESOLVED. Never empty — a resolution with no
       * usable provider is an `unavailable` state, not a `ready` one with an
       * empty list, because an empty list renders a panel with no way out of it.
       */
      readonly providers: readonly SignInPanelProvider[];
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
  | { readonly kind: "pending"; readonly providerId: string }
  | { readonly kind: "failed"; readonly messageKey: string };

/**
 * The accessible name for one button.
 *
 * With several providers a missing display name cannot fall back to the generic
 * label: every button would then have the SAME accessible name, and a screen
 * reader user would be choosing between identical controls. The provider id is
 * not pretty, but it is distinct, and the operator can set `display_name` in
 * Moira to replace it.
 */
function providerLabel(provider: SignInPanelProvider, total: number): string {
  if (provider.displayName !== null) {
    return t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: provider.displayName });
  }
  if (total === 1) return t(CONSOLE_MESSAGE_KEYS.sign_in_button_generic);
  return t(CONSOLE_MESSAGE_KEYS.sign_in_button, { provider: provider.providerId });
}

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

  const providers = state.providers;

  async function start(providerId: string): Promise<void> {
    setPhase({ kind: "pending", providerId });
    const send = fetchImpl ?? globalThis.fetch;
    const go = navigate ?? ((url: string) => globalThis.location.assign(url));

    let response: Response;
    try {
      response = await send(SIGN_IN_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ providerId, callbackURL: CALLBACK_PATH }),
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
      {providers.map((provider) => (
        <Button
          key={provider.providerId}
          type="button"
          variant="primary"
          // Only the button that was pressed shows a spinner. `loading` on all
          // of them would read as "every provider is being contacted".
          loading={phase.kind === "pending" && phase.providerId === provider.providerId}
          disabled={phase.kind === "pending"}
          onClick={() => {
            void start(provider.providerId);
          }}
        >
          {providerLabel(provider, providers.length)}
        </Button>
      ))}
      {phase.kind === "pending" && <Spinner label={t(CONSOLE_MESSAGE_KEYS.sign_in_pending)} />}
      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey)}
        </p>
      )}
    </section>
  );
}
