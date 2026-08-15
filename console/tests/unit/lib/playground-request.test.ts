import { describe, expect, test } from "bun:test";

import {
  buildDiagnosticExecutionRequest,
  buildPublicResponseRequest,
  readPlaygroundDiagnoseBody,
  readPlaygroundRunBody,
} from "@/lib/playground-request";

describe("readPlaygroundRunBody", () => {
  test("returns null when the prompt is missing", () => {
    expect(readPlaygroundRunBody({})).toBeNull();
  });

  test("returns null when the prompt is blank", () => {
    expect(readPlaygroundRunBody({ prompt: "   " })).toBeNull();
  });

  test("reads every optional control, defaulting to null when absent", () => {
    const input = readPlaygroundRunBody({ prompt: "hello" });
    expect(input).toEqual({
      prompt: "hello",
      route: null,
      provider: null,
      model: null,
      temperature: null,
      maxOutputTokens: null,
    });
  });

  test("reads a fully populated body", () => {
    const input = readPlaygroundRunBody({
      prompt: "hello",
      route: "general",
      provider: "aaaaaaaa-1111-4111-8111-111111111111",
      model: "gpt-test",
      temperature: 0.7,
      max_output_tokens: 512,
    });
    expect(input).toEqual({
      prompt: "hello",
      route: "general",
      provider: "aaaaaaaa-1111-4111-8111-111111111111",
      model: "gpt-test",
      temperature: 0.7,
      maxOutputTokens: 512,
    });
  });

  test("ignores a non-finite temperature rather than sending NaN/Infinity", () => {
    const input = readPlaygroundRunBody({ prompt: "hello", temperature: Number.NaN });
    expect(input?.temperature).toBeNull();
  });
});

describe("buildPublicResponseRequest", () => {
  test("builds the exact PublicResponseRequest shape, wrapping the prompt as a user input_text message", () => {
    const body = buildPublicResponseRequest({
      prompt: "hello",
      route: "general",
      provider: null,
      model: "gpt-test",
      temperature: 0.5,
      maxOutputTokens: 256,
    });
    expect(body).toEqual({
      input: [{ role: "user", content: [{ type: "input_text", text: "hello" }] }],
      route: "general",
      provider: null,
      model: "gpt-test",
      temperature: 0.5,
      max_output_tokens: 256,
    });
  });
});

describe("readPlaygroundDiagnoseBody", () => {
  test("returns null when the prompt is missing or blank", () => {
    expect(readPlaygroundDiagnoseBody({})).toBeNull();
    expect(readPlaygroundDiagnoseBody({ prompt: "" })).toBeNull();
  });

  test("reads priority and complexity_hint alongside the rest", () => {
    const input = readPlaygroundDiagnoseBody({
      prompt: "hello",
      route: "general",
      provider_id: "aaaaaaaa-1111-4111-8111-111111111111",
      provider_model_id: "bbbbbbbb-1111-4111-8111-111111111111",
      temperature: 0.2,
      max_tokens: 128,
      priority: 5,
      complexity_hint: "heavy",
    });
    expect(input).toEqual({
      prompt: "hello",
      route: "general",
      providerId: "aaaaaaaa-1111-4111-8111-111111111111",
      providerModelId: "bbbbbbbb-1111-4111-8111-111111111111",
      temperature: 0.2,
      maxTokens: 128,
      priority: 5,
      complexityHint: "heavy",
    });
  });

  test("rejects an unrecognised complexity_hint value rather than forwarding it", () => {
    const input = readPlaygroundDiagnoseBody({ prompt: "hello", complexity_hint: "extreme" });
    expect(input?.complexityHint).toBeNull();
  });
});

describe("buildDiagnosticExecutionRequest", () => {
  test("builds the exact DiagnosticExecutionRequest shape, prompt as a bare string and stream always false", () => {
    const body = buildDiagnosticExecutionRequest({
      prompt: "hello",
      route: "general",
      providerId: "aaaaaaaa-1111-4111-8111-111111111111",
      providerModelId: "bbbbbbbb-1111-4111-8111-111111111111",
      temperature: 0.2,
      maxTokens: 128,
      priority: 5,
      complexityHint: "heavy",
    });
    expect(body).toEqual({
      prompt: "hello",
      route: "general",
      provider_id: "aaaaaaaa-1111-4111-8111-111111111111",
      provider_model_id: "bbbbbbbb-1111-4111-8111-111111111111",
      stream: false,
      options: {
        temperature: 0.2,
        max_tokens: 128,
        priority: 5,
        complexity_hint: "heavy",
      },
    });
  });
});
