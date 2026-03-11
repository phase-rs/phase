import type { Phase } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { usePlayerId } from "./usePlayerId.ts";

export type PhaseDisplayKey = "draw" | "main1" | "combat" | "main2" | "end";

export interface PhaseStripEntry {
  key: PhaseDisplayKey;
  label: string;
  order: number;
}

export interface PhaseInfo {
  displayKey: PhaseDisplayKey;
  currentOrder: number;
  phaseLabel: string;
  phases: readonly PhaseStripEntry[];
  advanceLabel: string;
  isCombatPhase: boolean;
}

const PHASE_STRIP: readonly PhaseStripEntry[] = [
  { key: "draw", label: "Draw", order: 0 },
  { key: "main1", label: "Main 1", order: 1 },
  { key: "combat", label: "Combat", order: 2 },
  { key: "main2", label: "Main 2", order: 3 },
  { key: "end", label: "End", order: 4 },
] as const;

const PHASE_TO_DISPLAY: Record<Phase, PhaseDisplayKey> = {
  Untap: "draw",
  Upkeep: "draw",
  Draw: "draw",
  PreCombatMain: "main1",
  BeginCombat: "combat",
  DeclareAttackers: "combat",
  DeclareBlockers: "combat",
  CombatDamage: "combat",
  EndCombat: "combat",
  PostCombatMain: "main2",
  End: "end",
  Cleanup: "end",
};

const PHASE_LABELS: Record<Phase, string> = {
  Untap: "Untap",
  Upkeep: "Upkeep",
  Draw: "Draw",
  PreCombatMain: "Main Phase 1",
  BeginCombat: "Begin Combat",
  DeclareAttackers: "Declare Attackers",
  DeclareBlockers: "Declare Blockers",
  CombatDamage: "Combat Damage",
  EndCombat: "End Combat",
  PostCombatMain: "Main Phase 2",
  End: "End Step",
  Cleanup: "Cleanup",
};

const DISPLAY_ORDER: Record<PhaseDisplayKey, number> = {
  draw: 0,
  main1: 1,
  combat: 2,
  main2: 3,
  end: 4,
};

const COMBAT_PHASES = new Set<Phase>([
  "BeginCombat",
  "DeclareAttackers",
  "DeclareBlockers",
  "CombatDamage",
  "EndCombat",
]);

function getAdvanceLabel(phase: Phase, hasStackItems: boolean, isMyTurn: boolean): string {
  if (hasStackItems) return "Resolve";
  if (isMyTurn && phase === "PreCombatMain") return "To Combat";
  return "Next";
}

export function usePhaseInfo(): PhaseInfo {
  const phase = useGameStore((s) => s.gameState?.phase ?? "Untap");
  const stackLength = useGameStore((s) => s.gameState?.stack.length ?? 0);
  const activePlayer = useGameStore((s) => s.gameState?.active_player ?? 0);
  const playerId = usePlayerId();
  const isMyTurn = activePlayer === playerId;

  const displayKey = PHASE_TO_DISPLAY[phase];
  const currentOrder = DISPLAY_ORDER[displayKey];
  const isCombatPhase = COMBAT_PHASES.has(phase);
  const advanceLabel = getAdvanceLabel(phase, stackLength > 0, isMyTurn);

  return {
    displayKey,
    currentOrder,
    phaseLabel: PHASE_LABELS[phase],
    phases: PHASE_STRIP,
    advanceLabel,
    isCombatPhase,
  };
}
