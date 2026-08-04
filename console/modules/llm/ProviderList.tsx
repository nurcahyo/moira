"use client";

// Every configured provider, what it serves, and whether routing points at it.
//
// ============================================================================
// ONE SECTION PER PROVIDER, NOT ONE TABLE
// ============================================================================
//
// A provider is a four-part thing — the row, its models, its credential rows,
// the policies pointing at it — and a table row can hold one of those. The
// question this screen exists to answer is "can a prompt reach this?", which is
// a conjunction over all four, so each provider gets a section that shows all
// four and states the conjunction.
//
// ============================================================================
// WHAT IS RENDERED ABOUT A CREDENTIAL, AND WHAT IS NOT
// ============================================================================
//
// That a row exists, its type, and its status. Not `masked_secret`, and not
// `secret_fingerprint` — neither reaches the browser at all, because
// `loadLlmSettings` projects them away before the response is built. See
// `lib/llm-view.ts`.
//
// ============================================================================
// DISABLE, NOT DELETE — AND THE UNDO HAS AN UNDO
// ============================================================================
//
// A wrong endpoint is the most likely first mistake on this screen, so every
// created thing has an undo. None of them destroys anything: Moira's LLM surface
// has no delete operation in this console's registry, and a disabled row stays
// readable, which is what makes "what did I do wrong" answerable afterwards.
//
// "Reversible" is the stated justification for choosing disable over delete, so
// each of the three nested rows renders ONE control that swaps with its status:
// disable while it is active, enable while it is not. Without the second half
// the claim was false — routing accepts only active rows, re-adding a model with
// the same identifier collides with the disabled row still holding the unique
// index, and the operator's only route back was a SQL prompt.
//
// The provider's own control does not swap, and that asymmetry is deliberate: a
// disabled provider is brought back by the "Connect local vLLM" shortcut, which
// re-runs the whole chain and repairs every part of it at once, rather than by a
// button that fixes one of four things and leaves the other three.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { isRoutable, providerEndpoint, type LlmProviderView } from "@/lib/llm-view";

import { ProviderChainPanel } from "./ProviderChainPanel";
import { postJson, sendDelete, type LlmFailure } from "./request";
import styles from "./ProviderList.module.css";

export interface ProviderListProps {
  readonly providers: readonly LlmProviderView[];
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
  /** Shipped call sites reload the server-rendered data. */
  readonly onChanged?: () => void;
}

/** `active` renders as one badge tone, everything else as the other. */
export function statusKey(status: string): string {
  return status === "active"
    ? CONSOLE_MESSAGE_KEYS.llm_status_active
    : CONSOLE_MESSAGE_KEYS.llm_status_disabled;
}

