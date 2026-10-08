/**
 * The LLM transport: execute the HTTP call the engine described, return the raw
 * body.
 *
 * This is the whole of the display layer's involvement in an LLM decision. It
 * does not build the request, does not read the response, and does not decide
 * what a failure means — it only moves bytes and enforces a wall-clock bound so
 * a hung provider cannot stall a turn.
 */

import type { LlmHttpRequestSpec, LlmMessageCode } from "./types";

/**
 * Per-call ceiling. A decision that has not come back by now is abandoned and
 * the seat falls back to the heuristic AI, which is why this can be generous
 * enough for a reasoning model without risking a stuck game.
 */
export const LLM_REQUEST_TIMEOUT_MS = 45_000;

/**
 * Ceiling on how much of a provider response is read.
 *
 * The body is untrusted input from a third-party endpoint, and the engine only
 * ever needs a short JSON decision out of it. Buffering the whole stream would
 * let a misconfigured, hostile, or simply chatty endpoint pin arbitrary memory
 * in the tab, so the read stops at this many bytes and the call fails. 1 MiB is
 * orders of magnitude above any real completion envelope.
 */
export const LLM_MAX_RESPONSE_BYTES = 1024 * 1024;

/**
 * A transport-level failure, distinct from the engine's `LlmError` outcomes.
 *
 * Carries both a `code` and a `message`: the code is what the UI translates,
 * the message is the developer-facing text that reaches the debug log. Keeping
 * both means a localized surface and a legible log without one constraining the
 * other.
 */
export class LlmTransportError extends Error {
  constructor(
    readonly code: LlmMessageCode,
    message: string,
    readonly status?: number,
  ) {
    super(message);
    this.name = "LlmTransportError";
  }
}

export interface LlmCallOptions {
  timeoutMs?: number;
  /** Aborts the call when the decision it belongs to is no longer current. */
  signal?: AbortSignal;
}

/** A provider response, as received. Status travels WITH the body because the
 *  body alone cannot be judged: an error page or a gateway failure can parse as
 *  a completion envelope, so only the engine, holding both, can rule. */
export interface LlmResponse {
  status: number;
  body: string;
}

/**
 * Perform one provider call.
 *
 * A non-2xx response is NOT thrown away: providers put their most useful
 * diagnostics (bad key, unknown model, rate limit) in the error body, and the
 * engine's response parser surfaces them. The status is returned alongside so
 * the engine can refuse the response regardless of how its body parses — this
 * transport deliberately does not make that call itself. Only a transport
 * failure with no body throws.
 */
export async function executeLlmRequest(
  spec: LlmHttpRequestSpec,
  options: LlmCallOptions = {},
): Promise<LlmResponse> {
  const timeoutMs = options.timeoutMs ?? LLM_REQUEST_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const onAbort = () => controller.abort();
  options.signal?.addEventListener("abort", onAbort);

  try {
    // A decision that is already stale must not open a socket at all. Without
    // this, an abort raised before the call is made is only noticed after the
    // request has gone out — the provider is billed and the reply discarded.
    if (options.signal?.aborted) {
      throw new LlmTransportError("cancelled", "LLM request cancelled");
    }
    const response = await fetch(spec.url, {
      method: spec.method,
      headers: Object.fromEntries(spec.headers.map((header) => [header.name, header.value])),
      body: spec.body,
      signal: controller.signal,
      // Never attach the player's cookies to a third-party AI endpoint.
      credentials: "omit",
      cache: "no-store",
    });
    const text = await readCappedText(response, LLM_MAX_RESPONSE_BYTES);
    if (!text) {
      throw new LlmTransportError(
        "emptyBody",
        `LLM endpoint returned an empty body (HTTP ${response.status})`,
        response.status,
      );
    }
    return { status: response.status, body: text };
  } catch (error) {
    if (error instanceof LlmTransportError) throw error;
    if (error instanceof DOMException && error.name === "AbortError") {
      throw options.signal?.aborted
        ? new LlmTransportError("cancelled", "LLM request cancelled")
        : new LlmTransportError("timedOut", `LLM request timed out after ${timeoutMs}ms`);
    }
    // A CORS rejection and a DNS failure are indistinguishable to `fetch`, so
    // the message names both rather than guessing.
    throw new LlmTransportError(
      "unreachable",
      `Could not reach the LLM endpoint (network error, or the provider does not allow browser requests): ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", onAbort);
  }
}

/**
 * Read a response body, refusing anything past `maxBytes`.
 *
 * Streams through the body reader so an oversized response is abandoned as soon
 * as the budget is exceeded, rather than after it has been buffered in full.
 * Falls back to `response.text()` only where the stream API is unavailable (an
 * environment without `Response.body`, which includes some test doubles); the
 * length is still checked there, just after the fact.
 */
async function readCappedText(response: Response, maxBytes: number): Promise<string> {
  const body = response.body;
  if (!body?.getReader) {
    const text = await response.text();
    if (byteLength(text) > maxBytes) throw oversized(maxBytes);
    return text;
  }

  const reader = body.getReader();
  const decoder = new TextDecoder();
  const chunks: string[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBytes) throw oversized(maxBytes);
      chunks.push(decoder.decode(value, { stream: true }));
    }
  } finally {
    // Releases the socket whether the read completed, overran, or aborted.
    reader.cancel().catch(() => {});
  }
  chunks.push(decoder.decode());
  return chunks.join("");
}

function oversized(maxBytes: number): LlmTransportError {
  return new LlmTransportError(
    "oversized",
    `LLM response exceeded ${maxBytes} bytes; the endpoint is not returning a chat completion`,
  );
}

function byteLength(text: string): number {
  return new TextEncoder().encode(text).byteLength;
}
