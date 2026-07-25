import { forwardRef, type InputHTMLAttributes } from "react";
import styles from "./Input.module.css";

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "size"> {
  /** Marks the field as invalid; sets `aria-invalid`. Styling only —
   * pairing with a visible error message is the caller's/molecule's job. */
  invalid?: boolean;
}

/**
 * Primitive, feature-agnostic text input. Presentational only: it is a
 * controlled or uncontrolled native `<input>` wrapper with no validation
 * logic, no API calls, and no assumptions about the surrounding form.
 */
export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { invalid = false, className, ...rest },
  ref,
) {
  const classes = [styles.input, invalid ? styles.invalid : undefined, className]
    .filter(Boolean)
    .join(" ");

  return (
    <input ref={ref} className={classes} aria-invalid={invalid ? true : undefined} {...rest} />
  );
});
