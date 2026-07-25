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
 * `role="status"` + `aria-live="polite"` with a visually-hidden text
 * label, so assistive technology gets the same information sighted users
 * get from the animation.
 */
export function Spinner({
  size = "md",
  label = "Loading",
  className,
  ...rest
}: SpinnerProps) {
  const classes = [styles.spinner, styles[size], className]
    .filter(Boolean)
    .join(" ");

  return (
    <span
      role="status"
      aria-live="polite"
      className={styles.wrapper}
      {...rest}
    >
      <span className={classes} aria-hidden="true" />
      <span className={styles.visuallyHidden}>{label}</span>
    </span>
  );
}
