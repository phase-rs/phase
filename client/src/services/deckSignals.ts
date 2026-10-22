import { getSharedAdapter } from "../adapter/wasm-adapter";
import type { BracketDeckRequest } from "../types/bracket";
import type { DeckSignals } from "../types/deckSignals";

export type DeckSignalsOutcome =
  | { kind: "signals"; signals: DeckSignals }
  | { kind: "no-commander" }
  | { kind: "card-data-unavailable"; reason: string };

/** Read engine-computed structural evidence for a deck through the shared WASM adapter. */
export async function getDeckSignals(deck: BracketDeckRequest): Promise<DeckSignalsOutcome> {
  const adapter = getSharedAdapter();
  try {
    const signals = await adapter.deckSignals(deck);
    return signals ? { kind: "signals", signals } : { kind: "no-commander" };
  } catch (err) {
    return {
      kind: "card-data-unavailable",
      reason: err instanceof Error ? err.message : String(err),
    };
  }
}
