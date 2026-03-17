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

/** Scale for land column (left) */
const LAND_SCALE = 0.9;

const LAND_COL_STYLE = {
  "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE})`,
  "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 0.75)`,
  "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2})`,
  "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2} * 1.4)`,
} as React.CSSProperties;

/** Scale for enchantment/artifact column (right) */
const OTHER_SCALE = 0.95;

const OTHER_COL_STYLE = {
  "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE})`,
  "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE} * 0.75)`,
  "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${OTHER_SCALE})`,
  "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${OTHER_SCALE} * 1.4)`,
} as React.CSSProperties;

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
      style={OTHER_COL_STYLE}
    >
      {partitioned?.planeswalkers.map((g) => (
        <GroupedPermanentDisplay key={g.ids[0]} group={g} />
      ))}
      <CommandZone playerId={playerId} />
    </div>
  ) : null;
  const middleRow = (
    <div className="flex min-h-0 items-start justify-between gap-4" data-debug-label="Middle Row">
      <div
        className="z-10 flex min-w-0 basis-0 flex-1 flex-wrap items-start justify-start gap-2"
        style={LAND_COL_STYLE}
        data-debug-label="Lands"
      >
        {partitioned?.lands.map((g) => (
          <GroupedPermanentDisplay key={g.ids[0]} group={g} />
        ))}
        {landColumnExtra}
      </div>
      <div
        className="z-10 flex min-w-0 basis-0 flex-1 justify-end"
        style={OTHER_COL_STYLE}
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
          mode === "full" ? "pt-2 pb-4" : "justify-end py-2"
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
            <div className="flex min-h-0 flex-1 items-end" data-debug-label="Opp Creatures">
              <BattlefieldRow groups={creatures} rowType="creatures" className="w-full" />
            </div>
          </>
        ) : (
          <>
            <div className="min-h-0 flex-1" data-debug-label="Creatures">
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
