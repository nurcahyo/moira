import { forwardRef, type TextareaHTMLAttributes } from "react";
import styles from "./Textarea.module.css";

export interface TextareaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  /** Marks the field as invalid; sets `aria-invalid`. Styling only —
   * pairing with a visible error message is the caller's/molecule's job. */
  invalid?: boolean;
}

/**
 * Primitive, feature-agnostic multi-line text input. Presentational only: it
 * is a controlled or uncontrolled native `<textarea>` wrapper with no
 * validation logic, no API calls, and no assumptions about the surrounding
 * form — the multi-line sibling of `Input`.
 */
export const Textarea = forwardRef<HTMLTextAreaElement, TextareaProps>(function Textarea(
  { invalid = false, className, rows = 6, ...rest },
  ref,
) {
  const classes = [styles.textarea, invalid ? styles.invalid : undefined, className]
    .filter(Boolean)
    .join(" ");

  return (
    <textarea
      ref={ref}
      className={classes}
      rows={rows}
      aria-invalid={invalid ? true : undefined}
      {...rest}
    />
  );
});
