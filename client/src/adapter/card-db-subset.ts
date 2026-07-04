/**
 * AI-worker card-database loading strategy.
 *
 * iOS PvE worker pools spin up several WASM engine instances. If each fetched
 * and parsed the full ~93MB card-data corpus they would OOM WebKit. The MAIN
 * engine worker keeps the full database; the AI pool instead loads a
 * game-scoped subset built by the engine (`build_ai_card_subset`). Games whose
 * card universe is not statically bounded (today: Momir) escalate to the full
 * database on AI workers.
 */
import type { EngineWorkerClient } from "./engine-worker-client";
import type { AiWorkerPool } from "./ai-worker-pool";
import type { AiCardSubsetResult } from "./types";

export type AiCardDataMode = "auto" | "subset" | "full";
export const DEFAULT_AI_CARD_DATA_MODE: AiCardDataMode = "auto";

/**
 * Load the AI worker pool's card database according to `mode`.
 *
 * INVARIANT: `mainEngine` is the MAIN `EngineWorkerClient` (full CARD_DB + live
 * GAME_STATE). `buildAiCardSubset()` is called ONLY here, on `mainEngine` —
 * never on a pool worker (pool workers carry only the subset and may have no
 * game state).
 *
 * - `full`: every pool worker fetches+parses the full corpus.
 * - `auto`/`subset`: the engine builds this game's subset on the main worker;
 *   a `full` result (unbounded universe, e.g. Momir) falls back to the full DB.
 */
export async function loadAiPoolCardDb(
  mode: AiCardDataMode,
  mainEngine: EngineWorkerClient,
  aiPool: AiWorkerPool,
): Promise<void> {
  if (mode === "full") {
    await aiPool.loadCardDb();
    return;
  }
  const result: AiCardSubsetResult = JSON.parse(
    await mainEngine.buildAiCardSubset(),
  );
  if (result.kind === "full") {
    await aiPool.loadCardDb();
  } else {
    await aiPool.loadCardDbText(result.json);
  }
}
