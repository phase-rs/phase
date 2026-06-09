import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { commanderDamageEntriesFor, commandersInZone } from "../../viewmodel/commanderColumn.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
import { CompactStrip } from "./CompactStrip.tsx";
import { CommanderDamage } from "./CommanderDamage.tsx";
import { CommanderCardZone } from "../zone/CommanderCardZone.tsx";
import { CommandZone } from "../zone/CommandZone.tsx";

/** Base scales — used when few cards; shrinks as more are added.
 *  On compact-height (landscape phones), lands shrink hard so creatures
 *  (which players actually interact with — attack, block, P/T, abilities)
 *  get vertical breathing room. */
const LAND_BASE_SCALE = 0.82;
const LAND_BASE_SCALE_COMPACT = 0.42;
const OTHER_BASE_SCALE = 0.92;
const OTHER_BASE_SCALE_COMPACT = 0.42;
/** Minimum scale floor */
const MIN_ZONE_SCALE = 0.35;

/** Compute dynamic scale that shrinks as group count increases */
function zoneScale(baseScale: number, groupCount: number): number {
  if (groupCount <= 3) return baseScale;
  // Inverse-sqrt decay past threshold, floored at MIN_ZONE_SCALE
  const excess = groupCount - 3;
  return Math.max(MIN_ZONE_SCALE, baseScale / Math.sqrt(1 + excess * 0.2));
}

function zoneStyle(scale: number): React.CSSProperties {
  return {
    "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale})`,
    "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale} * 0.85)`,
    "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${scale})`,
    "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${scale} * 1.4)`,
  } as React.CSSProperties;
}

export type PlayerAreaMode = "full" | "focused" | "compact";

interface PlayerAreaProps {
  playerId: PlayerId;
  mode: PlayerAreaMode;
  onFocus?: () => void;
  /** Whether this compact strip is the currently focused opponent */
  isActive?: boolean;
  /** Extra content to render in the land column (e.g. undo button) */
  landColumnExtra?: React.ReactNode;
  /** Override creature groups with pre-sorted list (for blocker alignment) */
  creatureOverride?: GroupedPermanent[];
  battlefieldView?: PlayerBattlefieldView;
  /** HUD element rendered inline between lands and support in the middle row */
  hud?: React.ReactNode;
}

