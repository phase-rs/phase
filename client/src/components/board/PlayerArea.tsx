import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
import { BattlefieldZoneOverflow } from "./BattlefieldZoneOverflow.tsx";
import { CompactStrip } from "./CompactStrip.tsx";
import { CommandDock } from "../zone/CommandDock.tsx";

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
  // Combined support cluster: artifacts/enchantments then planeswalkers, in ONE
  // wrapping row (like the lands column) so it stays a single line until crowded.
  // Keeping it one row keeps the middle-row band ~one card tall so the flex-1
  // creature row isn't pinched. Memoized for a stable ref (BattlefieldRow perf);
  // declared above the early return to keep hook order stable.
  const supportGroups = useMemo(
    () => [...(battlefieldView?.support ?? []), ...(battlefieldView?.planeswalkers ?? [])],
    [battlefieldView?.support, battlefieldView?.planeswalkers],
  );

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
  const landAlignClass = isCompactHeight
    ? "flex-nowrap items-center justify-start"
    : "flex-wrap items-center content-center justify-start";
  // Support cluster mirrors the lands column but right-aligned: one wrapping row
  // that wraps only when crowded (cards shrink with count via supportStyle).
  const supportAlignClass = isCompactHeight
    ? "flex-nowrap items-center justify-end"
    : "flex-wrap items-center content-center justify-end";

  const landCount = partitioned?.lands.length ?? 0;
  // Count the full support cluster (enchantments/artifacts + planeswalkers) so
  // zoneScale shrinks the cards as it fills — mirroring lands. Counting only
  // `support` left the planeswalkers unscaled and overflowing the column.
  const supportLen = partitioned?.support.length ?? 0;
  const planeswalkerLen = partitioned?.planeswalkers.length ?? 0;
  const supportCount = supportLen + planeswalkerLen;
  // Divider sits at the enchantment/artifact → planeswalker boundary within the
  // single combined support row, but only when both sub-clusters are present.
  const supportDividerIndex = supportLen > 0 && planeswalkerLen > 0 ? supportLen : undefined;
  const landBase = isCompactHeight ? LAND_BASE_SCALE_COMPACT : LAND_BASE_SCALE;
  const supportBase = isCompactHeight ? OTHER_BASE_SCALE_COMPACT : OTHER_BASE_SCALE;
  const landStyle = zoneStyle(zoneScale(landBase, landCount));
  const supportStyle = zoneStyle(zoneScale(supportBase, supportCount));

  // Two-column middle row: lands (left, justify-start) and support (right,
  // justify-end) each take a flex-1 half and meet in the center. The HUD is no
  // longer wedged between them — it gets its own band (`hudBand`) adjacent to
  // this row — so the two card tracks reclaim the central corridor.
  const middleRow = (
    <div className="flex min-h-0 min-w-0 items-stretch justify-between gap-2" data-debug-label="Middle Row">
      <div
        className={`z-10 flex min-w-0 basis-0 flex-1 gap-2 pl-2 ${landAlignClass}`}
        style={landStyle}
        data-debug-label="Lands"
      >
        <BattlefieldZoneOverflow
          groups={partitioned?.lands ?? []}
          zone="lands"
          side="left"
          className="justify-start px-0"
        />
        {landColumnExtra}
      </div>
      {/* Support column: artifacts/enchantments + planeswalkers in ONE wrapping
          row (mirrors the lands column) so the band stays ~one card tall and the
          creature row keeps its height. A thin divider (`supportDividerIndex`)
          separates the two sub-clusters without stacking them onto a second row. */}
      <div
        className={`z-10 flex min-w-0 basis-0 flex-1 gap-2 ${supportAlignClass}`}
        style={supportStyle}
        data-debug-label="Support"
      >
        <BattlefieldZoneOverflow
          groups={supportGroups}
          zone="support"
          side="right"
          dividerBeforeIndex={supportDividerIndex}
          className="justify-end px-0"
        />
      </div>
      {/* Command zone (CR 408) as an in-flow column on the far edge so it reserves
          real horizontal space — support cards (`justify-end`) no longer slide
          under it. `self-center` + `shrink-0` lets it claim width without forcing
          the band taller than its own content: compact mode is a ~48px button
          (shorter than a land card → zero band growth); inline grows the band
          only when the user opts into it on a roomy screen. CommandDock renders
          null when the command zone is empty, collapsing this column to nothing. */}
      <div
        className="z-10 flex shrink-0 items-center self-center pr-2"
        data-debug-label="Command"
      >
        <CommandDock playerId={playerId} isMirrored={isMirrored} />
      </div>
    </div>
  );

  // Player HUD (life, mana, phase arrows) overlaid below the middle row rather
  // than wedged into its own column or lane. As a content-width absolute overlay
  // it consumes zero vertical space, so the flex-1 creature row keeps its full
  // height, and the box hugs the HUD instead of spanning the row. Centered
  // horizontally (`left-1/2 -translate-x-1/2`) and dropped just below the middle
  // row for the player (`top-[130%] -translate-y-full`); the focused opponent
  // uses the vertical mirror (`bottom-[130%] translate-y-full`) so the HUD sits
  // just above its middle row. z-20 keeps it
  // above resting cards (lands/support are z-10) but below a hovered card
  // (PermanentCard lifts to z-60), so a card slides over the HUD on hover.
  const hudBand = hud ? (
    <div
      className={`absolute left-1/2 z-20 -translate-x-1/2 ${isMirrored ? "bottom-[130%] translate-y-full" : "top-[130%] -translate-y-full"}`}
      data-debug-label="HUD"
    >
      {hud}
    </div>
  ) : null;

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
            <div className={`relative ${isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}`}>
              {middleRow}
              {hudBand}
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
            <div className={`relative ${isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}`}>
              {middleRow}
              {hudBand}
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
