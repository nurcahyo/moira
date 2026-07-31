"use client";

// Copy a value to the clipboard.
//
// ============================================================================
// IT TAKES AN ELEMENT ID, NOT THE VALUE — AND THAT IS THE WHOLE POINT
// ============================================================================
//
// The obvious API is `<CopyButton value={secret} />`. It is also the exact thing
// `plans/CONVENTIONS.md:159` §6 rule 5 forbids and that
// `tests/unit/architecture/no-secret-props.test.ts` rule (b) scans for: the
// secret would become a member of a REUSABLE component's props object, and from
// there it is one careless `console.log`, one error boundary, or one dev-tools
// screenshot away from being somewhere nobody meant.
//
// So this atom reads the text out of the DOM node it is pointed at, at click
// time. The consequence is the property plan 09:324 asks for and that nothing
// previously expressed: the once-only token exists in exactly ONE place in
// JavaScript — the render of `modules/secrets/OnceOnlySecretModal.tsx` — and in
// exactly one place in the DOM, the element it is displayed in. Copying it adds
// no third place.
//
// It also means this atom is genuinely feature-agnostic: it has no idea whether
// it is copying a token, a URL, or an id, which is why its labels live under
// `console.action.*` rather than `console.secret.*`.
//
// ============================================================================
// FAILURE IS A DEGRADATION, NOT AN ERROR
// ============================================================================
//
// `navigator.clipboard` requires a secure context and can be blocked by
// permissions policy, so `writeText` genuinely rejects in real deployments. The
// value is still on screen when it does — the user can select it — so the
// failure message says that rather than presenting itself as a fault.

import { useEffect, useRef, useState, type ButtonHTMLAttributes } from "react";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

import styles from "./CopyButton.module.css";

/** How long the "Copied" confirmation stays up. */
const CONFIRMATION_MS = 2000;

export interface CopyButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type"> {
  /**
   * `id` of the element whose `textContent` is copied.
   *
   * Deliberately not the value itself — see the header.
   */
  targetId: string;
}

type Phase = "idle" | "copied" | "failed";

export function CopyButton({ targetId, className, ...rest }: CopyButtonProps) {
  const [phase, setPhase] = useState<Phase>("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, []);

  async function copy(): Promise<void> {
    const node = typeof document === "undefined" ? null : document.getElementById(targetId);
    const text = node?.textContent ?? "";
    try {
      if (text === "") throw new Error("nothing to copy");
      await navigator.clipboard.writeText(text);
      setPhase("copied");
    } catch {
      // The rejection reason is not read: it is a DOMException whose message
      // varies by browser and says nothing an operator can act on.
      setPhase("failed");
    }
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => setPhase("idle"), CONFIRMATION_MS);
  }

  const label =
    phase === "copied"
      ? t(CONSOLE_MESSAGE_KEYS.action_copied)
      : t(CONSOLE_MESSAGE_KEYS.action_copy);

  return (
    <span className={styles.wrapper}>
      <button
        type="button"
        className={[styles.button, className].filter(Boolean).join(" ")}
        onClick={() => {
          void copy();
        }}
        {...rest}
      >
        {label}
      </button>
      {/* Polite, so the confirmation is announced without interrupting, and
          always present so the region is not created at announce time — a live
          region added and populated in the same tick is frequently missed. */}
      <span role="status" aria-live="polite" className={styles.visuallyHidden}>
        {phase === "copied" ? t(CONSOLE_MESSAGE_KEYS.action_copied) : ""}
        {phase === "failed" ? t(CONSOLE_MESSAGE_KEYS.action_copy_failed) : ""}
      </span>
    </span>
  );
}