export function PlayerArea({
  playerId,
  mode,
  onFocus,
  isActive,
  landColumnExtra,
  creatureOverride,
  battlefieldView,
  hud,
}: PlayerAreaProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const isCompactHeight = useIsCompactHeight();

  if (!gameState) return null;

  // Compact mode renders a condensed strip
  if (mode === "compact") {
    return (
      <CompactStrip
        playerId={playerId}
        onClick={onFocus}
        isActive={isActive}
      />
    );
  }

  const player = gameState.players[playerId];
  const isEliminated = player?.is_eliminated ?? false;
  // CR 702.26-style player phasing: while phased out, dim the player area
  // to mirror the engine-side exclusion (targeting/damage/attack/SBA). Use
  // the same visual treatment as elimination for consistency.
  const isPhasedOut = player?.status?.type === "PhasedOut";
  const isMirrored = mode === "focused";
  const partitioned = battlefieldView;

  const creatures = creatureOverride ?? partitioned?.creatures ?? [];
  const hasPlaneswalkers = (partitioned?.planeswalkers.length ?? 0) > 0;
  const hasEmblems = (gameState.command_zone ?? []).some((id) => {
    const obj = gameState.objects[id];
    return obj?.is_emblem && obj.controller === playerId;
  });
  // Scale the commander column to the land scale (not the support scale) so
  // presence/absence of a commander doesn't change the middle-row height. The
  // support row's height is otherwise driven by the tallest card in it, and
  // the commander card at OTHER_BASE_SCALE (1.0) is ~28% taller than adjacent
  // lands at LAND_BASE_SCALE (0.78), which squeezes the creatures row via
  // flex-1. Stacked CommanderDamage entries compound the warp.
  const commanderScale = isCompactHeight ? LAND_BASE_SCALE_COMPACT : LAND_BASE_SCALE;
  // Render the commander column only when it has real content — a commander
  // still in the command zone, or live commander-damage entries. Gating on the
  // format flag alone reserved an empty column (and its divider) for the whole
  // game once the commander was cast. These go through the same selectors
  // CommanderCardZone and CommanderDamage render from, so the wrapper appears
  // iff a child will (single source of truth in viewmodel/commanderColumn).
  const hasCommanderInZone = commandersInZone(gameState, playerId).length > 0;
  const hasActiveCommanderDamage = commanderDamageEntriesFor(gameState, playerId).length > 0;
  const commanderSection = hasCommanderInZone || hasActiveCommanderDamage ? (
    // No `min-w-0` here: this column holds the commander-damage labels, and
    // allowing it to shrink below its content width lets the adjacent
    // (non-shrinking) planeswalker row collapse it and hide the labels.
    <div
      className="flex flex-col items-end gap-1"
      style={zoneStyle(commanderScale)}
    >
      <CommanderCardZone playerId={playerId} />
      <CommanderDamage playerId={playerId} />
    </div>
  ) : null;
  const supportExtras = (
    <>
      <BattlefieldRow groups={partitioned?.planeswalkers ?? []} rowType="planeswalkers" />
      <CommandZone playerId={playerId} />
      {commanderSection}
    </>
  );
  const hasSupportExtras = hasPlaneswalkers || hasEmblems || commanderSection != null;
  const supportSection = hasSupportExtras ? (
    <>
      <div className="mx-2 h-3/4 w-px shrink-0 bg-white/20" />
      {/* No `min-w-0`: the support track is justify-end, so letting this
          wrapper collapse below its content width pushes the right-rail cards
          (emblems/commander) off the right edge of the screen. Holding its
          intrinsic width clamps the right edge to the track and grows the
          cluster leftward on-screen instead. */}
      <div
        className="flex items-center gap-2"
        style={zoneStyle(isCompactHeight ? OTHER_BASE_SCALE_COMPACT : OTHER_BASE_SCALE)}
      >
        {supportExtras}
      </div>
    </>
  ) : null;
  const landAlignClass = isCompactHeight
    ? "flex-nowrap items-center justify-start"
    : "flex-wrap items-center content-center justify-start";

  const landCount = partitioned?.lands.length ?? 0;
  const supportCount = partitioned?.support.length ?? 0;
  const landBase = isCompactHeight ? LAND_BASE_SCALE_COMPACT : LAND_BASE_SCALE;
  const supportBase = isCompactHeight ? OTHER_BASE_SCALE_COMPACT : OTHER_BASE_SCALE;
  const landStyle = zoneStyle(zoneScale(landBase, landCount));
  const supportStyle = zoneStyle(zoneScale(supportBase, supportCount));

  const middleRow = (
    <div className="flex min-h-0 min-w-0 items-stretch justify-between gap-2" data-debug-label="Middle Row">
      <div
        className={`z-10 flex min-w-0 basis-0 flex-1 gap-2 pl-2 ${landAlignClass}`}
        style={landStyle}
        data-debug-label="Lands"
      >
        <BattlefieldRow
          groups={partitioned?.lands ?? []}
          rowType="lands"
          className="justify-start px-0"
        />
        {landColumnExtra}
      </div>
      {hud && (
        <div className="z-20 flex shrink-0 items-center" data-debug-label="Inline HUD">
          {hud}
        </div>
      )}
      <div
        className="z-10 flex min-w-0 basis-0 flex-1 items-center justify-end pr-2"
        style={supportStyle}
        data-debug-label="Support"
      >
        <BattlefieldRow
          groups={partitioned?.support ?? []}
          rowType="support"
          className="ml-auto w-full justify-end px-0"
        />
        {supportSection}
      </div>
    </div>
  );

  return (
    <div
      className={`relative flex min-h-0 min-w-0 flex-1 overflow-visible ${
        isEliminated ? "opacity-40 grayscale" : isPhasedOut ? "opacity-70" : ""
      }`}
      data-testid={`player-area-${playerId}`}
      data-phased-out={isPhasedOut ? "true" : undefined}
    >
      <div
        className={`flex min-w-0 flex-1 flex-col px-1 ${
          isCompactHeight ? "gap-0.5" : "gap-2"
        } ${
          mode === "full"
            ? isCompactHeight ? "pt-0 pb-0.5" : "pt-1 pb-8"
            : isCompactHeight ? "justify-end py-0" : "justify-end py-1"
        }`}
      >
        {isMirrored ? (
          <>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
            <div className={isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}>
              {middleRow}
            </div>
            <div className="flex min-h-0 flex-1 items-end px-2" data-debug-label="Opp Creatures">
              <BattlefieldRow groups={creatures} rowType="creatures" className="w-full" />
            </div>
          </>
        ) : (
          <>
            <div className="min-h-0 flex-1 px-2" data-debug-label="Creatures">
              <BattlefieldRow groups={creatures} rowType="creatures" />
            </div>
            <div className={isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}>
              {middleRow}
            </div>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
          </>
        )}
      </div>
      {/* Eliminated badge */}
      {isEliminated && (
        <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
          <span className="rounded-lg bg-red-900/80 px-4 py-2 text-lg font-bold text-red-200">
            {t("player.eliminated")}
          </span>
        </div>
      )}
      {/* Phased-out tint overlay + badge (CR 702.26-style player phasing).
          Translucent blue evokes the "ethereal plane" reading of phasing and
          is a stronger signal than dim-alone, which overlaps with tap/grayed
          states. `pointer-events-none` preserves card hover/click semantics —
          interactivity gating is an engine concern, not a visual one. */}
      {isPhasedOut && !isEliminated && (
        <>
          <div className="absolute inset-0 z-20 bg-sky-500/25 mix-blend-screen pointer-events-none" />
          <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
            <span className="rounded-lg bg-indigo-900/80 px-4 py-2 text-lg font-bold text-indigo-200">
              {t("player.phasedOut")}
            </span>
          </div>
        </>
      )}
    </div>
  );
}
