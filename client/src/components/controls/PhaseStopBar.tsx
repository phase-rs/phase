import type { Phase } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

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

const LEFT_PHASES: Phase[] = ["Upkeep", "Draw", "PreCombatMain"];
const RIGHT_PHASES: Phase[] = ["PostCombatMain", "End"];
const COMBAT_PHASES: Phase[] = [
  "BeginCombat",
  "DeclareAttackers",
  "DeclareBlockers",
  "CombatDamage",
  "EndCombat",
];

function PhaseDot({ phase }: { phase: Phase }) {
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  const setPhaseStops = usePreferencesStore((s) => s.setPhaseStops);

  const isActive = phase === currentPhase;
  const hasStop = phaseStops.includes(phase);

  const togglePhase = () => {
    if (hasStop) {
      setPhaseStops(phaseStops.filter((p) => p !== phase));
    } else {
      setPhaseStops([...phaseStops, phase]);
    }
  };

  return (
    <button
      onClick={togglePhase}
      title={phase}
      className={`relative rounded-full px-1.5 py-0.5 text-[10px] font-medium transition-colors ${
        isActive
          ? "text-white shadow-[0_0_6px_rgba(34,211,238,0.5)]"
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
}

/** Upkeep, Draw, Main1 — placed to the left of the player avatar */
export function PhaseIndicatorLeft() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-2 py-0.5">
      {LEFT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** Main2, End — placed to the right of the player avatar */
export function PhaseIndicatorRight() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-2 py-0.5">
      {RIGHT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** BeginCombat through EndCombat — placed near ActionButton on the right side */
export function CombatPhaseIndicator() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-2 py-0.5">
      {COMBAT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** @deprecated Use PhaseIndicatorLeft, PhaseIndicatorRight, CombatPhaseIndicator instead */
export function PhaseStopBar() {
  return (
    <div className="flex items-center gap-1">
      <PhaseIndicatorLeft />
      <CombatPhaseIndicator />
      <PhaseIndicatorRight />
    </div>
  );
}
