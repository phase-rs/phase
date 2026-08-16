import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { Phase, PhaseStopScope } from "../../adapter/types";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { DialogShell } from "../modal/DialogShell.tsx";
import {
  PHASE_ICONS,
  PHASE_KEY,
  SCOPE_DOT_CLASS,
  usePhaseStopCycle,
} from "./PhaseStopBar.tsx";

// CR 500.1: full turn order. The desktop indicators split this list across
// three components for layout reasons; the sheet has room to show every
// stoppable step in sequence.
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

const SCOPE_SHORT_KEY: Record<PhaseStopScope, string> = {
  AllTurns: "phaseStop.scopeShortAllTurns",
  OwnTurn: "phaseStop.scopeShortOwnTurn",
  OpponentsTurns: "phaseStop.scopeShortOpponentsTurns",
};

// Same hue axis as SCOPE_DOT_CLASS in PhaseStopBar.tsx (keep in sync): amber =
// all turns, emerald = own turns, rose = opponents' turns. Full class strings
// because Tailwind only sees statically analyzable names. The pill always
// carries its text label, so color reinforces the scope rather than encoding
// it alone.
const SCOPE_PILL_CLASS: Record<PhaseStopScope, string> = {
  AllTurns: "border-amber-400/40 bg-amber-400/10 text-amber-300",
  OwnTurn: "border-emerald-400/40 bg-emerald-400/10 text-emerald-300",
  OpponentsTurns: "border-rose-400/40 bg-rose-400/10 text-rose-300",
};

/**
 * Compact persistent phase display for mobile, where the desktop PhaseDot
 * strips are hidden (their 24px stop-toggle targets are untappable and crowd
 * the HUD). Splits the dots' two jobs across a disclosure: the chip shows the
 * current phase (icon + name, seat-color dot for whose turn), and tapping it
 * opens the phase-stop sheet with full-size per-step stop controls.
 */
export function MobilePhaseChip({ className }: { className?: string } = {}) {
  const { t } = useTranslation("game");
  const [sheetOpen, setSheetOpen] = useState(false);
  const phase = useGameStore((s) => s.gameState?.phase);
  const activePlayer = useGameStore((s) => s.gameState?.active_player);
  const phaseStops = usePreferencesStore((s) => s.phaseStops);
  // Unconditional hook call (before the early return); resolves NEUTRAL for
  // an absent player, which is never rendered thanks to the guard below.
  const turnSeatColor = useSeatColor(activePlayer);

  if (phase === undefined) return null;

  // The chip is the current phase's row, collapsed: its stop mark mirrors the
  // desktop PhaseDot exactly — shown only when THIS phase has an armed stop,
  // in that stop's scope color. Stops on other phases surface in the sheet.
  const currentStop = phaseStops.find((s) => s.phase === phase);

  const phaseLabel = t(`phaseName.${phase}`);

  return (
    <>
      <button
        type="button"
        onClick={() => setSheetOpen(true)}
        aria-label={t("phaseStop.chipAria", { phase: phaseLabel })}
        aria-haspopup="dialog"
        aria-expanded={sheetOpen}
        className={`relative flex items-center justify-center gap-1.5 rounded-full border border-cyan-400/20 bg-slate-950/64 px-3 py-1 text-[10px] font-semibold uppercase tracking-[0.18em] text-slate-300 ring-1 ring-cyan-400/15 backdrop-blur-xl transition-all duration-200 hover:border-cyan-300/40 hover:text-white hover:ring-cyan-300/30 ${className ?? ""}`}
      >
        {/* Seat-color dot: whose turn it is, mirroring the HUD plate's
            dot+label identity convention. */}
        <span
          aria-hidden
          className="h-1.5 w-1.5 shrink-0 rounded-full"
          style={{ backgroundColor: turnSeatColor }}
        />
        <span aria-hidden className="text-cyan-300 [&>svg]:h-3 [&>svg]:w-3">
          {PHASE_ICONS[phase]}
        </span>
        <span className="truncate">{phaseLabel}</span>
        {currentStop && (
          <span
            aria-hidden
            className={`absolute -bottom-0.5 left-1/2 h-1 w-1 -translate-x-1/2 rounded-full ${SCOPE_DOT_CLASS[currentStop.scope]}`}
          />
        )}
      </button>
      {sheetOpen && <PhaseStopSheet onClose={() => setSheetOpen(false)} />}
    </>
  );
}

/** Full-turn phase list with per-step auto-pass stop controls, at touch-size
 *  targets. The scope is shown as visible text (not a hover tooltip, which
 *  doesn't exist on touch) so cycling a row is self-explanatory. */
function PhaseStopSheet({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation("game");
  return (
    <DialogShell
      title={t("phaseStop.sheetTitle")}
      subtitle={t("phaseStop.sheetSubtitle")}
      size="sm"
      scrollable
      onClose={onClose}
    >
      <ul className="flex flex-col px-2 py-2">
        {ALL_PHASES.map((phase) => (
          <PhaseStopRow key={phase} phase={phase} />
        ))}
      </ul>
    </DialogShell>
  );
}

function PhaseStopRow({ phase }: { phase: Phase }) {
  const { t } = useTranslation("game");
  const currentPhase = useGameStore((s) => s.gameState?.phase);
  const { stop, cyclePhase } = usePhaseStopCycle(phase);

  const isActive = phase === currentPhase;
  const hasStop = stop !== undefined;
  const key = PHASE_KEY[phase];

  return (
    <li>
      <button
        type="button"
        onClick={cyclePhase}
        aria-pressed={hasStop}
        className={`flex w-full items-center gap-3 rounded-[10px] border px-3 py-2 text-left transition-colors duration-150 ${
          isActive
            ? "border-cyan-300/45 bg-cyan-950/60"
            : "border-transparent hover:bg-white/5"
        }`}
      >
        <span
          aria-hidden
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-[7px] border ${
            isActive
              ? "border-cyan-300/45 bg-cyan-950/82 text-white"
              : "border-white/10 bg-white/5 text-slate-400"
          } [&>svg]:h-3.5 [&>svg]:w-3.5`}
        >
          {PHASE_ICONS[phase]}
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium text-slate-100">
            {t(`phaseStop.${key}Label`)}
          </span>
          <span className="block truncate text-[11px] text-slate-400">
            {t(`phaseStop.${key}Description`)}
          </span>
        </span>
        <span
          className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${
            stop ? SCOPE_PILL_CLASS[stop.scope] : "border-white/10 text-slate-500"
          }`}
        >
          {stop ? t(SCOPE_SHORT_KEY[stop.scope]) : t("phaseStop.scopeShortOff")}
        </span>
      </button>
    </li>
  );
}
