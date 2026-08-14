"use client";

// Create the application a consumer key hangs off.
//
// On a freshly claimed deployment this is the FIRST control on the screen that
// does anything: `ConsumerKeyCreateRequest.application_id` is required, so
// nothing can be minted until an application exists. The mint forms live inside
// application rows for exactly that reason — the ordering is Moira's, and the
// layout says so rather than leaving an operator to discover it from a 400.
//
// A slug that is not slug-shaped is DROPPED by the handler, not refused. It is a
// convenience field; failing an operator's real intent (an application) over a
// field Moira does not require would be the wrong trade.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { ResponseText } from "@/lib/types";

import styles from "./AddApplicationForm.module.css";

/** The console's own create endpoint. Not Moira's. */
const CREATE_ENDPOINT = "/api/keys/applications";

export interface AddApplicationFormProps {
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites re-read the server data. */
  readonly onCreated?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly messageKey: string; readonly text: ResponseText | null };

export function AddApplicationForm({ fetchImpl, onCreated }: AddApplicationFormProps) {
  const [displayName, setDisplayName] = useState("");
  const [slug, setSlug] = useState("");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  const pending = phase.kind === "pending";

  async function submit(): Promise<void> {
    if (displayName.trim() === "") {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.keys_display_name_required,
        text: null,
      });
      return;
    }
    setPhase({ kind: "pending" });
    const send = fetchImpl ?? globalThis.fetch;

    let response: Response;
    try {
      response = await send(CREATE_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          display_name: displayName.trim(),
          application_slug: slug.trim(),
        }),
      });
    } catch {
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.keys_request_failed,
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
          text?.message_key ?? error?.message_key ?? CONSOLE_MESSAGE_KEYS.keys_request_failed,
        text,
      });
      return;
    }

    setPhase({ kind: "idle" });
    setDisplayName("");
    setSlug("");
    onCreated?.();
  }

  return (
    <section
      className={styles.panel}
      aria-label={t(CONSOLE_MESSAGE_KEYS.keys_add_application_heading)}
    >
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.keys_add_application_heading)}</h2>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.keys_application_name_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.keys_application_name_hint)}
        required
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.keys_application_slug_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.keys_application_slug_hint)}
        inputProps={{
          value: slug,
          disabled: pending,
          onChange: (event) => setSlug(event.target.value),
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
        {t(CONSOLE_MESSAGE_KEYS.keys_create_application_button)}
      </Button>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}
    </section>
  );
}
