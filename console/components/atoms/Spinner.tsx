import type { HTMLAttributes } from "react";
import styles from "./Spinner.module.css";

export type SpinnerSize = "sm" | "md" | "lg";

export interface SpinnerProps extends HTMLAttributes<HTMLSpanElement> {
  /** Sizing. Defaults to "md". */
  size?: SpinnerSize;
  /**
   * Accessible label announced to screen readers while the spinner is
   * visible. Defaults to "Loading". The label is visually hidden — the
   * spinner itself is a decorative animation.
   */
  label?: string;
}

/**
 * Primitive, feature-agnostic loading indicator. Announces itself via
 * `role="status"` + `aria-live="polite"`. The accessible name comes from
 * `aria-label` — per the ARIA spec, `status` is "name from author", not
 * "name from content", so a visually-hidden text node alone would not be
 * exposed as the element's name to assistive tech (it would only be
 * picked up as the live-region announcement on subsequent updates). A
 * matching visually-hidden text node is kept too, so the label is also
 * legible to a screen reader landing directly inside the region.
 */
export function Spinner({ size = "md", label = "Loading", className, ...rest }: SpinnerProps) {
  const classes = [styles.spinner, styles[size], className].filter(Boolean).join(" ");

  return (
    <span role="status" aria-live="polite" aria-label={label} className={styles.wrapper} {...rest}>
      <span className={classes} aria-hidden="true" />
      <span className={styles.visuallyHidden}>{label}</span>
    </span>
  );
}
