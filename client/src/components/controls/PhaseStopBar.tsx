import type { Phase } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

const ALL_PHASES: Phase[] = [
  "Untap",
  "Upkeep",
  "Draw",
  "PreCombatMain",
  "BeginCombat",
  "DeclareAttackers",
  "DeclareBlockers",
  "CombatDamage",
  "EndCombat",
  "PostCombatMain",
  "End",
  "Cleanup",
];

const PHASE_LABELS: Record<Phase, string> = {
  Untap: "UT",
  Upkeep: "UP",
  Draw: "DR",
  PreCombatMain: "M1",
  BeginCombat: "BC",
  DeclareAttackers: "DA",
  DeclareBlockers: "DB",
  CombatDamage: "CD",
  EndCombat: "EC",
  PostCombatMain: "M2",
  End: "EN",
  Cleanup: "CL",
};

export function PhaseStopBar() {
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  const setPhaseStops = usePreferencesStore((s) => s.setPhaseStops);

  const togglePhase = (phase: Phase) => {
    if (phaseStops.includes(phase)) {
      setPhaseStops(phaseStops.filter((p) => p !== phase));
    } else {
      setPhaseStops([...phaseStops, phase]);
    }
  };

  return (
    <div className="flex items-center gap-0.5">
      {ALL_PHASES.map((phase) => {
        const isActive = phase === currentPhase;
        const hasStop = phaseStops.includes(phase);

        return (
          <button
            key={phase}
            onClick={() => togglePhase(phase)}
            title={phase}
            className={`relative px-1.5 py-0.5 text-[10px] font-medium transition-colors ${
              isActive
                ? "border-b-2 border-cyan-400 text-white"
                : hasStop
                  ? "text-gray-300 hover:text-white"
                  : "text-gray-600 hover:text-gray-400"
            }`}
          >
            {PHASE_LABELS[phase]}
            {hasStop && (
              <span className="absolute -bottom-1 left-1/2 h-1 w-1 -translate-x-1/2 rounded-full bg-amber-400" />
            )}
          </button>
        );
      })}
    </div>
  );
}
