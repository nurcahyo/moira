"use client";

// Primitive modal dialog, built on the native `<dialog>` element.
//
// ============================================================================
// WHY NATIVE `<dialog>` AND NOT A HAND-ROLLED OVERLAY
// ============================================================================
//
// `showModal()` gives, from the platform and therefore correctly:
//
//   * the top layer, so no `z-index` arithmetic and no stacking-context bug;
//   * `aria-modal` semantics and the implicit `dialog` role;
//   * FOCUS CONTAINMENT — Tab cannot leave the dialog;
//   * `inert` on everything behind it, so a screen reader cannot wander out;
//   * Escape-to-close, wired to `cancel`/`close`.
//
// Every one of those is a thing a hand-rolled overlay gets subtly wrong, and
// three of them are invisible to sighted mouse testing. This is the first
// component in the console with a focus trap; getting it from the platform is
// the only version worth shipping at this size.
//
// ============================================================================
// WHAT IT DELIBERATELY DOES NOT DO
// ============================================================================
//
// It holds no secret and knows nothing about one. `components/atoms/**` is
// scanned by `tests/unit/architecture/no-secret-props.test.ts`, and this atom's
// props are `open`, `label`, `onDismiss`, `children` — nothing that could carry
// a credential, by name or by shape.
//
// It also does NOT close on backdrop click. A once-only secret is the first
// thing rendered inside it, and dismissing that by a stray click is
// irreversible: the token cannot be shown again.

import { useEffect, useRef, type DialogHTMLAttributes, type ReactNode } from "react";

import styles from "./Dialog.module.css";

export interface DialogProps extends Omit<
  DialogHTMLAttributes<HTMLDialogElement>,
  "open" | "onCancel"
> {
  /** Whether the dialog is shown. Driven by the caller, never internally. */
  open: boolean;
  /** Accessible name. Required — an unnamed dialog is announced as "dialog". */
  label: string;
  /**
   * Called when the user asks to close: Escape, or the platform's own close.
   * Omit to make the dialog non-dismissible except through its own controls.
   */
  onDismiss?: () => void;
  children: ReactNode;
}

export function Dialog({ open, label, onDismiss, className, children, ...rest }: DialogProps) {
  const ref = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const node = ref.current;
    if (node === null) return;
    if (open) {
      // `showModal` is what produces the top layer, the focus trap and `inert`
      // on the rest of the document. `happy-dom` does not implement it, so the
      // fallback keeps the component renderable under `bun test` — the attribute
      // alone gives the right DOM shape for assertions, without the platform
      // behaviour the test is not asserting.
      if (typeof node.showModal === "function" && !node.open) node.showModal();
      else if (!node.hasAttribute("open")) node.setAttribute("open", "");
    } else if (typeof node.close === "function" && node.open) {
      node.close();
    } else {
      node.removeAttribute("open");
    }
  }, [open]);

  return (
    <dialog
      ref={ref}
      aria-label={label}
      className={[styles.dialog, className].filter(Boolean).join(" ")}
      onCancel={(event) => {
        // Escape fires `cancel`. With no `onDismiss` the dialog is deliberately
        // not dismissible that way — see the header.
        if (onDismiss === undefined) {
          event.preventDefault();
          return;
        }
        event.preventDefault();
        onDismiss();
      }}
      {...rest}
    >
      <div className={styles.body}>{children}</div>
    </dialog>
  );
}
