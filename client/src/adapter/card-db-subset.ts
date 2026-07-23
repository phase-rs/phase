/**
 * AI-worker card-database loading strategy.
 *
 * iOS PvE worker pools spin up several WASM engine instances. If each fetched
 * and parsed the full ~93MB card-data corpus they would OOM WebKit. The MAIN
 * engine worker keeps the full database; the AI pool instead loads a
 * game-scoped subset built by the engine (`build_ai_card_subset`). Games whose
 * card universe is not statically bounded (today: Momir) do NOT escalate the
 * full database onto every pool worker — each full copy costs ~380MB of WASM
 * linear memory, measured at ~3GB total on a desktop with a 4-worker pool.
 * Instead the caller drops the pool for that game and the single main worker
 * (which already holds the full corpus) serves the AI.
 */
import type { EngineWorkerClient } from "./engine-worker-client";
import type { AiWorkerPool } from "./ai-worker-pool";
import type { AiCardSubsetResult } from "./types";

export type AiCardDataMode = "auto" | "subset" | "full";
export const DEFAULT_AI_CARD_DATA_MODE: AiCardDataMode = "auto";

/**
 * This game's AI-pool card-data resolution, decided WITHOUT touching a pool:
 * - `full`: the user explicitly forced full-corpus workers (opt-in memory cost).
 * - `subset`: the engine bounded this game's universe; `json` is the corpus.
 * - `unbounded`: the universe cannot be statically bounded (e.g. Momir). A
 *   pool must not carry this game — the caller skips creating one (or
 *   disposes an existing one) and the main worker's full DB serves the AI.
 */
export type AiPoolCardDbPlan =
  | { kind: "full" }
  | { kind: "subset"; json: string }
  | { kind: "unbounded" };

/**
 * Resolve this game's AI-pool card-data plan according to `mode`.
 *
 * INVARIANT: `mainEngine` is the MAIN `EngineWorkerClient` (full CARD_DB + live
 * GAME_STATE). `buildAiCardSubset()` is called ONLY here, on `mainEngine` —
 * never on a pool worker (pool workers carry only the subset and may have no
 * game state).
 *
 * Split into resolve (decision, main engine only) and apply (execution, pool
 * only) so callers can decide whether a pool should EXIST before paying to
 * spawn its workers — an unbounded game never creates one.
 */
export async function resolveAiPoolCardDbPlan(
  mode: AiCardDataMode,
  mainEngine: EngineWorkerClient,
): Promise<AiPoolCardDbPlan> {
  if (mode === "full") return { kind: "full" };
  const result: AiCardSubsetResult = JSON.parse(
    await mainEngine.buildAiCardSubset(),
  );
  if (result.kind === "full") return { kind: "unbounded" };
  return { kind: "subset", json: result.json };
}

/** Load a resolved plan into a live pool. `unbounded` is unrepresentable
 *  here by construction — a pool must never carry an unbounded game. */
export async function applyAiPoolCardDbPlan(
  plan: Exclude<AiPoolCardDbPlan, { kind: "unbounded" }>,
  aiPool: AiWorkerPool,
): Promise<void> {
  if (plan.kind === "full") {
    await aiPool.loadCardDb();
    return;
  }
  await aiPool.loadCardDbText(plan.json);
}
