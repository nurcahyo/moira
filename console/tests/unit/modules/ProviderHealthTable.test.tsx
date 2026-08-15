// The `/providers/health` table (issue #83) — a plain server-renderable
// organism with no client interactivity.

import { afterEach, describe, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";

import { CONSOLE_CATALOG } from "@/lib/i18n";
import { CONSOLE_MESSAGE_KEYS, type ConsoleMessageKey } from "@/lib/i18n/keys";
import type { ProviderHealthEntry, ProviderHealthResponse } from "@/lib/types";
import { ProviderHealthTable } from "@/modules/providerHealth/ProviderHealthTable";

const copy = (key: string): string => CONSOLE_CATALOG[key as ConsoleMessageKey].message;

afterEach(cleanup);

function entry(overrides: Partial<ProviderHealthEntry> = {}): ProviderHealthEntry {
  return {
    provider_id: "p1",
    provider_type: "open_ai_compatible",
    display_name: "Local vLLM",
    status: "healthy",
    probes_total: 10,
    probes_successful: 10,
    average_latency_ms: 123.4,
    last_failure_at: null,
    last_probe_at: "2026-08-15T00:00:00Z",
    last_success_at: "2026-08-15T00:00:00Z",
    ...overrides,
  };
}

describe("ProviderHealthTable", () => {
  test("an empty deployment gets an honest empty state, not a blank table", () => {
    render(<ProviderHealthTable health={{ providers: [] }} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_empty))).toBeDefined();
    expect(screen.queryByRole("table")).toBeNull();
  });

  test("every health status renders words, never colour alone", () => {
    const health: ProviderHealthResponse = {
      providers: [
        entry({ provider_id: "p1", status: "healthy" }),
        entry({ provider_id: "p2", status: "degraded" }),
        entry({ provider_id: "p3", status: "unhealthy" }),
        entry({ provider_id: "p4", status: "unknown", average_latency_ms: null, last_probe_at: null, last_success_at: null, last_failure_at: null }),
      ],
    };
    render(<ProviderHealthTable health={health} />);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_status_healthy))).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_status_degraded))).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_status_unhealthy))).toBeDefined();
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_status_unknown))).toBeDefined();
  });

  test("null timestamps and null latency render their own stated absence, not blank cells", () => {
    render(
      <ProviderHealthTable
        health={{
          providers: [
            entry({ average_latency_ms: null, last_probe_at: null, last_success_at: null, last_failure_at: null }),
          ],
        }}
      />,
    );
    expect(screen.getAllByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_never)).length).toBe(3);
    expect(screen.getByText(copy(CONSOLE_MESSAGE_KEYS.providerhealth_latency_unknown))).toBeDefined();
  });

  test("a present latency is rounded and rendered in milliseconds", () => {
    render(<ProviderHealthTable health={{ providers: [entry({ average_latency_ms: 123.4 })] }} />);
    expect(screen.getByText("123 ms")).toBeDefined();
  });

  test("the probe counts render as successful over total", () => {
    render(<ProviderHealthTable health={{ providers: [entry({ probes_successful: 7, probes_total: 10 })] }} />);
    expect(screen.getByText(/7\s*\/\s*10/)).toBeDefined();
  });
});
