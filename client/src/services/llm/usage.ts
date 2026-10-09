/**
 * Measurement of what LLM opponents cost per call: prompt size (from the
 * engine, for every provider) and token usage (from the provider, when it
 * reports any).
 *
 * Console only, by design. The shared game log is rendered back into later
 * prompts (see `diagnostics.ts`), so nothing here may be written to it. The
 * running totals exist so a session's trend — does the prompt stay flat as the
 * game goes on? — reads straight off the console.
 */

import type { LlmTokenUsage } from "./types";

interface UsageTotals {
  calls: number;
  promptChars: number;
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens: number;
}

const totals = new Map<string, UsageTotals>();

function emptyTotals(): UsageTotals {
  return { calls: 0, promptChars: 0, inputTokens: 0, outputTokens: 0, cachedInputTokens: 0 };
}

/**
 * Record one call's prompt size and provider-reported usage under `scope`
 * (`"game"` or `"draft"`), and log it with the scope's running average.
 */
export function recordLlmUsage(
  scope: string,
  label: string,
  promptChars: number | undefined,
  usage: LlmTokenUsage | null | undefined,
): void {
  const running = totals.get(scope) ?? emptyTotals();
  running.calls += 1;
  running.promptChars += promptChars ?? 0;
  running.inputTokens += usage?.inputTokens ?? 0;
  running.outputTokens += usage?.outputTokens ?? 0;
  running.cachedInputTokens += usage?.cachedInputTokens ?? 0;
  totals.set(scope, running);

  const reported = usage?.inputTokens != null
    ? `${usage.inputTokens} in / ${usage.outputTokens ?? "?"} out tokens`
      + (usage.cachedInputTokens ? ` (${usage.cachedInputTokens} cached)` : "")
    : "no token count reported";
  console.info(
    `[LLM ${scope}] ${label}: prompt ${promptChars ?? "?"} chars, ${reported}; `
      + `${running.calls} calls, avg ${Math.round(running.promptChars / running.calls)} chars`,
  );
}

/** Running totals for `scope`, for tests and ad-hoc inspection. */
export function llmUsageTotals(scope: string): Readonly<UsageTotals> {
  return { ...(totals.get(scope) ?? emptyTotals()) };
}

/** Clear the running totals. */
export function resetLlmUsage(): void {
  totals.clear();
}
