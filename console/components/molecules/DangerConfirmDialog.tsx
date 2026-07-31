"use client";

// Confirm an irreversible action.
//
// ============================================================================
// IT COMPOSES `Dialog`, IT DOES NOT REIMPLEMENT IT
// ============================================================================
//
// The `Dialog` atom is a native `<dialog>` driven through `showModal()`, which
// is where the top layer, `aria-modal`, FOCUS CONTAINMENT, `inert` on the rest
// of the document, and Escape-to-close all come from. Every one of those is a
// thing a hand-rolled overlay gets subtly wrong, and three are invisible to
// sighted mouse testing. This molecule adds the two things that are specific to
// a destructive confirmation and nothing else:
//
//   * a `role="alertdialog"`-shaped announcement — the body carries
//     `role="alert"`, so the consequence is read out rather than left for the
//     user to find;
//   * a `danger` confirm button beside a plain cancel, with CANCEL FIRST IN DOM
//     ORDER. That is not a layout preference: `showModal()` focuses the first
//     tabbable descendant, so whichever control comes first is the one a
//     reflexive Enter presses. Putting the destructive control there would make
//     "open the dialog and press Enter to get rid of it" perform the action.
//     Achieved by ordering rather than by a `ref` and a focus call, because the
//     `Button` atom forwards no ref and widening its props to accept one for a
//     behaviour the platform already provides would be the wrong trade.
//
// It deliberately does NOT dismiss on backdrop click: `Dialog` does not offer
// that, for the same reason it does not here — the actions this confirms are
// irreversible.
//
// ============================================================================
// EVERY STRING IS THE CALLER'S, AND EVERY STRING IS A CATALOG KEY
// ============================================================================
//
// `title`, `body` and `confirmLabel` are resolved copy passed in, because the
// molecule is feature-agnostic — it has no idea whether it is confirming a
// transfer, a revocation or a withdrawal. `no-hardcoded-copy.test.tsx` forbids
// literal `aria-label` and default-parameter copy, so the one string this file
// owns (`Cancel`) comes from `console.action.cancel`.

import { Button } from "../atoms/Button";
import { Dialog } from "../atoms/Dialog";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

import styles from "./DangerConfirmDialog.module.css";

export interface DangerConfirmDialogProps {
  readonly open: boolean;
  /** Accessible name and heading. Already resolved through `t()`. */
  readonly title: string;
  /** The consequence, in one sentence. Announced via `role="alert"`. */
  readonly body: string;
  /** Label of the destructive control. */
  readonly confirmLabel: string;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
  /** Disables both controls and marks the confirm one busy. */
  readonly busy?: boolean;
}

export function DangerConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  onConfirm,
  onCancel,
  busy = false,
}: DangerConfirmDialogProps) {
  return (
    <Dialog open={open} label={title} onDismiss={onCancel}>
      <h3 className={styles.title}>{title}</h3>
      <p className={styles.body} role="alert">
        {body}
      </p>
      <div className={styles.actions}>
        <Button type="button" variant="secondary" disabled={busy} onClick={onCancel}>
          {t(CONSOLE_MESSAGE_KEYS.action_cancel)}
        </Button>
        <Button type="button" variant="danger" loading={busy} onClick={onConfirm}>
          {confirmLabel}
        </Button>
      </div>
    </Dialog>
  );
}
