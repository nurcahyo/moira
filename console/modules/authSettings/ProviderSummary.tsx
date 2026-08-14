"use client";

// What is configured now, including the one fact that explains a broken sign-in.
//
// ============================================================================
// THE SEALED CLIENT ID IS THE POINT OF THIS PANEL
// ============================================================================
//
// Moira holds the `client_id`; this console holds a secret SEALED AGAINST a
// client id. When those two disagree the deployment renders no sign-in button
// and says only "sign-in is not available" — a state that is otherwise
// indistinguishable from an outage. Showing both values, side by side, turns it
// into a sentence an operator can act on.
//
// `sealedAgainstClientId` is stored in the clear precisely so this is possible
// without decrypting anything. The secret itself is not here in any form.

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AuthSettingsView } from "@/lib/auth-settings-view";

import styles from "./ProviderSummary.module.css";

export interface ProviderSummaryProps {
  readonly view: AuthSettingsView;
}

export function ProviderSummary({ view }: ProviderSummaryProps) {
  const provider = view.provider;
  if (provider === null) return null;

  return (
    <section
      className={styles.panel}
      aria-label={t(CONSOLE_MESSAGE_KEYS.authsettings_current_heading)}
    >
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.authsettings_current_heading)}</h2>

      <dl className={styles.rows}>
        <dt>{t(CONSOLE_MESSAGE_KEYS.setup_auth_display_name_label)}</dt>
        <dd>{provider.displayName}</dd>

        <dt>{t(CONSOLE_MESSAGE_KEYS.setup_auth_client_id_label)}</dt>
        <dd>
          <code>{provider.clientId ?? ""}</code>
        </dd>

        <dt>{t(CONSOLE_MESSAGE_KEYS.setup_auth_discovery_url_label)}</dt>
        <dd>
          <code>{provider.discoveryUrl ?? ""}</code>
        </dd>

        <dt>{t(CONSOLE_MESSAGE_KEYS.setup_auth_allowed_domains_label)}</dt>
        <dd>{provider.allowedEmailDomains.join(", ")}</dd>
      </dl>

      <p className={styles.sealed}>
        {view.hasSealedEnvelope && view.sealedAgainstClientId !== null
          ? t(CONSOLE_MESSAGE_KEYS.authsettings_sealed_against, {
              client_id: view.sealedAgainstClientId,
            })
          : t(CONSOLE_MESSAGE_KEYS.authsettings_sealed_absent)}
      </p>

      {/* The drift the login screen can only report as "unavailable". */}
      {view.driftKey !== null && (
        <p className={styles.problem} role="alert">
          {t(view.driftKey)}
        </p>
      )}
    </section>
  );
}
