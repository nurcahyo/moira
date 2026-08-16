// `lib/runners.ts` — the two pieces of business logic issue #275/#272
// workstream R3 needed beyond a thin pass-through to `MoiraClient`:
// deterministic idempotency for provisioning, and the read-then-delete
// pairing that keeps `deleteRunner`'s required `If-Match` from racing the
// ETag advance `GET /runners/{id}` causes on every poll.

import { describe, expect, test } from "bun:test";

import { MoiraClient } from "@/lib/moira-client";
import { deleteRunnerSafely, provisionRunner } from "@/lib/runners";
import type { ClaudeRunnerRecord } from "@/lib/types";

import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type StubHandler,
} from "../../support/moira-stub";

const RUNNER_ID = "aaaaaaaa-1111-4111-8111-111111111111";

function runnerRecord(overrides: Partial<ClaudeRunnerRecord> = {}): ClaudeRunnerRecord {
  return {
    id: RUNNER_ID,
    label: "claude-1",
    runner_reference: "runner-ref-1",
    state: "provisioning",
    scope: { type: "global" },
    metadata: {},
    created_at: "2026-08-16T00:00:00Z",
    updated_at: "2026-08-16T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function client(handlers: Record<string, StubHandler>) {
  const stub = createMoiraStub(handlers);
  return {
    stub,
    moira: new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test", fetch: stub.fetch }),
  };
}

describe("provisionRunner", () => {
  test("derives the idempotency key from the label alone", async () => {
    const { moira, stub } = client({
      "POST /api/v1/admin/runners": () => ({ status: 201, body: runnerRecord() }),
    });
    await provisionRunner(moira, { label: "claude-1" });
    const request = stub.requestsFor("POST /api/v1/admin/runners")[0]!;
    expect(request.headers["Idempotency-Key"]).toBe("runner-provision:claude-1");
  });

  test("omits ttl_seconds and scope when not supplied", async () => {
    const { moira, stub } = client({
      "POST /api/v1/admin/runners": () => ({ status: 201, body: runnerRecord() }),
    });
    await provisionRunner(moira, { label: "claude-1" });
    const body = stub.bodyOf("POST /api/v1/admin/runners") as Record<string, unknown>;
    expect(body).toEqual({ label: "claude-1" });
  });

  test("forwards ttl_seconds and a tenant scope when supplied", async () => {
    const { moira, stub } = client({
      "POST /api/v1/admin/runners": () => ({ status: 201, body: runnerRecord() }),
    });
    await provisionRunner(moira, {
      label: "claude-acme",
      ttlSeconds: 600,
      scope: { type: "tenant", external_tenant_id: "acme" },
    });
    const body = stub.bodyOf("POST /api/v1/admin/runners") as Record<string, unknown>;
    expect(body).toEqual({
      label: "claude-acme",
      ttl_seconds: 600,
      scope: { type: "tenant", external_tenant_id: "acme" },
    });
  });
});

describe("deleteRunnerSafely", () => {
  test("re-reads the runner and deletes with the FRESH version, not a stale one", async () => {
    const { moira, stub } = client({
      "GET /api/v1/admin/runners/aaaaaaaa-1111-4111-8111-111111111111": () => ({
        status: 200,
        // A version far ahead of anything the caller could already be holding —
        // this is the ETag-advances-on-every-poll property, simulated.
        body: runnerRecord({ version: 7 }),
      }),
      [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: (request) => {
        expect(request.headers["If-Match"]).toBe("7");
        return { status: 204 };
      },
    });
    const outcome = await deleteRunnerSafely(moira, RUNNER_ID);
    expect(outcome).toEqual({ ok: true });
    expect(stub.routes()).toEqual([
      `GET /api/v1/admin/runners/${RUNNER_ID}`,
      `DELETE /api/v1/admin/runners/${RUNNER_ID}`,
    ]);
  });

  test("on resource_version_conflict, re-reads AGAIN rather than retrying the stale delete", async () => {
    let getCount = 0;
    let deleteCount = 0;
    const { moira, stub } = client({
      [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => {
        getCount += 1;
        return { status: 200, body: runnerRecord({ version: getCount }) };
      },
      [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: (request) => {
        deleteCount += 1;
        if (deleteCount === 1) {
          // The first delete lost the race. `deleteRunnerSafely` must re-read
          // rather than simply resubmitting the same `If-Match`.
          expect(request.headers["If-Match"]).toBe("1");
          return { status: 409, body: errorEnvelope("resource_version_conflict") };
        }
        expect(request.headers["If-Match"]).toBe("2");
        return { status: 204 };
      },
    });
    const outcome = await deleteRunnerSafely(moira, RUNNER_ID);
    expect(outcome).toEqual({ ok: true });
    expect(getCount).toBe(2);
    expect(deleteCount).toBe(2);
    expect(stub.routes()).toEqual([
      `GET /api/v1/admin/runners/${RUNNER_ID}`,
      `DELETE /api/v1/admin/runners/${RUNNER_ID}`,
      `GET /api/v1/admin/runners/${RUNNER_ID}`,
      `DELETE /api/v1/admin/runners/${RUNNER_ID}`,
    ]);
  });

  test("gives up after its bounded number of conflicts, rather than retrying forever", async () => {
    const { moira } = client({
      [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({ status: 200, body: runnerRecord() }),
      [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
        status: 409,
        body: errorEnvelope("resource_version_conflict"),
      }),
    });
    const outcome = await deleteRunnerSafely(moira, RUNNER_ID);
    expect(outcome).toEqual({ ok: false, failure: { kind: "conflict_exhausted" } });
  });

  test("a runner not found on the initial read is reported, not thrown", async () => {
    const { moira } = client({
      [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
        status: 404,
        body: errorEnvelope("runner_not_found"),
      }),
    });
    const outcome = await deleteRunnerSafely(moira, RUNNER_ID);
    expect(outcome).toEqual({ ok: false, failure: { kind: "not_found" } });
  });

  test("a runner deleted by someone else between the read and the delete is also reported, not thrown", async () => {
    const { moira } = client({
      [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({ status: 200, body: runnerRecord() }),
      [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
        status: 404,
        body: errorEnvelope("runner_not_found"),
      }),
    });
    const outcome = await deleteRunnerSafely(moira, RUNNER_ID);
    expect(outcome).toEqual({ ok: false, failure: { kind: "not_found" } });
  });

  test("an unrelated failure is rethrown rather than swallowed", async () => {
    const { moira } = client({
      [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
        status: 503,
        body: errorEnvelope("runner_service_unavailable"),
      }),
    });
    await expect(deleteRunnerSafely(moira, RUNNER_ID)).rejects.toThrow();
  });
});
