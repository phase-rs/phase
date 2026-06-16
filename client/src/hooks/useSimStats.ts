/**
 * Hook for accessing Monte Carlo simulation results from the playtest store.
 * Provides derived chart data and running-state signals.
 */

import { usePlaytestStore } from "../stores/playtestStore";
import type { SimulationResult, SimulationConfig } from "../services/playtestSession";

export interface SimStatsHook {
  result: SimulationResult | null;
  isSimulating: boolean;
  config: SimulationConfig;
  setConfig: (config: Partial<SimulationConfig>) => void;
  runSimulation: (config?: Partial<SimulationConfig>) => Promise<void>;

  // ── Chart-ready data ───────────────────────────────────────────────────────
  /** X labels for turn axis: ["T1", "T2", …] */
  turnLabels: string[];
  /** Average available mana per turn. */
  avgManaData: number[];
  /** Average hand size per turn. */
  avgHandData: number[];
  /** Average lands in play per turn. */
  avgLandsData: number[];
  /** Average playable-card count per turn. */
  avgPlayableData: number[];
  /** Stddev bands for mana chart (lower, upper). */
  manaLowerBand: number[];
  manaUpperBand: number[];

  // ── Opening hand summary ───────────────────────────────────────────────────
  avgOpeningLands: number;
  avgOpeningSpells: number;
  avgMulligans: number;
  pctKeepFirst: number;
  gamesSimulated: number;
}

export function useSimStats(): SimStatsHook {
  const { simulationResult, simulationConfig, phase, runSimulation, setSimulationConfig } =
    usePlaytestStore();

  const turns = simulationResult?.turns ?? [];

  const turnLabels = turns.map((t) => `T${t.turnNumber}`);
  const avgManaData = turns.map((t) => Number(t.avgAvailableMana.toFixed(2)));
  const avgHandData = turns.map((t) => Number(t.avgHandSize.toFixed(2)));
  const avgLandsData = turns.map((t) => Number(t.avgLandsInPlay.toFixed(2)));
  const avgPlayableData = turns.map((t) => Number(t.avgPlayableCount.toFixed(2)));
  const manaLowerBand = turns.map((t) =>
    Number(Math.max(0, t.avgAvailableMana - t.stddevAvailableMana).toFixed(2)),
  );
  const manaUpperBand = turns.map((t) =>
    Number((t.avgAvailableMana + t.stddevAvailableMana).toFixed(2)),
  );

  const oh = simulationResult?.openingHand;

  return {
    result: simulationResult,
    isSimulating: phase === "simulating",
    config: simulationConfig,
    setConfig: setSimulationConfig,
    runSimulation,
    turnLabels,
    avgManaData,
    avgHandData,
    avgLandsData,
    avgPlayableData,
    manaLowerBand,
    manaUpperBand,
    avgOpeningLands: oh?.avgLands ?? 0,
    avgOpeningSpells: oh?.avgSpells ?? 0,
    avgMulligans: oh?.avgMulligans ?? 0,
    pctKeepFirst: oh?.pctKeepFirst ?? 0,
    gamesSimulated: simulationResult?.gamesSimulated ?? 0,
  };
}
