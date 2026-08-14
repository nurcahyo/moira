"use client";

// Replace the stored client secret, and nothing else.
//
// ============================================================================
// WHY THIS IS A SEPARATE FORM AND NOT A FIELD ON THE OTHER ONE
// ============================================================================
//
// It touches a different system. The update form writes Moira's row; this writes
// only this console's sealed envelope — no request to Moira, no `If-Match`, no
// version bump — because the secret is console-owned and Moira's row is already
// correct when the identity provider has issued a new secret for the same client
// id.
//
// It is also the action that makes the `CONSOLE_SECRET_ENCRYPTION_KEY` rotation
// runbook in `docs/console-storage.md` followable: that procedure ends with
// "re-enter each provider's client secret through the console", and until this
// screen the form it meant had closed with the setup window.
//
// The field is never pre-filled and the value is dropped on success.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ResponseText } from "@/lib/types";

import styles from "./ReplaceSecretForm.module.css";

/** The console's own endpoint. Not Moira's. */
const SAVE_ENDPOINT = "/api/settings/auth";

export interface ReplaceSecretFormProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites re-read the server data. */
  readonly onSaved?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "saved" }
  | { readonly kind: "failed"; readonly messageKey: string; readonly text: ResponseText | null };

export function ReplaceSecretForm({ fetchImpl, onSaved }: ReplaceSecretFormProps) {
  const [value, setValue] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    if (value.trim() === "") {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.authsettings_secret_required,
        text: null,
      });
      return;
    }
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(SAVE_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ action: "replace_secret", client_secret: value }),
      });
    } catch {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.authsettings_request_failed,
        text: null,
      });
      return;
    }

    if (!response.ok) {
      let body: unknown;
      try {
        body = await response.json();
      } catch {
        body = undefined;
      }
      const error = (body as { error?: { text?: ResponseText; message_key?: string } } | undefined)
        ?.error;
      const text = error?.text ?? null;
      setPhase({
        kind: "failed",
        messageKey:
          text?.message_key ??
          error?.message_key ??
          CONSOLE_MESSAGE_KEYS.authsettings_request_failed,
        text,
      });
      return;
    }

    setValue("");
    setPhase({ kind: "saved" });
    onSaved?.();
  }

  return (
    <section
      className={styles.panel}
      aria-label={t(CONSOLE_MESSAGE_KEYS.authsettings_rotate_heading)}
    >
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.authsettings_rotate_heading)}</h2>
      <p className={styles.intro}>{t(CONSOLE_MESSAGE_KEYS.authsettings_rotate_intro)}</p>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_secret_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.setup_auth_client_secret_hint)}
        required
        inputProps={{
          value,
          type: "password",
          autoComplete: "new-password",
          disabled: pending,
          onChange: (event) => setValue(event.target.value),
        }}
      />

      <Button
        type="button"
        variant="primary"
        loading={pending}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.authsettings_rotate_button)}
      </Button>

      {phase.kind === "saved" && (
        <p className={styles.saved} role="status">
          {t(CONSOLE_MESSAGE_KEYS.authsettings_saved)}
        </p>
      )}

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}
    </section>
  );
}
