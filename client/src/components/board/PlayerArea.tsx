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

/** Scale for land column (left) */
const LAND_SCALE = 0.65;

const LAND_COL_STYLE = {
  "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE})`,
  "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 0.75)`,
  "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2})`,
  "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${LAND_SCALE + 0.2} * 1.4)`,
} as React.CSSProperties;

/** Scale for enchantment/artifact column (right) */
const OTHER_SCALE = 0.85;

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

  const creatures = creatureOverride ?? partitioned?.creatures ?? [];

  return (
    <div
      className={`relative flex min-h-0 flex-1 ${isEliminated ? "opacity-40 grayscale" : ""}`}
      data-testid={`player-area-${playerId}`}
    >
      {/* Lands -- left column, flows top-to-bottom then wraps into additional columns */}
      <div
        className="z-10 flex h-full flex-shrink-0 flex-col flex-wrap gap-2 px-1 py-2"
        style={LAND_COL_STYLE}
      >
        {partitioned?.lands.map((g) => (
          <GroupedPermanentDisplay key={g.ids[0]} group={g} />
        ))}
        {landColumnExtra}
      </div>
      {/* Creatures -- center area, gets remaining space */}
      <div
        className={`flex flex-1 flex-col ${mode === "full" ? "pt-2 pb-4" : "justify-end py-2"} gap-1`}
      >
        <BattlefieldRow groups={creatures} rowType="creatures" />
      </div>
      {/* Enchantments/artifacts -- right column */}
      <div
        className="z-10 flex h-full flex-shrink-0 flex-col flex-wrap-reverse gap-2 px-1 py-2"
        style={OTHER_COL_STYLE}
      >
        {partitioned?.other.map((g) => (
          <GroupedPermanentDisplay key={g.ids[0]} group={g} />
        ))}
      </div>
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
