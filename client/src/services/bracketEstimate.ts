import { getSharedAdapter } from "../adapter/wasm-adapter";
import type { BracketEstimate, BracketEstimateRequest } from "../types/bracketEstimate";

/**
 * Outcome of one bracket-estimate request. A typed sum rather than
 * `BracketEstimate | null` plus a boolean, because the engine's `Option::None`
 * has two causes with different meanings for the player — it carries no
 * commander, or the card database was not readable — and the panel renders a
 * different sentence for each.
 */
export type BracketEstimateOutcome =
  | { kind: "estimate"; estimate: BracketEstimate }
  | { kind: "no-commander" }
  | { kind: "card-data-unavailable"; reason: string };

/**
 * Engine-computed bracket reading for a decklist. Pure and stateless on the
 * engine side — a CARD_DB read with no game state — so it routes through the
 * single shared engine worker, exactly like `evaluateDeckCompatibility`.
 * There is deliberately no adapter parameter: bracket estimation is not a
 * transport concern and is not a member of `EngineAdapter`.
 */
export async function estimateDeckBracket(
  request: BracketEstimateRequest,
): Promise<BracketEstimateOutcome> {
  const adapter = getSharedAdapter();
  try {
    const estimate = await adapter.estimateBracket(request);
    return estimate ? { kind: "estimate", estimate } : { kind: "no-commander" };
  } catch (err) {
    // `WasmAdapter.estimateBracket` awaits `requireCardDb()`, which throws with
    // the underlying load failure attached as `cause`; every other failure mode
    // on this path is the same "we could not read card data" answer.
    return {
      kind: "card-data-unavailable",
      reason: err instanceof Error ? err.message : String(err),
    };
  }
}
