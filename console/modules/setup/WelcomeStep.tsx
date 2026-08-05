"use client";

// The setup wizard's informational first step.
//
// Explains the two facts an operator must hold before touching anything:
// the claim is once-only, and the provider must be configured FIRST because the
// admission policy is deny-by-default with no first-claim exemption
// (`lib/setup-flow.ts`'s header is the long version). It performs no request
// and holds no state — its one control advances the wizard's cursor.

import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";

import styles from "./WelcomeStep.module.css";

export interface WelcomeStepProps {
  readonly onContinue: () => void;
}

export function WelcomeStep({ onContinue }: WelcomeStepProps) {
  return (
    <section className={styles.step} aria-label={t(CONSOLE_MESSAGE_KEYS.setup_welcome_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.setup_welcome_heading)}</h2>
      <p className={styles.body}>{t(CONSOLE_MESSAGE_KEYS.setup_welcome_claim_once)}</p>
      <p className={styles.body}>{t(CONSOLE_MESSAGE_KEYS.setup_welcome_provider_first)}</p>
      <Button type="button" variant="primary" onClick={onContinue}>
        {t(CONSOLE_MESSAGE_KEYS.setup_welcome_continue)}
      </Button>
    </section>
  );
}
