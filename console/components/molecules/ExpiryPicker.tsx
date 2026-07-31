// Choose how long an invitation stays usable.
//
// ============================================================================
// IT RESPECTS BOTH BOUNDS, AND THEY ARE DIFFERENT KINDS OF BOUND
// ============================================================================
//
// Moira's `validated_invite_lifetime` refuses outside `[60, 259200]` seconds:
//
//   ABOVE  `422 admin_invite_expiry_too_long` — a HARD CAP, refused rather than
//          clamped, because an operator who believes they issued a 30-day
//          invitation and silently received a 3-day one finds out at the worst
//          possible moment.
//   BELOW  `422 invalid_request` — a different code, so a UI that only knew
//          about the cap would render the floor's refusal as a generic
//          validation failure.
//
// Plan 09 names the cap and never mentions the floor. This molecule offers only
// values inside both, and asserts that property rather than assuming the option
// list was written correctly: `OPTION_SECONDS` is filtered through
// `isAcceptableInviteLifetime` at module scope, and the unit test asserts the
// filter removed nothing — a list that silently lost an option is a picker whose
// choices quietly shrank.
//
// ============================================================================
// WHY A `<select>` AND NOT A NUMBER INPUT
// ============================================================================
//
// A free number field makes an out-of-range value the operator's problem to
// discover, and there is no useful reason to issue an invitation valid for 4
// minutes. A closed set of durations is also the only shape in which "the
// console never sends a value Moira will refuse" is true by construction rather
// than by validation.
//
// It is presentational: no `fetch`, no auth import, no navigation. Molecules are
// scanned for all three (`architecture.test.ts`).

import { useId } from "react";

import { Label } from "../atoms/Label";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { MAX_INVITE_EXPIRY_SECONDS, MIN_INVITE_EXPIRY_SECONDS } from "@/lib/invite-bounds";

import styles from "./ExpiryPicker.module.css";

const HOUR_SECONDS = 60 * 60;

/**
 * The offered lifetimes, in seconds.
 *
 * Filtered rather than hand-checked. `expiryOptions()` returns only values
 * inside Moira's bounds, so an option added outside them disappears — and the
 * unit test asserts nothing disappeared, which is what turns a silent shrink
 * into a failure.
 */
const CANDIDATE_SECONDS = [
  1 * HOUR_SECONDS,
  4 * HOUR_SECONDS,
  12 * HOUR_SECONDS,
  24 * HOUR_SECONDS,
  48 * HOUR_SECONDS,
  72 * HOUR_SECONDS,
] as const;

/** Lifetimes this picker may offer: inside `[MIN, MAX]`, inclusive. */
export function expiryOptions(): number[] {
  return CANDIDATE_SECONDS.filter(
    (seconds) => seconds >= MIN_INVITE_EXPIRY_SECONDS && seconds <= MAX_INVITE_EXPIRY_SECONDS,
  );
}

/** Every candidate, offered or not. The test compares the two lists. */
export const EXPIRY_CANDIDATE_SECONDS: readonly number[] = CANDIDATE_SECONDS;

/** The catalog label for one lifetime. Singular has its own key. */
export function expiryOptionLabel(seconds: number): string {
  const hours = Math.round(seconds / HOUR_SECONDS);
  if (hours === 1) return t(CONSOLE_MESSAGE_KEYS.expiry_option_one_hour);
  return t(CONSOLE_MESSAGE_KEYS.expiry_option_hours, { hours });
}

export interface ExpiryPickerProps {
  /** Selected lifetime in seconds. Controlled by the caller. */
  readonly seconds: number;
  readonly onChange: (seconds: number) => void;
  readonly disabled?: boolean;
}

export function ExpiryPicker({ seconds, onChange, disabled = false }: ExpiryPickerProps) {
  const selectId = useId();
  const hintId = `${selectId}-hint`;
  const options = expiryOptions();

  return (
    <div className={styles.field}>
      <Label htmlFor={selectId} required>
        {t(CONSOLE_MESSAGE_KEYS.expiry_label)}
      </Label>
      <select
        id={selectId}
        className={styles.select}
        value={String(seconds)}
        disabled={disabled}
        aria-describedby={hintId}
        onChange={(event) => onChange(Number(event.target.value))}
      >
        {options.map((option) => (
          <option key={option} value={String(option)}>
            {expiryOptionLabel(option)}
          </option>
        ))}
      </select>
      <p id={hintId} className={styles.hint}>
        {t(CONSOLE_MESSAGE_KEYS.expiry_hint)}
      </p>
    </div>
  );
}
