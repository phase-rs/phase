import type { Phase } from "../../adapter/types";
import type { ReactNode } from "react";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

// MTGA-style phase icons as inline SVGs (14x14)
const PHASE_ICONS: Record<Phase, ReactNode> = {
  // Sun — untap
  Untap: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <circle cx="7" cy="7" r="3" />
      <path d="M7 1v2M7 11v2M1 7h2M11 7h2M2.8 2.8l1.4 1.4M9.8 9.8l1.4 1.4M2.8 11.2l1.4-1.4M9.8 4.2l1.4-1.4" stroke="currentColor" strokeWidth="1.2" fill="none" />
    </svg>
  ),
  // Droplet — upkeep
  Upkeep: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M7 1.5C7 1.5 3 6 3 8.5a4 4 0 0 0 8 0C11 6 7 1.5 7 1.5Z" />
    </svg>
  ),
  // Card — draw
  Draw: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <rect x="3" y="2" width="8" height="10" rx="1" />
      <line x1="5" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="0.8" opacity="0.4" />
    </svg>
  ),
  // Diamond/gem — main phase 1
  PreCombatMain: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M7 1L12 7L7 13L2 7Z" />
    </svg>
  ),
  // Crossed swords — begin combat
  BeginCombat: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M3 2l8 8M11 2l-8 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />
    </svg>
  ),
  // Upward sword — declare attackers
  DeclareAttackers: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M7 2v9M4.5 4.5L7 2l2.5 2.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" fill="none" />
      <line x1="5" y1="12" x2="9" y2="12" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  ),
  // Shield — declare blockers
  DeclareBlockers: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M7 1.5L2.5 3.5V7C2.5 10 7 12.5 7 12.5S11.5 10 11.5 7V3.5L7 1.5Z" />
    </svg>
  ),
  // Crossed swords — combat damage
  CombatDamage: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M3 2l8 8M11 2l-8 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />
      <circle cx="7" cy="7" r="1.5" />
    </svg>
  ),
  // Flag — end combat
  EndCombat: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M3.5 2v10" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" fill="none" />
      <path d="M3.5 2H10L8.5 5L10 8H3.5Z" />
    </svg>
  ),
  // Diamond/gem — main phase 2
  PostCombatMain: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M7 1L12 7L7 13L2 7Z" />
    </svg>
  ),
  // Hourglass — end step
  End: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <path d="M4 2h6M4 12h6M4.5 2C4.5 5 7 6.5 7 7S4.5 9 4.5 12M9.5 2C9.5 5 7 6.5 7 7S9.5 9 9.5 12" stroke="currentColor" strokeWidth="1.2" fill="none" />
    </svg>
  ),
  // Broom — cleanup
  Cleanup: (
    <svg viewBox="0 0 14 14" className="h-3.5 w-3.5" fill="currentColor">
      <circle cx="7" cy="4" r="2.5" />
      <path d="M5.5 6.5L4 12h6l-1.5-5.5" />
    </svg>
  ),
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
      className={`relative flex h-8 w-8 items-center justify-center rounded-full transition-colors sm:h-auto sm:w-auto sm:p-1 ${
        isActive
          ? "text-white shadow-[0_0_6px_rgba(34,211,238,0.5)]"
          : hasStop
            ? "text-gray-300 hover:text-white"
            : "text-gray-400 hover:text-gray-200"
      }`}
    >
      {isActive && (
        <span className="absolute -top-1 left-1/2 h-1.5 w-1.5 -translate-x-1/2 rounded-full bg-amber-300 shadow-[0_0_10px_rgba(252,211,77,0.9)]" />
      )}
      {PHASE_ICONS[phase]}
      {hasStop && (
        <span className="absolute -bottom-0.5 left-1/2 h-1 w-1 -translate-x-1/2 rounded-full bg-amber-400" />
      )}
    </button>
  );
}

/** Upkeep, Draw, Main1 — placed to the left of the player avatar */
export function PhaseIndicatorLeft() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-1 py-0.5 sm:px-1.5">
      {LEFT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** Main2, End — placed to the right of the player avatar */
export function PhaseIndicatorRight() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-1 py-0.5 sm:px-1.5">
      {RIGHT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** BeginCombat through EndCombat — placed near ActionButton on the right side */
export function CombatPhaseIndicator() {
  return (
    <div className="flex items-center gap-0.5 rounded-full bg-black/40 px-1 py-0.5 sm:px-1.5">
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
