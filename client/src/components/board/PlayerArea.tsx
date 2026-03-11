import { useMemo } from "react";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { partitionByType, groupByName } from "../../viewmodel/battlefieldProps.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
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
  width: `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 2 + 1.5rem)`,
} as React.CSSProperties;

const LAND_COL_WIDTH = `calc(var(--art-crop-base) * var(--card-size-scale) * ${LAND_SCALE} * 2 + 1.5rem)`;

/** Scale for enchantment/artifact column (right) */
const OTHER_SCALE = 0.85;

const OTHER_COL_STYLE = {
  "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE})`,
  "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE} * 0.75)`,
  "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${OTHER_SCALE})`,
  "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${OTHER_SCALE} * 1.4)`,
  width: `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE} + 1.5rem)`,
} as React.CSSProperties;

const OTHER_COL_WIDTH = `calc(var(--art-crop-base) * var(--card-size-scale) * ${OTHER_SCALE} + 1.5rem)`;

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
      {/* Lands -- left column */}
      <div
        className="absolute left-0 top-0 bottom-0 z-10 flex flex-col overflow-visible px-1 py-2"
        style={LAND_COL_STYLE}
      >
        {partitioned && (
          <BattlefieldRow groups={partitioned.lands} rowType="lands" />
        )}
        {landColumnExtra}
      </div>
      {/* Creatures -- center area */}
      <div
        className={`flex flex-1 flex-col ${mode === "full" ? "pt-2 pb-4" : "justify-end py-2"} gap-1`}
        style={{ paddingLeft: LAND_COL_WIDTH, paddingRight: OTHER_COL_WIDTH }}
      >
        <BattlefieldRow groups={creatures} rowType="creatures" />
      </div>
      {/* Enchantments/artifacts -- right column */}
      <div
        className="absolute right-0 top-0 bottom-0 z-10 overflow-visible px-1 py-2"
        style={OTHER_COL_STYLE}
      >
        {partitioned && (
          <BattlefieldRow groups={partitioned.other} rowType="other" />
        )}
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
