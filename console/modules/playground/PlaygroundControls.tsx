"use client";

// The playground's collapsed-by-default controls panel: route, agent profile
// (a route filter, not a wire field — see `types.ts`), provider/model hint,
// temperature, max tokens, and the diagnostics-only priority/complexity hint.
//
// An ORGANISM, not a molecule: it calls `fetch(` for the provider-dependent
// model list, which `layer-dependencies.test.ts` forbids below this layer.

import { useEffect, useId, useState } from "react";

import { Input } from "@/components/atoms/Input";
import { getJson } from "@/lib/console-request";
import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import type {
  AgentProfileRecord,
  ComplexityTier,
  ListResponse,
  ProviderModelRecord,
  ProviderRecord,
  RouteDefinitionRecord,
} from "@/lib/types";

import type { PlaygroundControlsValue } from "./types";
import styles from "./PlaygroundControls.module.css";

export interface PlaygroundControlsProps {
  readonly routes: readonly RouteDefinitionRecord[] | null;
  readonly agentProfiles: readonly AgentProfileRecord[] | null;
  readonly providers: readonly ProviderRecord[] | null;
  readonly value: PlaygroundControlsValue;
  readonly onChange: (next: PlaygroundControlsValue) => void;
  readonly disabled: boolean;
  /** Injected by the unit test. Shipped call sites use the global. */
  readonly fetchImpl?: typeof fetch;
}

const COMPLEXITY_TIERS: readonly ComplexityTier[] = ["trivial", "standard", "heavy"];

