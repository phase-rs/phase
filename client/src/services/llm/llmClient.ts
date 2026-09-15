/**
 * The LLM transport: execute the HTTP call the engine described, return the raw
 * body.
 *
 * This is the whole of the display layer's involvement in an LLM decision. It
 * does not build the request, does not read the response, and does not decide
 * what a failure means — it only moves bytes and enforces a wall-clock bound so
 * a hung provider cannot stall a turn.
 */

import type { LlmHttpRequestSpec } from "./types";

/**
 * Per-call ceiling. A decision that has not come back by now is abandoned and
 * the seat falls back to the heuristic AI, which is why this can be generous
 * enough for a reasoning model without risking a stuck game.
 */
export const LLM_REQUEST_TIMEOUT_MS = 45_000;

/** A transport-level failure, distinct from the engine's `LlmError` outcomes. */
export class LlmTransportError extends Error {
  constructor(
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

/**
 * Perform one provider call.
 *
 * A non-2xx response is NOT thrown away: providers put their most useful
 * diagnostics (bad key, unknown model, rate limit) in the error body, and the
 * engine's response parser surfaces them. The body is returned for any status
 * that carries one; only a transport failure with no body throws.
 */
export async function executeLlmRequest(
  spec: LlmHttpRequestSpec,
  options: LlmCallOptions = {},
): Promise<string> {
  const timeoutMs = options.timeoutMs ?? LLM_REQUEST_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const onAbort = () => controller.abort();
  options.signal?.addEventListener("abort", onAbort);

  try {
    const response = await fetch(spec.url, {
      method: spec.method,
      headers: Object.fromEntries(spec.headers.map((header) => [header.name, header.value])),
      body: spec.body,
      signal: controller.signal,
      // Never attach the player's cookies to a third-party AI endpoint.
      credentials: "omit",
      cache: "no-store",
    });
    const text = await response.text();
    if (!text) {
      throw new LlmTransportError(
        `LLM endpoint returned an empty body (HTTP ${response.status})`,
        response.status,
      );
    }
    return text;
  } catch (error) {
    if (error instanceof LlmTransportError) throw error;
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new LlmTransportError(
        options.signal?.aborted
          ? "LLM request cancelled"
          : `LLM request timed out after ${timeoutMs}ms`,
      );
    }
    // A CORS rejection and a DNS failure are indistinguishable to `fetch`, so
    // the message names both rather than guessing.
    throw new LlmTransportError(
      `Could not reach the LLM endpoint (network error, or the provider does not allow browser requests): ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", onAbort);
  }
}
