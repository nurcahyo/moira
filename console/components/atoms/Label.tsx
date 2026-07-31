import type { LabelHTMLAttributes, ReactNode } from "react";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import styles from "./Label.module.css";

export interface LabelProps extends LabelHTMLAttributes<HTMLLabelElement> {
  /** The id of the form control this label describes. */
  htmlFor: string;
  /**
   * Shows a required indicator. The indicator itself carries a
   * screen-reader-only "(required)" suffix so the requirement isn't
   * conveyed by a bare asterisk glyph alone.
   */
  required?: boolean;
  children: ReactNode;
}

/**
 * Primitive, feature-agnostic form label. Always a native `<label>`
 * associated to its control via `htmlFor`, never a styled `<span>`.
 */
export function Label({ htmlFor, required = false, className, children, ...rest }: LabelProps) {
  const classes = [styles.label, className].filter(Boolean).join(" ");

  return (
    <label htmlFor={htmlFor} className={classes} {...rest}>
      {children}
      {required && (
        <span className={styles.required} aria-hidden="true">
          {" "}
          *
        </span>
      )}
      {/* The catalog English carries a LEADING SPACE — it is what separates the
          suffix from the label text in the computed accessible name
          ("Email * (required)"), which FormField.test.tsx pins. */}
      {required && (
        <span className={styles.visuallyHidden}>{t(CONSOLE_MESSAGE_KEYS.a11y_required)}</span>
      )}
    </label>
  );
}
