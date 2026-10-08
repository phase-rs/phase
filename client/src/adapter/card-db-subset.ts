/** Card-data planning for memory-safe parallel AI workers. */
import type { AiWorkerPool } from "./ai-worker-pool";
import type { EngineWorkerClient } from "./engine-worker-client";
import type { AiCardSubsetResult } from "./types";

export type AiCardDataMode = "auto" | "subset" | "full";
export const DEFAULT_AI_CARD_DATA_MODE: AiCardDataMode = "auto";

export type AiPoolCardDbPlan =
  | { kind: "full" }
  | { kind: "subset"; json: string }
  | { kind: "unbounded" };

/** Main-engine only: resolve the bounded subset before a pool is created. */
export async function resolveAiPoolCardDbPlan(
  mode: AiCardDataMode,
  mainEngine: EngineWorkerClient,
): Promise<AiPoolCardDbPlan> {
  if (mode === "full") return { kind: "full" };
  const result: AiCardSubsetResult = JSON.parse(await mainEngine.buildAiCardSubset());
  return result.kind === "full"
    ? { kind: "unbounded" }
    : { kind: "subset", json: result.json };
}

export async function applyAiPoolCardDbPlan(
  plan: Exclude<AiPoolCardDbPlan, { kind: "unbounded" }>,
  pool: AiWorkerPool,
): Promise<void> {
  if (plan.kind === "full") {
    await pool.loadCardDb();
  } else {
    await pool.loadCardDbText(plan.json);
  }
}