export function PlaygroundControls({
  routes,
  agentProfiles,
  providers,
  value,
  onChange,
  disabled,
  fetchImpl,
}: PlaygroundControlsProps) {
  const headingId = useId();
  // One id per field, EXPLICITLY paired via `htmlFor`/`id` rather than
  // implicit `<label>` wrapping — a hint or error `<p>` nested INSIDE an
  // implicit label becomes part of the control's accessible name (every
  // descendant text node is concatenated), which silently breaks
  // `getByLabelText("Priority")`-shaped queries the moment a hint is added.
  // Explicit association keeps the accessible name to the label text alone.
  const agentProfileId = useId();
  const routeId = useId();
  const providerId = useId();
  const modelId = useId();
  const temperatureId = useId();
  const maxTokensId = useId();
  const priorityId = useId();
  const complexityHintId = useId();
  // Keyed by the provider it was fetched FOR, not just "the latest models
  // fetched" — so the derived values below can express "stale/loading"
  // (providerId does not match) without ever calling `setState` synchronously
  // inside the effect body itself, which `react-hooks/set-state-in-effect`
  // forbids: every `setModelsState` call here happens after the `await`,
  // inside the resolved-promise continuation, never in the effect's own
  // synchronous run.
  const [modelsState, setModelsState] = useState<{
    readonly providerId: string;
    readonly models: readonly ProviderModelRecord[] | null;
    readonly failed: boolean;
  }>({ providerId: "", models: null, failed: false });

  useEffect(() => {
    if (value.providerId === "") return;
    let cancelled = false;
    void (async () => {
      const result = await getJson<ListResponse<ProviderModelRecord>>(
        `/api/playground/providers/${encodeURIComponent(value.providerId)}/models`,
        CONSOLE_MESSAGE_KEYS.playground_models_load_failed,
        fetchImpl,
      );
      if (cancelled) return;
      if (!result.ok) {
        setModelsState({ providerId: value.providerId, models: null, failed: true });
        return;
      }
      setModelsState({ providerId: value.providerId, models: result.data.data, failed: false });
    })();
    return () => {
      cancelled = true;
    };
  }, [value.providerId, fetchImpl]);

  const models = modelsState.providerId === value.providerId ? modelsState.models : null;
  const modelsFailed = modelsState.providerId === value.providerId ? modelsState.failed : false;

  function patch(next: Partial<PlaygroundControlsValue>): void {
    onChange({ ...value, ...next });
  }

  const visibleRoutes = (routes ?? []).filter(
    (route) => value.agentProfileFilter === "" || route.agent_profile_id === value.agentProfileFilter,
  );

  return (
    <details className={styles.detailsPanel}>
      <summary className={styles.summary} id={headingId}>
        {t(CONSOLE_MESSAGE_KEYS.playground_controls_heading)}
      </summary>

      <div className={styles.grid} aria-labelledby={headingId}>
        <div className={styles.field}>
          <label className={styles.label} htmlFor={agentProfileId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_agent_profile_label)}
          </label>
          <select
            id={agentProfileId}
            className={styles.select}
            disabled={disabled}
            value={value.agentProfileFilter}
            onChange={(event) => patch({ agentProfileFilter: event.target.value, route: "" })}
          >
            <option value="">{t(CONSOLE_MESSAGE_KEYS.playground_field_agent_profile_none)}</option>
            {(agentProfiles ?? []).map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.display_name}
              </option>
            ))}
          </select>
          <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.playground_field_agent_profile_hint)}</p>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={routeId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_route_label)}
          </label>
          <select
            id={routeId}
            className={styles.select}
            disabled={disabled}
            value={value.route}
            onChange={(event) => patch({ route: event.target.value })}
          >
            <option value="">{t(CONSOLE_MESSAGE_KEYS.playground_field_route_none)}</option>
            {visibleRoutes.map((route) => (
              <option key={route.id} value={route.route_key}>
                {route.display_name}
              </option>
            ))}
          </select>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={providerId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_provider_label)}
          </label>
          <select
            id={providerId}
            className={styles.select}
            disabled={disabled}
            value={value.providerId}
            onChange={(event) => patch({ providerId: event.target.value, modelKey: "", providerModelId: "" })}
          >
            <option value="">{t(CONSOLE_MESSAGE_KEYS.playground_field_provider_none)}</option>
            {(providers ?? []).map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.display_name}
              </option>
            ))}
          </select>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={modelId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_model_label)}
          </label>
          {value.providerId === "" ? (
            <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.playground_field_model_needs_provider)}</p>
          ) : (
            <>
              <select
                id={modelId}
                className={styles.select}
                disabled={disabled}
                value={value.modelKey}
                onChange={(event) => {
                  const selected = (models ?? []).find((model) => model.model_key === event.target.value);
                  patch({ modelKey: event.target.value, providerModelId: selected?.id ?? "" });
                }}
              >
                <option value="">{t(CONSOLE_MESSAGE_KEYS.playground_field_model_none)}</option>
                {(models ?? []).map((model) => (
                  <option key={model.id} value={model.model_key}>
                    {model.display_name ?? model.model_key}
                  </option>
                ))}
              </select>
              {modelsFailed && (
                <p className={styles.problem} role="alert">
                  {t(CONSOLE_MESSAGE_KEYS.playground_models_load_failed)}
                </p>
              )}
            </>
          )}
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={temperatureId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_temperature_label)}
          </label>
          <Input
            id={temperatureId}
            type="number"
            step="0.1"
            min={0}
            max={2}
            disabled={disabled}
            value={value.temperature}
            onChange={(event) => patch({ temperature: event.target.value })}
          />
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={maxTokensId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_max_tokens_label)}
          </label>
          <Input
            id={maxTokensId}
            type="number"
            step="1"
            min={0}
            disabled={disabled}
            value={value.maxTokens}
            onChange={(event) => patch({ maxTokens: event.target.value })}
          />
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={priorityId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_priority_label)}
          </label>
          <Input
            id={priorityId}
            type="number"
            step="1"
            disabled={disabled || !value.diagnostics}
            value={value.priority}
            onChange={(event) => patch({ priority: event.target.value })}
          />
          <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.playground_field_priority_hint)}</p>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor={complexityHintId}>
            {t(CONSOLE_MESSAGE_KEYS.playground_field_complexity_hint_label)}
          </label>
          <select
            id={complexityHintId}
            className={styles.select}
            disabled={disabled || !value.diagnostics}
            value={value.complexityHint}
            onChange={(event) =>
              patch({ complexityHint: event.target.value as PlaygroundControlsValue["complexityHint"] })
            }
          >
            <option value="">{t(CONSOLE_MESSAGE_KEYS.playground_complexity_none)}</option>
            {COMPLEXITY_TIERS.map((tier) => (
              <option key={tier} value={tier}>
                {t(
                  tier === "trivial"
                    ? CONSOLE_MESSAGE_KEYS.playground_complexity_trivial
                    : tier === "standard"
                      ? CONSOLE_MESSAGE_KEYS.playground_complexity_standard
                      : CONSOLE_MESSAGE_KEYS.playground_complexity_heavy,
                )}
              </option>
            ))}
          </select>
        </div>

        <label className={styles.toggleField}>
          <input
            type="checkbox"
            disabled={disabled}
            checked={value.stream}
            onChange={(event) => patch({ stream: event.target.checked })}
          />
          {t(CONSOLE_MESSAGE_KEYS.playground_field_stream_toggle_label)}
        </label>

        <label className={styles.toggleField}>
          <input
            type="checkbox"
            disabled={disabled}
            checked={value.diagnostics}
            onChange={(event) => patch({ diagnostics: event.target.checked })}
          />
          {t(CONSOLE_MESSAGE_KEYS.playground_field_diagnostics_toggle_label)}
        </label>
        <p className={styles.hint}>{t(CONSOLE_MESSAGE_KEYS.playground_field_diagnostics_toggle_hint)}</p>
      </div>
    </details>
  );
}
