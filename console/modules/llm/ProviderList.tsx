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
// DISABLE, NOT DELETE
// ============================================================================
//
// A wrong endpoint is the most likely first mistake on this screen, so every
// created thing has an undo. None of them destroys anything: Moira's LLM surface
// has no delete operation in this console's registry, and a disabled row stays
// readable, which is what makes "what did I do wrong" answerable afterwards.

import { useState } from "react";

import { Badge } from "@/components/atoms/Badge";
import { Button } from "@/components/atoms/Button";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { providerEndpoint, type LlmProviderView } from "@/lib/llm-view";

import { ProviderChainPanel } from "./ProviderChainPanel";
import { sendDelete, type LlmFailure } from "./request";
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
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      loading={busyId === model.id}
                      onClick={() => {
                        void disable(
                          model.id,
                          providerEndpoint(provider.id, `/models/${encodeURIComponent(model.id)}`),
                        );
                      }}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.llm_disable_model)}
                    </Button>
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
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      loading={busyId === row.id}
                      onClick={() => {
                        void disable(
                          row.id,
                          providerEndpoint(provider.id, `/credentials/${encodeURIComponent(row.id)}`),
                        );
                      }}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.llm_disable_key_row)}
                    </Button>
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
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      loading={busyId === policy.id}
                      onClick={() => {
                        void disable(
                          policy.id,
                          providerEndpoint(
                            provider.id,
                            `/routing/${encodeURIComponent(policy.id)}`,
                          ),
                        );
                      }}
                    >
                      {t(CONSOLE_MESSAGE_KEYS.llm_disable_policy)}
                    </Button>
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
