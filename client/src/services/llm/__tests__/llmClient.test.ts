import { afterEach, describe, expect, it, vi } from "vitest";

import { executeLlmRequest, LlmTransportError } from "../llmClient";
import type { LlmHttpRequestSpec } from "../types";

const spec: LlmHttpRequestSpec = {
  url: "https://api.example.test/v1/chat/completions",
  method: "POST",
  headers: [
    { name: "content-type", value: "application/json" },
    { name: "authorization", value: "Bearer secret" },
  ],
  body: '{"model":"x"}',
};

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("executeLlmRequest", () => {
  it("sends exactly the engine-described call", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response('{"ok":true}', { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(executeLlmRequest(spec)).resolves.toBe('{"ok":true}');

    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe(spec.url);
    expect(init.method).toBe("POST");
    expect(init.body).toBe(spec.body);
    expect(init.headers).toEqual({
      "content-type": "application/json",
      authorization: "Bearer secret",
    });
    // A third-party AI endpoint must never receive the player's cookies.
    expect(init.credentials).toBe("omit");
  });

  it("returns the body of an error response so the engine can surface the vendor's message", async () => {
    const body = '{"error":{"message":"Incorrect API key provided"}}';
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(body, { status: 401 })));

    await expect(executeLlmRequest(spec)).resolves.toBe(body);
  });

  it("raises a transport error for an empty body", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 500 })));

    await expect(executeLlmRequest(spec)).rejects.toBeInstanceOf(LlmTransportError);
  });

  it("raises a transport error when the endpoint cannot be reached", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));

    await expect(executeLlmRequest(spec)).rejects.toThrow(/Could not reach the LLM endpoint/);
  });

  it("aborts when the decision it belongs to is cancelled", async () => {
    const controller = new AbortController();
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_url: string, init: RequestInit) =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(new DOMException("aborted", "AbortError")),
            );
          }),
      ),
    );

    const pending = executeLlmRequest(spec, { signal: controller.signal });
    controller.abort();

    await expect(pending).rejects.toThrow(/cancelled/);
  });

  it("gives up on a provider that never answers", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        (_url: string, init: RequestInit) =>
          new Promise((_resolve, reject) => {
            init.signal?.addEventListener("abort", () =>
              reject(new DOMException("aborted", "AbortError")),
            );
          }),
      ),
    );

    await expect(executeLlmRequest(spec, { timeoutMs: 5 })).rejects.toThrow(/timed out/);
  });
});
