import type { Phase, PhaseStop, PhaseStopScope } from "../../adapter/types";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import { PhaseIconFor } from "./phaseIcons.tsx";

// Turn-phase glyphs live in `./phaseIcons.tsx` — one hand-written SVG component
// per phase, in the same shape as `chrome/SparkleIcon.tsx` and
// `chrome/navIcons.tsx`. Two phases used to render visually identical inline
// SVGs here (both main phases were the same diamond; begin-combat and
// combat-damage were both crossed swords), which left the bar with no visual
// cue for which phase a click would set a stop on. `phaseIcons.tsx` documents
// the mapping and the shape language.
//
// Sizing note: each glyph is drawn on a 24-unit grid and defaults to 16px. The
// classes at the call sites carry the responsive sizes and are what actually
// decide the rendered box (`PhaseDot` below, plus the mobile chip/sheet's own
// `[&>svg]:h-*` containers). They are one step above the 10/14px the old 14-grid
// glyphs used, because a 2-unit stroke on a 24 grid is optically thinner at the
// same pixel size: at 10px it scales to ~0.83px and reads as a hairline.

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
      className={`group relative flex h-6 w-6 items-center justify-center rounded-[7px] border transition-colors duration-150 lg:h-8 lg:w-8 lg:p-1 ${
        isActive
          ? "border-cyan-300/45 bg-cyan-950/82 text-white"
          : hasStop
            ? "border-white/12 bg-white/8 text-slate-200 hover:border-white/20 hover:text-white"
            : "border-transparent bg-transparent text-slate-500 hover:border-white/10 hover:bg-white/5 hover:text-slate-200"
      }`}
    >
      {isActive && (
        <span className="absolute -top-1 left-1/2 h-1 w-3 -translate-x-1/2 rounded-[2px] bg-amber-300" />
      )}
      <PhaseIconFor phase={phase} className="h-3 w-3 lg:h-4 lg:w-4" />
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

/** Upkeep, Draw, Main1 — placed to the left of the player avatar.
 *  Hidden on mobile (<lg) where the dots are too small to tap and crowd the HUD. */
export function PhaseIndicatorLeft() {
  return (
    <div className="hidden items-center gap-0.5 rounded-[10px] border border-white/10 bg-slate-950/88 px-1 py-1 lg:flex lg:px-1.5">
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
    <div className="hidden items-center gap-0.5 rounded-[10px] border border-white/10 bg-slate-950/88 px-1 py-1 lg:flex lg:px-1.5">
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
  // next to the ActionButton. MobilePhaseChip conveys the current phase there.
  if (isCompactHeight) return null;
  return (
    <div
      data-combat-phase-indicator
      className="flex items-center gap-0.5 rounded-[10px] border border-white/10 bg-slate-950/88 px-1 py-1 lg:px-1.5"
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
