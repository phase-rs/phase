/**
 * "Test connection" — one real request, validated by the engine.
 *
 * Runs the exact path a game decision runs: the engine builds the request from
 * the player's endpoint config, the transport performs it, and the engine
 * extracts and decodes the reply. That matters because the transport
 * deliberately returns non-2xx bodies rather than rejecting, so a vendor's
 * error message survives to be shown — which makes "the fetch resolved" a
 * meaningless success signal. A rejected key and an unknown model both come
 * back as well-formed HTTP responses.
 */

import { ensureWasmInit } from "../engineRuntime";
import { executeLlmRequest, LlmTransportError } from "./llmClient";
import {
  endpointOf,
  type LlmFailure,
  type LlmHttpRequestSpec,
  type LlmProfile,
} from "./types";

/** Bound for a probe. Shorter than a real decision: a player is watching it. */
export const LLM_PROBE_TIMEOUT_MS = 20_000;

export type LlmProbeResult = { ok: true } | ({ ok: false } & LlmFailure);

interface ProbeRequestResult {
  request?: LlmHttpRequestSpec;
  error?: string;
}

interface ProbeValidation {
  ok?: boolean;
  error?: string;
}

export async function testLlmEndpoint(
  profile: LlmProfile,
  options: { timeoutMs?: number; signal?: AbortSignal } = {},
): Promise<LlmProbeResult> {
  let wasm: typeof import("@wasm/engine");
  try {
    await ensureWasmInit();
    wasm = await import("@wasm/engine");
  } catch (error) {
    return { ok: false, code: "engineUnavailable", detail: describe(error) };
  }

  // The engine refuses here for a missing model, a missing key, or a credential
  // bound for a plaintext endpoint — before anything reaches the network.
  const built = wasm.buildLlmProbeRequest(JSON.stringify(endpointOf(profile))) as ProbeRequestResult;
  if (!built?.request) {
    // `built.error` is the ENGINE's own refusal (no model, missing key, a
    // credential bound for a plaintext endpoint). It is data, shown verbatim.
    return { ok: false, code: "requestNotBuilt", detail: built?.error };
  }

  let response: { status: number; body: string };
  try {
    response = await executeLlmRequest(built.request, {
      timeoutMs: options.timeoutMs ?? LLM_PROBE_TIMEOUT_MS,
      // Forwarded so a probe for an endpoint the player has since edited,
      // removed, or navigated away from is cut loose instead of running to its
      // timeout against a configuration that no longer exists.
      signal: options.signal,
    });
  } catch (error) {
    if (error instanceof LlmTransportError) {
      return { ok: false, code: error.code, detail: undefined };
    }
    return { ok: false, code: "unreachable", detail: describe(error) };
  }

  // Status travels with the body: a non-2xx reply is refused however it parses.
  const verdict = wasm.validateLlmProbeResponse(
    profile.provider,
    response.status,
    response.body,
  ) as ProbeValidation;
  if (verdict?.ok) return { ok: true };
  // The vendor's own message ("Incorrect API key provided", "models/x is not
  // found") is the most useful half and survives untranslated.
  return { ok: false, code: "undecodable", detail: verdict?.error };
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
