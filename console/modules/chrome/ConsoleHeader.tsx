"use client";

// The authenticated console's header: where you are, where you can go, and out.
//
// ============================================================================
// WHY THIS ARRIVES NOW AND NOT IN WAVE 3
// ============================================================================
//
// `app/(console)/layout.tsx` shipped the session BOUNDARY and deliberately no
// chrome, saying so in its own header: *"navigation, a header and a sign-out
// control arrive with the surfaces they navigate to."* This wave adds the first
// surface worth navigating to. Without this component `/admins` is reachable
// only by typing the URL.
//
// ============================================================================
// WHY IT IS AN ORGANISM AND NOT A MOLECULE
// ============================================================================
//
// It calls `fetch(`, which `architecture.test.ts` rules 4 forbids below the
// organism layer, and it navigates. `modules/` IS the organism directory;
// `components/organisms/` does not exist and must not be created.
//
// ============================================================================
// SIGN-OUT IS A REQUEST, NOT A LINK
// ============================================================================
//
// `POST /api/auth/sign-out` is Better Auth's own endpoint, mounted by
// `app/api/auth/[...all]/route.ts`. It must be a POST from a control, never an
// anchor: a GET sign-out is triggerable by any third-party page that can get the
// browser to load a URL, which is the classic logout-CSRF.
//
// The failure copy is deliberately client-side ("close this browser or clear its
// cookies") rather than "try again". If the request did not reach the server,
// the console cannot promise a retry will; what it can say is what actually ends
// the session locally.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { Spinner } from "@/components/atoms/Spinner";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

import styles from "./ConsoleHeader.module.css";

/** Better Auth's sign-out endpoint, under the mounted base path. */
const SIGN_OUT_ENDPOINT = "/api/auth/sign-out";

/** Where a completed sign-out lands. Relative on purpose — never absolute. */
const AFTER_SIGN_OUT = "/login";

/** One navigation destination. Resolved copy, never a literal. */
interface NavItem {
  readonly href: string;
  readonly labelKey: string;
}

const NAV_ITEMS: readonly NavItem[] = [
  { href: "/", labelKey: CONSOLE_MESSAGE_KEYS.chrome_nav_home },
  { href: "/admins", labelKey: CONSOLE_MESSAGE_KEYS.chrome_nav_admins },
];

export interface ConsoleHeaderProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites navigate for real. */
  readonly navigate?: (url: string) => void;
}

type Phase = "idle" | "pending" | "failed";

export function ConsoleHeader({ fetchImpl, navigate }: ConsoleHeaderProps) {
  const [phase, setPhase] = useState<Phase>("idle");

  async function signOut(): Promise<void> {
    setPhase("pending");
    const send = fetchImpl ?? globalThis.fetch;
    const go = navigate ?? ((url: string) => globalThis.location.assign(url));
    try {
      const response = await send(SIGN_OUT_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      });
      if (!response.ok) {
        // The body is not echoed: it is a Better Auth error object, not copy.
        setPhase("failed");
        return;
      }
    } catch {
      setPhase("failed");
      return;
    }
    go(AFTER_SIGN_OUT);
  }

  return (
    <header className={styles.header}>
      <nav className={styles.nav} aria-label={t(CONSOLE_MESSAGE_KEYS.chrome_nav_label)}>
        <ul className={styles.list}>
          {NAV_ITEMS.map((item) => (
            <li key={item.href}>
              <a className={styles.link} href={item.href}>
                {t(item.labelKey)}
              </a>
            </li>
          ))}
        </ul>
      </nav>

      <div className={styles.actions}>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          loading={phase === "pending"}
          onClick={() => {
            void signOut();
          }}
        >
          {t(CONSOLE_MESSAGE_KEYS.chrome_sign_out)}
        </Button>
        {phase === "pending" && <Spinner size="sm" label={t(CONSOLE_MESSAGE_KEYS.chrome_sign_out_pending)} />}
      </div>

      {phase === "failed" && (
        <p className={styles.problem} role="alert">
          {t(CONSOLE_MESSAGE_KEYS.chrome_sign_out_failed)}
        </p>
      )}
    </header>
  );
}