export function ProviderList({ providers, fetchImpl, onChanged }: ProviderListProps) {
  const [busyId, setBusyId] = useState<string | null>(null);
  const [failure, setFailure] = useState<LlmFailure | null>(null);

  async function disable(id: string, url: string): Promise<void> {
    setBusyId(id);
    setFailure(null);
    const result = await sendDelete<{ id: string }>(url, fetchImpl);
    setBusyId(null);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    onChanged?.();
  }

  /** The same path, POSTed: the handler beside the disable one. */
  async function enable(id: string, url: string): Promise<void> {
    setBusyId(id);
    setFailure(null);
    const result = await postJson<{ id: string }>(url, {}, fetchImpl);
    setBusyId(null);
    if (!result.ok) {
      setFailure(result.failure);
      return;
    }
    onChanged?.();
  }

  /**
   * The one control a nested row gets, chosen by its status.
   *
   * Not two buttons with one disabled: a disabled "Enable" beside an enabled
   * "Disable" reads as two independent things, and a screen reader announces
   * both. One control that says what it will do is the honest rendering of a
   * two-state row.
   */
  function toggle(row: { id: string; status: string }, url: string, keys: {
    readonly disable: string;
    readonly enable: string;
  }) {
    const active = isRoutable(row.status);
    return (
      <Button
        type="button"
        variant="ghost"
        size="sm"
        loading={busyId === row.id}
        onClick={() => {
          void (active ? disable(row.id, url) : enable(row.id, url));
        }}
      >
        {t(active ? keys.disable : keys.enable)}
      </Button>
    );
  }

  return (
    <section className={styles.panel} aria-label={t(CONSOLE_MESSAGE_KEYS.llm_providers_heading)}>
      <h2 className={styles.heading}>{t(CONSOLE_MESSAGE_KEYS.llm_providers_heading)}</h2>

      {providers.length === 0 ? (
        <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.llm_providers_empty)}</p>
      ) : (
        providers.map((provider) => (
          <article key={provider.id} className={styles.provider}>
            <header className={styles.providerHeader}>
              <h3 className={styles.providerName}>{provider.displayName}</h3>
              <Badge tone={provider.status === "active" ? "success" : "neutral"}>
                {t(statusKey(provider.status))}
              </Badge>
            </header>

            <p className={styles.endpoint}>{provider.baseUrl ?? provider.providerType}</p>

            <h4 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.llm_models_heading)}</h4>
            {provider.models.length === 0 ? (
              <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.llm_models_empty)}</p>
            ) : (
              <ul className={styles.rows}>
                {provider.models.map((model) => (
                  <li key={model.id} className={styles.row}>
                    <span className={styles.rowName}>{model.modelKey}</span>
                    <Badge tone={model.status === "active" ? "success" : "neutral"}>
                      {t(statusKey(model.status))}
                    </Badge>
                    {toggle(
                      model,
                      providerEndpoint(provider.id, `/models/${encodeURIComponent(model.id)}`),
                      {
                        disable: CONSOLE_MESSAGE_KEYS.llm_disable_model,
                        enable: CONSOLE_MESSAGE_KEYS.llm_enable_model,
                      },
                    )}
                  </li>
                ))}
              </ul>
            )}

            <h4 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.llm_key_rows_heading)}</h4>
            {provider.keyRows.length === 0 ? (
              <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.llm_key_row_missing)}</p>
            ) : (
              <ul className={styles.rows}>
                {provider.keyRows.map((row) => (
                  <li key={row.id} className={styles.row}>
                    <span className={styles.rowName}>{t(CONSOLE_MESSAGE_KEYS.llm_key_row_present)}</span>
                    <Badge tone={row.status === "active" ? "success" : "neutral"}>
                      {t(statusKey(row.status))}
                    </Badge>
                    {toggle(
                      row,
                      providerEndpoint(provider.id, `/credentials/${encodeURIComponent(row.id)}`),
                      {
                        disable: CONSOLE_MESSAGE_KEYS.llm_disable_key_row,
                        enable: CONSOLE_MESSAGE_KEYS.llm_enable_key_row,
                      },
                    )}
                  </li>
                ))}
              </ul>
            )}

            <h4 className={styles.subheading}>{t(CONSOLE_MESSAGE_KEYS.llm_routing_heading)}</h4>
            {provider.policies.length === 0 ? (
              <p className={styles.empty}>{t(CONSOLE_MESSAGE_KEYS.llm_policy_missing)}</p>
            ) : (
              <ul className={styles.rows}>
                {provider.policies.map((policy) => (
                  <li key={policy.id} className={styles.row}>
                    <span className={styles.rowName}>
                      {policy.routeKey ?? t(CONSOLE_MESSAGE_KEYS.llm_policy_present)}
                    </span>
                    <Badge tone={policy.status === "active" ? "success" : "neutral"}>
                      {t(statusKey(policy.status))}
                    </Badge>
                    {toggle(
                      policy,
                      providerEndpoint(provider.id, `/routing/${encodeURIComponent(policy.id)}`),
                      {
                        disable: CONSOLE_MESSAGE_KEYS.llm_disable_policy,
                        enable: CONSOLE_MESSAGE_KEYS.llm_enable_policy,
                      },
                    )}
                  </li>
                ))}
              </ul>
            )}

            <ProviderChainPanel
              provider={provider}
              {...(fetchImpl === undefined ? {} : { fetchImpl })}
              {...(onChanged === undefined ? {} : { onChanged })}
            />

            <Button
              type="button"
              variant="danger"
              size="sm"
              loading={busyId === provider.id}
              disabled={provider.status !== "active"}
              onClick={() => {
                void disable(provider.id, providerEndpoint(provider.id));
              }}
            >
              {t(CONSOLE_MESSAGE_KEYS.llm_disable_provider)}
            </Button>
          </article>
        ))
      )}

      {failure !== null && (
        <p className={styles.problem} role="alert">
          {t(failure.messageKey, failure.messageArgs, failure.message)}
        </p>
      )}
    </section>
  );
}
