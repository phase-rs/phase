import { afterEach, describe, expect, it, vi } from "vitest";

import { executeLlmRequest, LLM_MAX_RESPONSE_BYTES, LlmTransportError } from "../llmClient";
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

    await expect(executeLlmRequest(spec)).resolves.toEqual({
      status: 200,
      body: '{"ok":true}',
    });

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

  it("returns an error response's body AND status so the engine can rule on both", async () => {
    const body = '{"error":{"message":"Incorrect API key provided"}}';
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(body, { status: 401 })));

    // The transport deliberately does not throw: the vendor's diagnostic is the
    // useful half. But the status must travel with it, because a body alone
    // cannot be judged — only the engine, holding both, can refuse it.
    await expect(executeLlmRequest(spec)).resolves.toEqual({ status: 401, body });
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

  it("never opens a socket for a decision that is already stale", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    const controller = new AbortController();
    controller.abort();

    await expect(
      executeLlmRequest(spec, { signal: controller.signal }),
    ).rejects.toThrow(/cancelled/);
    // The provider is never called, so it is never billed for a reply that
    // would be discarded.
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("abandons a response that exceeds the size cap instead of buffering it", async () => {
    const chunk = new TextEncoder().encode("x".repeat(64 * 1024));
    let emitted = 0;
    const cancel = vi.fn(async () => {});
    // A stream that would never stop on its own.
    const body = {
      getReader: () => ({
        read: async () => {
          emitted += chunk.byteLength;
          return { done: false, value: chunk };
        },
        cancel,
      }),
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ status: 200, body }) as unknown as Response),
    );

    await expect(executeLlmRequest(spec)).rejects.toThrow(/exceeded/);
    // Stopped at the budget rather than reading forever, and the socket was
    // released.
    expect(emitted).toBeLessThanOrEqual(LLM_MAX_RESPONSE_BYTES + chunk.byteLength);
    expect(cancel).toHaveBeenCalled();
  });

  it("reads a normal streamed body in full", async () => {
    const payload = '{"choices":[{"message":{"content":"pick 1"}}]}';
    const encoded = new TextEncoder().encode(payload);
    let sent = false;
    const body = {
      getReader: () => ({
        read: async () => {
          if (sent) return { done: true, value: undefined };
          sent = true;
          return { done: false, value: encoded };
        },
        cancel: async () => {},
      }),
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ status: 200, body }) as unknown as Response),
    );

    await expect(executeLlmRequest(spec)).resolves.toEqual({ status: 200, body: payload });
  });
});
