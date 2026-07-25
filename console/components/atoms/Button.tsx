import type { ButtonHTMLAttributes, ReactNode } from "react";
import styles from "./Button.module.css";

export type ButtonVariant = "primary" | "secondary" | "danger" | "ghost";
export type ButtonSize = "sm" | "md";

export interface ButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type"> {
  /** Visual style. Defaults to "primary". */
  variant?: ButtonVariant;
  /** Sizing. Defaults to "md". */
  size?: ButtonSize;
  /**
   * Marks the button as busy (e.g. a pending submission). Sets
   * `aria-busy` and disables the button so it cannot be re-triggered.
   */
  loading?: boolean;
  /** Native button `type`. Defaults to "button" so it never
   * accidentally submits a form it happens to be rendered inside. */
  type?: "button" | "submit" | "reset";
  children: ReactNode;
}

/**
 * Primitive, feature-agnostic button. Presentational only: it takes props
 * in and calls `onClick` (or relies on native form submission) — it never
 * calls an API or navigates.
 */
export function Button({
  variant = "primary",
  size = "md",
  loading = false,
  disabled = false,
  type = "button",
  className,
  children,
  ...rest
}: ButtonProps) {
  const classes = [styles.button, styles[variant], styles[size], className]
    .filter(Boolean)
    .join(" ");

  return (
    <button
      type={type}
      className={classes}
      disabled={disabled || loading}
      aria-busy={loading ? true : undefined}
      {...rest}
    >
      {children}
    </button>
  );
}
