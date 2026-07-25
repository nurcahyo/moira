import { useId } from "react";
import { Input, type InputProps } from "../atoms/Input";
import { Label } from "../atoms/Label";
import styles from "./FormField.module.css";

export interface FormFieldProps {
  /** Optional explicit id; a stable one is generated via `useId` when
   * omitted, so callers never need to invent ids themselves. */
  id?: string;
  label: string;
  hint?: string;
  /** Presence of `error` marks the field invalid and switches the
   * describing text from `hint` to this message, announced via
   * `role="alert"`. */
  error?: string;
  required?: boolean;
  inputProps?: Omit<InputProps, "id" | "required" | "invalid" | "aria-describedby">;
}

/**
 * Molecule composed from the Label and Input atoms: a labeled field with
 * optional hint/error text, correctly wired via `aria-describedby` and
 * `aria-invalid`. Feature-agnostic and presentational — no validation
 * logic, no API calls; the caller decides what `error` (if any) to pass.
 */
export function FormField({
  id,
  label,
  hint,
  error,
  required = false,
  inputProps,
}: FormFieldProps) {
  const generatedId = useId();
  const fieldId = id ?? generatedId;
  const hintId = hint ? `${fieldId}-hint` : undefined;
  const errorId = error ? `${fieldId}-error` : undefined;
  const describedBy = [hintId, errorId].filter(Boolean).join(" ") || undefined;

  return (
    <div className={styles.field}>
      <Label htmlFor={fieldId} required={required}>
        {label}
      </Label>
      <Input
        {...inputProps}
        id={fieldId}
        required={required}
        invalid={Boolean(error)}
        aria-describedby={describedBy}
      />
      {hint && !error && (
        <p id={hintId} className={styles.hint}>
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} className={styles.error} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
