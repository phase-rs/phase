import type { Phase } from "../../adapter/types";
import { useId, type ReactNode } from "react";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

// MTGA-style phase icons as inline SVGs (14x14)
const PHASE_ICONS: Record<Phase, ReactNode> = {
  // Sun — untap
  Untap: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <circle cx="7" cy="7" r="3" />
      <path d="M7 1v2M7 11v2M1 7h2M11 7h2M2.8 2.8l1.4 1.4M9.8 9.8l1.4 1.4M2.8 11.2l1.4-1.4M9.8 4.2l1.4-1.4" stroke="currentColor" strokeWidth="1.2" fill="none" />
    </svg>
  ),
  // Droplet — upkeep
  Upkeep: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M7 1.5C7 1.5 3 6 3 8.5a4 4 0 0 0 8 0C11 6 7 1.5 7 1.5Z" />
    </svg>
  ),
  // Card — draw
  Draw: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <rect x="3" y="2" width="8" height="10" rx="1" />
      <line x1="5" y1="5" x2="9" y2="5" stroke="currentColor" strokeWidth="0.8" opacity="0.4" />
    </svg>
  ),
  // Diamond/gem — main phase 1
  PreCombatMain: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M7 1L12 7L7 13L2 7Z" />
    </svg>
  ),
  // Crossed swords — begin combat
  BeginCombat: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M3 2l8 8M11 2l-8 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />
    </svg>
  ),
  // Upward sword — declare attackers
  DeclareAttackers: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M7 2v9M4.5 4.5L7 2l2.5 2.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" fill="none" />
      <line x1="5" y1="12" x2="9" y2="12" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
    </svg>
  ),
  // Shield — declare blockers
  DeclareBlockers: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M7 1.5L2.5 3.5V7C2.5 10 7 12.5 7 12.5S11.5 10 11.5 7V3.5L7 1.5Z" />
    </svg>
  ),
  // Crossed swords — combat damage
  CombatDamage: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M3 2l8 8M11 2l-8 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" fill="none" />
      <circle cx="7" cy="7" r="1.5" />
    </svg>
  ),
  // Flag — end combat
  EndCombat: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M3.5 2v10" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" fill="none" />
      <path d="M3.5 2H10L8.5 5L10 8H3.5Z" />
    </svg>
  ),
  // Diamond/gem — main phase 2
  PostCombatMain: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M7 1L12 7L7 13L2 7Z" />
    </svg>
  ),
  // Hourglass — end step
  End: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
      <path d="M4 2h6M4 12h6M4.5 2C4.5 5 7 6.5 7 7S4.5 9 4.5 12M9.5 2C9.5 5 7 6.5 7 7S9.5 9 9.5 12" stroke="currentColor" strokeWidth="1.2" fill="none" />
    </svg>
  ),
  // Broom — cleanup
  Cleanup: (
    <svg viewBox="0 0 14 14" className="h-2.5 w-2.5 lg:h-3.5 lg:w-3.5" fill="currentColor">
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

const PHASE_DETAILS: Record<Phase, { label: string; description: string }> = {
  Untap: {
    label: "Untap step",
    description: "Your tapped permanents untap here.",
  },
  Upkeep: {
    label: "Upkeep step",
    description: "Upkeep triggers and recurring costs happen here.",
  },
  Draw: {
    label: "Draw step",
    description: "You draw for turn here.",
  },
  PreCombatMain: {
    label: "First main phase",
    description: "Play lands and cast spells before combat.",
  },
  BeginCombat: {
    label: "Beginning of combat step",
    description: "Last priority window before attackers are chosen.",
  },
  DeclareAttackers: {
    label: "Declare attackers step",
    description: "The attacking player chooses attackers.",
  },
  DeclareBlockers: {
    label: "Declare blockers step",
    description: "Defending players choose blockers.",
  },
  CombatDamage: {
    label: "Combat damage step",
    description: "Creatures assign and deal combat damage.",
  },
  EndCombat: {
    label: "End of combat step",
    description: "Last combat priority window after damage.",
  },
  PostCombatMain: {
    label: "Second main phase",
    description: "Play lands or cast spells after combat.",
  },
  End: {
    label: "End step",
    description: "End-step triggers and end-of-turn actions happen here.",
  },
  Cleanup: {
    label: "Cleanup step",
    description: "Damage wears off and maximum hand size is checked.",
  },
};

function getPhaseTooltip(phase: Phase, hasStop: boolean, isActive: boolean): string {
  const details = PHASE_DETAILS[phase];
  const stopText = hasStop
    ? "Stop set: click to remove this auto-pass stop."
    : "No stop set: click to pause auto-pass here.";
  const activeText = isActive ? " Current phase." : "";
  return `Phase stop: ${details.label}. ${details.description} ${stopText}${activeText}`;
}

function PhaseDot({ phase }: { phase: Phase }) {
  const tooltipId = useId();
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  const setPhaseStops = usePreferencesStore((s) => s.setPhaseStops);

  const isActive = phase === currentPhase;
  const hasStop = phaseStops.includes(phase);
  const tooltip = getPhaseTooltip(phase, hasStop, isActive);

  const togglePhase = () => {
    if (hasStop) {
      setPhaseStops(phaseStops.filter((p) => p !== phase));
    } else {
      setPhaseStops([...phaseStops, phase]);
    }
  };

  return (
    <button
      type="button"
      onClick={togglePhase}
      aria-label={tooltip}
      aria-describedby={tooltipId}
      aria-pressed={hasStop}
      className={`group relative flex h-6 w-6 items-center justify-center rounded-full border transition-all duration-200 lg:h-8 lg:w-8 lg:p-1 ${
        isActive
          ? "border-cyan-300/45 bg-cyan-400/18 text-white shadow-[0_10px_22px_rgba(34,211,238,0.22)]"
          : hasStop
            ? "border-white/12 bg-white/8 text-slate-200 hover:border-white/20 hover:text-white"
            : "border-transparent bg-transparent text-slate-500 hover:border-white/10 hover:bg-white/5 hover:text-slate-200"
      }`}
    >
      {isActive && (
        <span className="absolute -top-1 left-1/2 h-1.5 w-1.5 -translate-x-1/2 rounded-full bg-amber-300 shadow-[0_0_10px_rgba(252,211,77,0.9)]" />
      )}
      {PHASE_ICONS[phase]}
      {hasStop && (
        <span className="absolute -bottom-0.5 left-1/2 h-1 w-1 -translate-x-1/2 rounded-full bg-amber-400" />
      )}
      <span
        id={tooltipId}
        role="tooltip"
        className="pointer-events-none absolute right-0 bottom-full z-50 mb-2 hidden w-64 rounded-md border border-white/10 bg-slate-950/95 px-3 py-2 text-left text-[11px] leading-snug font-medium text-slate-100 shadow-2xl shadow-black/40 backdrop-blur-xl group-hover:block group-focus-visible:block"
      >
        {tooltip}
      </span>
    </button>
  );
}

/** Upkeep, Draw, Main1 — placed to the left of the player avatar */
export function PhaseIndicatorLeft() {
  return (
    <div className="flex items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/58 px-1 py-1 backdrop-blur-xl lg:px-1.5">
      {LEFT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** Main2, End — placed to the right of the player avatar */
export function PhaseIndicatorRight() {
  return (
    <div className="flex items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/58 px-1 py-1 backdrop-blur-xl lg:px-1.5">
      {RIGHT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** BeginCombat through EndCombat — placed near ActionButton on the right side */
export function CombatPhaseIndicator() {
  const isCompactHeight = useIsCompactHeight();
  // Hide on landscape phones — non-essential and eats horizontal real estate
  // next to the ActionButton. The phase pills along the top still convey phase.
  if (isCompactHeight) return null;
  return (
    <div className="flex items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/64 px-1 py-1 backdrop-blur-xl lg:px-1.5">
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
