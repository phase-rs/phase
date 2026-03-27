import { useMemo } from "react";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";
import { CompactStrip } from "./CompactStrip.tsx";
import { CommanderDisplay } from "./CommanderDisplay.tsx";
import { CommanderDamage } from "./CommanderDamage.tsx";
import { CommandZone } from "../zone/CommandZone.tsx";

/** Base scales — used when few cards; shrinks as more are added */
const LAND_BASE_SCALE = 0.78;
const OTHER_BASE_SCALE = 1.0;
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
}

export function PlayerArea({ playerId, mode, onFocus, isActive, landColumnExtra, creatureOverride }: PlayerAreaProps) {
  const gameState = useGameStore((s) => s.gameState);

  const partitioned = useMemo(() => {
    if (!gameState) return null;

    const battlefieldObjects = gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(Boolean);

    const playerObjects = battlefieldObjects.filter(
      (obj) => obj.controller === playerId,
    );

    const partition = partitionByType(playerObjects);
    const objectMap = new Map(playerObjects.map((o) => [o.id, o]));
    const resolveObjects = (ids: number[]) =>
      ids.map((id) => objectMap.get(id)).filter(Boolean) as GameObject[];

    return {
      creatures: groupByName(resolveObjects(partition.creatures)),
      lands: groupByName(resolveObjects(partition.lands)),
      support: groupByName(resolveObjects(partition.support)),
      planeswalkers: groupByName(resolveObjects(partition.planeswalkers)),
      other: groupByName(resolveObjects(partition.other)),
    };
  }, [gameState, playerId]);

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
  const isCommander = gameState.format_config?.format === "Commander";
  const isEliminated = player?.is_eliminated ?? false;
  const isMirrored = mode === "focused";

  const creatures = creatureOverride ?? partitioned?.creatures ?? [];
  const hasPlaneswalkers = (partitioned?.planeswalkers.length ?? 0) > 0;
  const hasEmblems = (gameState.command_zone ?? []).some((id) => {
    const obj = gameState.objects[id];
    return obj?.is_emblem && obj.controller === playerId;
  });
  const planeswalkerLane = (hasPlaneswalkers || hasEmblems) ? (
    <div
      className={`absolute right-0 top-0 bottom-0 z-20 flex flex-col flex-wrap-reverse items-end gap-2 px-1 py-2 ${
        isCommander ? (mode === "focused" ? "pb-16" : "pb-24") : ""
      }`}
      style={zoneStyle(OTHER_BASE_SCALE)}
    >
      {partitioned?.planeswalkers.map((g) => (
        <GroupedPermanentDisplay key={g.ids[0]} group={g} />
      ))}
      <CommandZone playerId={playerId} />
    </div>
  ) : null;
  const landAlignClass = isMirrored
    ? "flex-wrap items-center justify-start"
    : "flex-wrap items-center content-end justify-start";

  const landCount = partitioned?.lands.length ?? 0;
  const supportCount = partitioned?.support.length ?? 0;
  const landStyle = zoneStyle(zoneScale(LAND_BASE_SCALE, landCount));
  const supportStyle = zoneStyle(zoneScale(OTHER_BASE_SCALE, supportCount));

  const middleRow = (
    <div className="flex min-h-0 items-stretch justify-between gap-4" data-debug-label="Middle Row">
      <div
        className={`z-10 flex min-w-0 basis-0 flex-1 gap-2 pl-2 ${landAlignClass}`}
        style={landStyle}
        data-debug-label="Lands"
      >
        {partitioned?.lands.map((g) => (
          <GroupedPermanentDisplay key={g.ids[0]} group={g} />
        ))}
        {landColumnExtra}
      </div>
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
      </div>
    </div>
  );

  return (
    <div
      className={`relative flex min-h-0 flex-1 overflow-visible ${isEliminated ? "opacity-40 grayscale" : ""}`}
      data-testid={`player-area-${playerId}`}
    >
      <div
        className={`flex min-w-0 flex-1 flex-col gap-2 px-1 ${
          mode === "full" ? "pt-1 pb-2" : "justify-end py-1"
        } ${
          isCommander ? (mode === "focused" ? "pb-16" : "pb-24") : ""
        }`}
      >
        {isMirrored ? (
          <>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
            <div className="shrink-0">
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
            <div className="shrink-0">
              {middleRow}
            </div>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
          </>
        )}
      </div>
      {planeswalkerLane}
      {/* Commander display overlay */}
      {isCommander && (
        <div className="absolute right-2 bottom-2 z-20 flex flex-col gap-1">
          <CommanderDisplay playerId={playerId} compact={mode === "focused"} />
          <CommanderDamage playerId={playerId} />
        </div>
      )}
      {/* Eliminated badge */}
      {isEliminated && (
        <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
          <span className="rounded-lg bg-red-900/80 px-4 py-2 text-lg font-bold text-red-200">
            Eliminated
          </span>
        </div>
      )}
    </div>
  );
}
