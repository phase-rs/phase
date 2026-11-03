import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const probeMocks = vi.hoisted(() => ({
  testLlmEndpoint: vi.fn<
    (
      profile: unknown,
      options?: { signal?: AbortSignal },
    ) => Promise<{ ok: true } | { ok: false; code: string; detail?: string }>
  >(),
}));
vi.mock("../../services/llm/probe", () => ({ testLlmEndpoint: probeMocks.testLlmEndpoint }));

import { useLlmConnectionTest, type LlmTestState } from "../useLlmConnectionTest";
import type { LlmProfile } from "../../services/llm/types";

function profile(overrides: Partial<LlmProfile> = {}): LlmProfile {
  return {
    id: "p1",
    name: "Test",
    provider: "OpenAi",
    baseUrl: "https://api.openai.com/v1",
    apiKey: "sk-test",
    model: "gpt-5",
    maxOutputTokens: null,
    temperature: null,
    enabled: true,
    ...overrides,
  };
}

/** Deferred result so a probe can be held open across a profile edit. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  return { promise: new Promise<T>((r) => (resolve = r)), resolve };
}

/** The most recent published state. `Array.prototype.at` is newer than this
 *  project's TS lib target. */
function last(states: LlmTestState[]): LlmTestState | undefined {
  return states[states.length - 1];
}

function renderTest(initial: LlmProfile) {
  const states: LlmTestState[] = [];
  const setTest = (state: LlmTestState) => {
    states.push(state);
  };
  const view = renderHook(({ p }: { p: LlmProfile }) => useLlmConnectionTest(p, setTest), {
    initialProps: { p: initial },
  });
  return { ...view, states };
}

beforeEach(() => {
  probeMocks.testLlmEndpoint.mockReset();
});

describe("useLlmConnectionTest", () => {
  it("publishes a success for the endpoint it tested", async () => {
    probeMocks.testLlmEndpoint.mockResolvedValue({ ok: true });
    const { result, states } = renderTest(profile());

    await act(async () => {
      result.current();
    });

    expect(last(states)).toEqual({ status: "ok" });
  });

  it("publishes the engine's failure message verbatim", async () => {
    probeMocks.testLlmEndpoint.mockResolvedValue({
      ok: false,
      code: "undecodable",
      detail: "API key not valid. Please pass a valid API key.",
    });
    const { result, states } = renderTest(profile());

    await act(async () => {
      result.current();
    });

    // The reason is a translatable code; the vendor's own diagnostic rides
    // alongside as data.
    expect(last(states)).toEqual({
      status: "failed",
      code: "undecodable",
      detail: "API key not valid. Please pass a valid API key.",
    });
  });

  /// The finding: a probe that resolves after the endpoint was edited would
  /// paint its verdict over a configuration that is no longer on screen.
  it("discards a result whose endpoint was edited while it was in flight", async () => {
    const pending = deferred<{ ok: true }>();
    probeMocks.testLlmEndpoint.mockReturnValue(pending.promise);
    const { result, rerender, states } = renderTest(profile());

    act(() => {
      result.current();
    });
    // The player edits the endpoint before the probe comes back.
    rerender({ p: profile({ baseUrl: "https://somewhere-else.example/v1" }) });
    await act(async () => {
      pending.resolve({ ok: true });
      await pending.promise;
    });

    // "Connected" must not be shown for the endpoint that is no longer set.
    expect(states).not.toContainEqual({ status: "ok" });
    expect(last(states)).toEqual({ status: "idle" });
  });

  it("aborts the in-flight request when the endpoint is edited", async () => {
    const pending = deferred<{ ok: true }>();
    let seenSignal: AbortSignal | undefined;
    probeMocks.testLlmEndpoint.mockImplementation((_p, options) => {
      seenSignal = options?.signal;
      return pending.promise;
    });
    const { result, rerender } = renderTest(profile());

    act(() => {
      result.current();
    });
    expect(seenSignal?.aborted).toBe(false);

    rerender({ p: profile({ model: "gpt-5-mini" }) });

    // The old request is cut loose rather than left running against a
    // configuration that no longer exists.
    expect(seenSignal?.aborted).toBe(true);
    await act(async () => {
      pending.resolve({ ok: true });
      await pending.promise;
    });
  });

  it("abandons an in-flight request on unmount", async () => {
    const pending = deferred<{ ok: true }>();
    let seenSignal: AbortSignal | undefined;
    probeMocks.testLlmEndpoint.mockImplementation((_p, options) => {
      seenSignal = options?.signal;
      return pending.promise;
    });
    const { result, unmount, states } = renderTest(profile());

    act(() => {
      result.current();
    });
    unmount();
    await act(async () => {
      pending.resolve({ ok: true });
      await pending.promise;
    });

    expect(seenSignal?.aborted).toBe(true);
    expect(states).not.toContainEqual({ status: "ok" });
  });

  it("supersedes an earlier probe when the button is pressed again", async () => {
    const first = deferred<{ ok: false; code: "unreachable"; detail?: string }>();
    const second = deferred<{ ok: true }>();
    probeMocks.testLlmEndpoint
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const { result, states } = renderTest(profile());

    act(() => {
      result.current();
    });
    act(() => {
      result.current();
    });
    await act(async () => {
      first.resolve({ ok: false, code: "unreachable", detail: "stale failure" });
      second.resolve({ ok: true });
      await Promise.resolve();
    });

    // The superseded probe's verdict never surfaces.
    expect(states).not.toContainEqual({
      status: "failed",
      code: "unreachable",
      detail: "stale failure",
    });
    expect(last(states)).toEqual({ status: "ok" });
  });

  /// The probe must not report Connected for a non-2xx response whose body
  /// happens to decode. The engine holds that verdict; this asserts the hook
  /// surfaces the refusal rather than the shape of the body.
  it("reports a failure when the engine refuses a non-2xx reply", async () => {
    probeMocks.testLlmEndpoint.mockResolvedValue({
      ok: false,
      code: "undecodable",
      detail: "HTTP 429: rate limited",
    });
    const { result, states } = renderTest(profile());

    await act(async () => {
      result.current();
    });

    expect(last(states)).toEqual({
      status: "failed",
      code: "undecodable",
      detail: "HTTP 429: rate limited",
    });
    expect(states).not.toContainEqual({ status: "ok" });
  });
});
