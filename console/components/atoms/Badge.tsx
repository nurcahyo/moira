import type { HTMLAttributes, ReactNode } from "react";
import styles from "./Badge.module.css";

export type BadgeTone = "neutral" | "info" | "success" | "warning" | "danger";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  /** Color tone. Defaults to "neutral". Tone is always paired with the
   * required text `children` — color is never the only signal. */
  tone?: BadgeTone;
  /**
   * Marks the badge as an assertive live status update (e.g. a value that
   * changes in place, like "Saving..." -> "Saved"), announced to screen
   * readers via `role="status"`. Leave false for a static label.
   */
  live?: boolean;
  children: ReactNode;
}

/**
 * Primitive, feature-agnostic status/tag badge. Always renders text
 * content — never a color swatch alone.
 */
export function Badge({
  tone = "neutral",
  live = false,
  className,
  children,
  ...rest
}: BadgeProps) {
  const classes = [styles.badge, styles[tone], className].filter(Boolean).join(" ");

  return (
    <span className={classes} role={live ? "status" : undefined} {...rest}>
      {children}
    </span>
  );
}
