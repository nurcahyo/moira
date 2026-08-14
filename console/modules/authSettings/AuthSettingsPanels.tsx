"use client";

// The whole of `/settings/auth`, wired to one thing: re-read the page after a
// successful save.
//
// Same shape and same reason as `LlmSettingsPanels` and `ConsumerKeyPanels`, and
// one reason of its own: this screen's subject is the sign-in configuration the
// operator is currently authenticated by, so a stale render here is not merely
// out of date — it shows a configuration that may no longer be the one their own
// next request will be judged against. `router.refresh()` re-runs the server
// load, so what re-renders is what MOIRA and the secret store say.
//
// A NON-OWNER SEES THE SUMMARY AND NO FORMS. Not an empty page and not a
// redirect: reading the configuration escalates nothing, and an admin who cannot
// change it is still owed an answer to "what is it".

import { useRouter } from "next/navigation";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { AuthSettingsView } from "@/lib/auth-settings-view";

import { ProviderSummary } from "./ProviderSummary";
import { ProviderUpdateForm } from "./ProviderUpdateForm";
import { ReplaceSecretForm } from "./ReplaceSecretForm";
import styles from "./AuthSettingsPanels.module.css";

export interface AuthSettingsPanelsProps {
  readonly view: AuthSettingsView;
}

export function AuthSettingsPanels({ view }: AuthSettingsPanelsProps) {
  const router = useRouter();
  const reload = (): void => router.refresh();

  if (view.provider === null) {
    return (
      <p className={styles.problem} role="alert">
        {t(CONSOLE_MESSAGE_KEYS.authsettings_no_provider)}
      </p>
    );
  }

  return (
    <>
      <ProviderSummary view={view} />

      {view.isOwner ? (
        <>
          <ProviderUpdateForm provider={view.provider} onSaved={reload} />
          <ReplaceSecretForm onSaved={reload} />
        </>
      ) : (
        // `role="status"`, not `alert`: nothing has gone wrong. This is a
        // standing fact about who may act, rendered where the controls would be
        // so the absence is explained rather than merely observed.
        <p className={styles.notice} role="status">
          {t(CONSOLE_MESSAGE_KEYS.authsettings_not_owner)}
        </p>
      )}
    </>
  );
}
