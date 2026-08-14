"use client";

// The whole of `/settings/keys`, wired to one thing: re-read the page after any
// panel changes something.
//
// Same shape and same reason as `LlmSettingsPanels`. `page.tsx` is a server
// component that reads the view from Moira and hands it down, so a `fetch` a
// client organism makes afterwards changes the deployment and nothing on the
// screen — the key it just minted would be absent from the list beside the modal
// still showing its secret. `router.refresh()` re-runs `loadConsumerKeys` on the
// server, so what re-renders is what MOIRA says rather than what this component
// believes it just did.
//
// It holds no state and renders no copy beyond the section it groups.

import { useRouter } from "next/navigation";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ConsumerKeysView } from "@/lib/keys-view";

import { AddApplicationForm } from "./AddApplicationForm";
import { KeyList } from "./KeyList";
import { MintKeyForm } from "./MintKeyForm";
import styles from "./ConsumerKeyPanels.module.css";

export interface ConsumerKeyPanelsProps {
  readonly view: ConsumerKeysView;
}

export function ConsumerKeyPanels({ view }: ConsumerKeyPanelsProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  return (
    <>
      <AddApplicationForm onCreated={reload} />

      <section
        className={styles.panel}
        aria-label={t(CONSOLE_MESSAGE_KEYS.keys_applications_heading)}
      >
        <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.keys_applications_heading)}</h2>

        {view.truncated && (
          <p className={styles.notice} role="status">
            {t(CONSOLE_MESSAGE_KEYS.keys_truncated_notice)}
          </p>
        )}

        {view.applications.length === 0 ? (
          <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.keys_applications_empty)}</p>
        ) : (
          view.applications.map((application) => (
            <article key={application.id} className={styles.application}>
              <h3 className={styles.applicationName}>
                {application.display_name}
                {application.application_slug !== null && (
                  <code className={styles.slug}>{application.application_slug}</code>
                )}
              </h3>

              <h4 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.keys_issued_heading)}</h4>
              <KeyList keys={application.keys} onRevoked={reload} />

              <MintKeyForm
                applicationId={application.id}
                offeredScopes={view.offeredScopes}
                defaultScope={view.defaultScope}
                onMinted={reload}
              />
            </article>
          ))
        )}
      </section>

      {/* Keys whose application this page did not list. Rendered rather than
          filtered: a key that still authenticates and appears on no screen is
          the one nobody revokes. */}
      {view.unattachedKeys.length > 0 && (
        <section
          className={styles.panel}
          aria-label={t(CONSOLE_MESSAGE_KEYS.keys_unattached_heading)}
        >
          <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.keys_unattached_heading)}</h2>
          <p className={styles.notice}>{t(CONSOLE_MESSAGE_KEYS.keys_unattached_intro)}</p>
          <KeyList keys={view.unattachedKeys} onRevoked={reload} />
        </section>
      )}
    </>
  );
}
