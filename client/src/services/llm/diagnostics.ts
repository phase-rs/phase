/**
 * Where LLM failure detail goes.
 *
 * Provider diagnostics are externally authored: a provider, a proxy, or a
 * hostile custom endpoint chooses that string. `debugLog` writes a
 * `visibility: "Public"` entry into `logHistory`, and `logHistory` is the game
 * history fed back into the NEXT prompt — so routing provider text through it
 * would let an endpoint write prose into a later decision's context. Response
 * validation does not defend against that: the text never has to pass as a
 * decision, only as narrative the model reads as history.
 *
 * So the two destinations are kept apart on purpose:
 *
 *  - the game log gets a Phase-authored line naming the seat and nothing else,
 *    so a player can see that a seat fell back;
 *  - the console gets the full detail, because it is a developer surface that
 *    is never rendered into a prompt.
 *
 * Nothing here is a judgement about any particular provider. It is the same
 * rule applied to every externally authored string: it may be shown to a human,
 * never fed back to a model as context.
 */

import { debugLog } from "../../game/debugLog";

/**
 * Report an LLM failure.
 *
 * `summary` must be Phase-authored — it reaches the shared game log. `detail`
 * may be externally authored and goes only to the console.
 */
export function reportLlmFailure(summary: string, detail?: unknown): void {
  if (detail !== undefined) {
    console.warn(`[LLM] ${summary}`, detail);
  }
  debugLog(summary, "warn");
}

/** A developer-facing string for a thrown value. Console only — never logged. */
export function describeLlmError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
