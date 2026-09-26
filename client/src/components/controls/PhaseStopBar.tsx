import type { Phase, PhaseStop, PhaseStopScope } from "../../adapter/types";
import { useId, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

// MTGA-style phase icons as inline SVGs (14x14). Shared with the mobile
// phase chip/sheet (MobilePhaseChip.tsx), which renders the same glyphs at
// touch-friendly sizes.
export const PHASE_ICONS: Record<Phase, ReactNode> = {
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

const MAJOR_PHASES: Phase[] = [
  "Upkeep",
  "PreCombatMain",
  "BeginCombat",
  "PostCombatMain",
  "End",
];
const MAJOR_PHASES_BY_SIDE: Record<"left" | "right", Phase[]> = {
  left: MAJOR_PHASES.slice(0, 2),
  right: MAJOR_PHASES.slice(2),
};

// i18n key suffix per phase, used to look up the localized label/description
// from the `phaseStop` group in game.json (e.g. `phaseStop.untapLabel`).
export const PHASE_KEY: Record<Phase, string> = {
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

// Scope hue, shared with the mobile sheet's scope pills (MobilePhaseChip.tsx —
// keep the two maps' hues in sync): amber = all turns (the established "stop
// armed" color), emerald = own turns (TurnStatusLine's "you act" tone), rose =
// opponents' turns. Deliberately NOT seat colors: a scope is a standing
// preference over many games and (in multiplayer) many opponents, so it must
// not borrow any one seat's identity color.
export const SCOPE_DOT_CLASS: Record<PhaseStopScope, string> = {
  AllTurns: "bg-amber-400",
  OwnTurn: "bg-emerald-400",
  OpponentsTurns: "bg-rose-400",
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

/**
 * Single authority for reading and cycling one phase's stop:
 * off → AllTurns → OwnTurn → OpponentsTurns → off. Shared by the desktop
 * PhaseDot buttons and the mobile phase-stop sheet rows.
 *
 * Updates in place so array order is preserved — `useGameplayPreferencesSync`
 * dedupes by positional comparison, so appending would reorder and force a
 * redundant engine dispatch even when the set of stops is unchanged.
 */
export function usePhaseStopCycle(phase: Phase): {
  stop: PhaseStop | undefined;
  cyclePhase: () => void;
} {
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  const setPhaseStops = usePreferencesStore((s) => s.setPhaseStops);
  const stop = phaseStops.find((s) => s.phase === phase);

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

  return { stop, cyclePhase };
}

function PhaseDot({ phase }: { phase: Phase }) {
  const { t } = useTranslation("game");
  const tooltipId = useId();
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const { stop, cyclePhase } = usePhaseStopCycle(phase);

  const isActive = phase === currentPhase;
  const hasStop = stop !== undefined;
  const tooltip = getPhaseTooltip(t, phase, stop?.scope, isActive);

  return (
    <button
      type="button"
      onClick={cyclePhase}
      aria-label={tooltip}
      aria-describedby={tooltipId}
      aria-pressed={hasStop}
      data-phase-stop-dot={phase}
      data-active-phase={isActive ? "true" : undefined}
      className={`group relative flex h-6 w-6 items-center justify-center rounded-[7px] border transition-colors duration-150 lg:h-8 lg:w-8 lg:p-1 ${
        isActive
          ? "border-transparent bg-transparent text-cyan-200 drop-shadow-[0_0_6px_rgba(103,232,249,0.85)]"
          : hasStop
            ? "border-white/12 bg-white/8 text-slate-200 hover:border-white/20 hover:text-white"
            : "border-transparent bg-transparent text-slate-500 hover:border-white/10 hover:bg-white/5 hover:text-slate-200"
      }`}
    >
      {PHASE_ICONS[phase]}
      {stop && (
        <span
          className={`absolute -bottom-0.5 left-1/2 h-1 w-1 -translate-x-1/2 rounded-full ${SCOPE_DOT_CLASS[stop.scope]}`}
        />
      )}
      <GameplayTooltip id={tooltipId}>
        {tooltip}
      </GameplayTooltip>
    </button>
  );
}

/** One continuous major-phase rail for the shared Tabletop HUD. The empty center
 *  lane sits beneath the independently centered local life badge; equal-width
 *  phase groups keep the rail itself centered despite the 2/3 phase split. */
export function MajorPhaseStopRail() {
  return (
    <div
      className="tabletop-liquid-glass-rail hidden items-center"
      data-major-phase-stop-rail="all"
    >
      <div data-phase-stop-rail-section="left">
        {MAJOR_PHASES_BY_SIDE.left.map((phase) => (
          <PhaseDot key={phase} phase={phase} />
        ))}
      </div>
      <span aria-hidden data-phase-stop-rail-center-gap="" />
      <div data-phase-stop-rail-section="right">
        {MAJOR_PHASES_BY_SIDE.right.map((phase) => (
          <PhaseDot key={phase} phase={phase} />
        ))}
      </div>
    </div>
  );
}

/** Upkeep, Draw, Main1 — placed to the left of the player avatar.
 *  Hidden on mobile (<lg) where the dots are too small to tap and crowd the HUD. */
export function PhaseIndicatorLeft() {
  return (
    <div className="tabletop-phase-plaque hidden items-center gap-0.5 border border-white/10 px-1 py-1 lg:flex lg:px-1.5">
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
    <div className="tabletop-phase-plaque hidden items-center gap-0.5 border border-white/10 px-1 py-1 lg:flex lg:px-1.5">
      {RIGHT_PHASES.map((phase) => (
        <PhaseDot key={phase} phase={phase} />
      ))}
    </div>
  );
}

/** BeginCombat through EndCombat — placed near ActionButton on the right side */
export function CombatPhaseIndicator() {
  return (
    <div
      data-combat-phase-indicator
      className="tabletop-phase-plaque flex items-center gap-0.5 border border-white/10 px-1 py-1 lg:px-1.5"
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
