"use client";

// The wizard's terminal step.
//
// Two shapes reach it: a claim that just succeeded in THIS session (the email
// is known, from the claim response) and a revisit of a deployment Moira
// already reports claimed (no email is known, and the console must not guess —
// the existing `console.error.setup_already_claimed` copy says what to do).
//
// The link is a plain anchor to the authenticated home route. Organisms may
// not import `app/**`, and a claimed session needs nothing more than a
// navigation.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

import styles from "./DoneStep.module.css";

/** The authenticated home route the new administrator lands on. */
const CONSOLE_HOME_PATH = "/";

export interface DoneStepProps {
  /** The claimed identity's email, when the claim happened in this session. */
  readonly email: string | null;
}

export function DoneStep({ email }: DoneStepProps) {
  return (
    <section className={styles.step} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_done_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.setup_done_heading)}</h2>
      <p className={styles.body}>
        {email !== null
          ? t(CONSOLE_MESSAGE_KEYS.setup_done_admin_email, { email })
          : t(CONSOLE_MESSAGE_KEYS.setup_already_claimed)}
      </p>
      <a className={styles.donelink} href={CONSOLE_HOME_PATH}>
        {t(CONSOLE_MESSAGE_KEYS.setup_done_open_console)}
      </a>
    </section>
  );
}
