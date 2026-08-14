"use client";

// Issue one consumer key for one application, and show it exactly once.
//
// ============================================================================
// THE SECOND MOUNT OF `OnceOnlySecretModal`, AND THE ARGUMENT FOR IT
// ============================================================================
//
// `no-secret-props.test.ts` rule (c) holds the mounting set to a named list and
// says a new entry "needs the same argument the first one made". That argument
// was: exactly one component may put a plaintext on screen, and the component
// that HANDS it there must hold the whole envelope rather than binding the
// secret to an identifier of its own — because rule (b), which scans for
// forwarding, runs only over the modal itself.
//
// It is made again here, unchanged and for the same reason. This file holds
// `MintedConsumerKey` in `minted` and reads `.secret` AT THE JSX SITE. There
// is no `const [secret, setSecret]` anywhere in it, which is what
// `STANDALONE_SECRET_BINDING` scans this file for now that it is on the list.
//
// What is NOT claimed: that a second mount is free. It is a second place a live
// credential is handled, and the alternative — a second modal — was rejected
// because rule (a) caps the "may hold the plaintext" allow-list at one entry, on
// the grounds that it is a one-answer question. Generalising the one modal keeps
// that answer at one; adding a mount moves the count that rule (c) tracks from
// one to two, deliberately and visibly.
//
// ============================================================================
// THE SCOPE CHECKBOXES ARE A SUGGESTION
// ============================================================================
//
// This is a client component. Whatever it posts, `POST /api/keys` runs
// `narrowScopes` over the body and keeps only what `OFFERED_CONSUMER_SCOPES`
// contains. So the curation is enforced on the server and merely RENDERED here;
// nothing about this form is load-bearing for what a key ends up able to do.

import { useState } from "react";

import { Button } from "@/components/atoms/Button";
import { FormField } from "@/components/molecules/FormField";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type { MintedConsumerKey } from "@/lib/keys-view";
import type { ResponseText } from "@/lib/types";
import { OnceOnlySecretModal } from "@/modules/secrets/OnceOnlySecretModal";

import { scopeMessageKey } from "./scope-copy";
import styles from "./MintKeyForm.module.css";

/** The console's own mint endpoint. Not Moira's. */
const MINT_ENDPOINT = "/api/keys";

export interface MintKeyFormProps {
  readonly applicationId: string;
  /** Raw Moira scope strings the server chose to offer. */
  readonly offeredScopes: readonly string[];
  /** Pre-checked. A key without it authenticates and can do nothing. */
  readonly defaultScope: string;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Injected by the unit test. Shipped call sites re-read the server data. */
  readonly onMinted?: () => void;
}

type Phase =
  | { readonly kind: "idle" }
  | { readonly kind: "pending" }
  | { readonly kind: "failed"; readonly messageKey: string; readonly text: ResponseText | null };

export function MintKeyForm({
  applicationId,
  offeredScopes,
  defaultScope,
  fetchImpl,
  onMinted,
}: MintKeyFormProps) {
  const [displayName, setDisplayName] = useState("");
  const [selected, setSelected] = useState<readonly string[]>([defaultScope]);
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  // THE WHOLE ENVELOPE, never the key on its own — see the header.
  const [minted, setMinted] = useState<MintedConsumerKey | null>(null);

  const pending = phase.kind === "pending";
  // Scopes nobody has written copy for are dropped rather than rendered raw.
  const offered = offeredScopes
    .map((scope) => ({ scope, messageKey: scopeMessageKey(scope) }))
    .filter((entry): entry is { scope: string; messageKey: string } => entry.messageKey !== null);

  function toggle(scope: string): void {
    setSelected((current) =>
      current.includes(scope) ? current.filter((entry) => entry !== scope) : [...current, scope],
    );
  }

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
      response = await send(MINT_ENDPOINT, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          application_id: applicationId,
          display_name: displayName.trim(),
          scopes: selected,
        }),
      });
    } catch {
      // The thrown cause is deliberately not read: a fetch failure can carry a
      // URL with credentials in it.
      setPhase({
        kind: "failed",
        messageKey: CONSOLE_MESSAGE_KEYS.keys_request_failed,
        text: null,
      });
      return;
    }

    let body: unknown;
    try {
      body = await response.json();
    } catch {
      body = undefined;
    }

    if (!response.ok) {
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
    // `secret === null` here is the idempotent-replay case and is a SUCCESS; the
    // modal renders it as "already shown".
    setMinted(body as MintedConsumerKey);
    onMinted?.();
  }

  return (
    <div className={styles.form}>
      <h5 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.keys_mint_heading)}</h5>

      <FormField
        label={t(CONSOLE_MESSAGE_KEYS.keys_key_name_label)}
        hint={t(CONSOLE_MESSAGE_KEYS.keys_key_name_hint)}
        required
        inputProps={{
          value: displayName,
          disabled: pending,
          onChange: (event) => setDisplayName(event.target.value),
        }}
      />

      <fieldset className={styles.scopes}>
        <legend className={styles.legend}>{t(CONSOLE_MESSAGE_KEYS.keys_scopes_label)}</legend>
        <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.keys_scopes_hint)}</p>
        {offered.map((entry) => (
          <label key={entry.scope} className={styles.choice}>
            <input
              type="checkbox"
              checked={selected.includes(entry.scope)}
              disabled={pending}
              onChange={() => toggle(entry.scope)}
            />
            {t(entry.messageKey)}
          </label>
        ))}
      </fieldset>

      <Button
        type="button"
        variant="primary"
        loading={pending}
        onClick={() => {
          void submit();
        }}
      >
        {t(CONSOLE_MESSAGE_KEYS.keys_mint_button)}
      </Button>

      {phase.kind === "failed" && (
        <p className={styles.problem} role="alert">
          {t(phase.messageKey, phase.text?.message_args, phase.text?.message)}
        </p>
      )}

      {minted !== null && (
        <OnceOnlySecretModal
          open
          secret={minted.secret}
          resource={minted.resource}
          headingKey={CONSOLE_MESSAGE_KEYS.keys_secret_heading}
          noticeKey={CONSOLE_MESSAGE_KEYS.keys_secret_notice}
          secretLabelKey={CONSOLE_MESSAGE_KEYS.secret_key_label}
          onDismiss={() => setMinted(null)}
        />
      )}
    </div>
  );
}
