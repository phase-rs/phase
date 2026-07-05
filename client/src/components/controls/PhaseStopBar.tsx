import type { Phase, PhaseStopScope } from "../../adapter/types";
import { useId, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

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

// i18n key suffix per phase, used to look up the localized label/description
// from the `phaseStop` group in game.json (e.g. `phaseStop.untapLabel`).
const PHASE_KEY: Record<Phase, string> = {
  Untap: "untap",
  Upkeep: "upkeep",
  Draw: "draw",
  PreCombatMain: "preCombatMain",
  BeginCombat: "beginCombat",
  DeclareAttackers: "declareAttackers",
  DeclareBlockers: "declareBlockers",
  CombatDamage: "combatDamage",
  EndCombat: "endCombat",
  PostCombatMain: "postCombatMain",
  End: "end",
  Cleanup: "cleanup",
};

type PhaseTranslate = ReturnType<typeof useTranslation>["t"];

const SCOPE_TOOLTIP_KEY: Record<PhaseStopScope, string> = {
  AllTurns: "phaseStop.scopeAllTurns",
  OwnTurn: "phaseStop.scopeOwnTurn",
  OpponentsTurns: "phaseStop.scopeOpponentsTurns",
};

function getPhaseTooltip(
  t: PhaseTranslate,
  phase: Phase,
  scope: PhaseStopScope | undefined,
  isActive: boolean,
): string {
  const key = PHASE_KEY[phase];
  return t("phaseStop.tooltip", {
    label: t(`phaseStop.${key}Label`),
    description: t(`phaseStop.${key}Description`),
    stopText: scope ? t(SCOPE_TOOLTIP_KEY[scope]) : t("phaseStop.tooltipNoStop"),
    activeText: isActive ? ` ${t("phaseStop.tooltipCurrentPhase")}` : "",
  });
}

function PhaseDot({ phase }: { phase: Phase }) {
  const { t } = useTranslation("game");
  const tooltipId = useId();
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  const setPhaseStops = usePreferencesStore((s) => s.setPhaseStops);

  const isActive = phase === currentPhase;
  const stop = phaseStops.find((s) => s.phase === phase);
  const hasStop = stop !== undefined;
  const tooltip = getPhaseTooltip(t, phase, stop?.scope, isActive);

  // Cycle the stop for this phase: off → AllTurns → OwnTurn → OpponentsTurns → off.
  // Update in place so array order is preserved — `usePhaseStopsSync` dedupes by
  // positional comparison, so appending would reorder and force a redundant
  // engine dispatch even when the set of stops is unchanged.
  const cyclePhase = () => {
    if (stop === undefined) {
      setPhaseStops([...phaseStops, { phase, scope: "AllTurns" }]);
      return;
    }
    const next: PhaseStopScope | null =
      stop.scope === "AllTurns"
        ? "OwnTurn"
        : stop.scope === "OwnTurn"
          ? "OpponentsTurns"
          : null; // OpponentsTurns → off
    setPhaseStops(
      next === null
        ? phaseStops.filter((s) => s.phase !== phase)
        : phaseStops.map((s) => (s.phase === phase ? { phase, scope: next } : s)),
    );
  };

  return (
    <button
      type="button"
      onClick={cyclePhase}
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
      <GameplayTooltip id={tooltipId}>
        {tooltip}
      </GameplayTooltip>
    </button>
  );
}

/** Upkeep, Draw, Main1 — placed to the left of the player avatar.
 *  Hidden on mobile (<lg) where the dots are too small to tap and crowd the HUD. */
export function PhaseIndicatorLeft() {
  return (
    <div className="hidden items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/58 px-1 py-1 backdrop-blur-xl lg:flex lg:px-1.5">
      {LEFT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** Main2, End — placed to the right of the player avatar.
 *  Hidden on mobile (<lg) where the dots are too small to tap and crowd the HUD. */
export function PhaseIndicatorRight() {
  return (
    <div className="hidden items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/58 px-1 py-1 backdrop-blur-xl lg:flex lg:px-1.5">
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
    <div
      data-combat-phase-indicator
      className="flex items-center gap-0.5 rounded-full border border-white/10 bg-slate-950/64 px-1 py-1 backdrop-blur-xl lg:px-1.5"
    >
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
