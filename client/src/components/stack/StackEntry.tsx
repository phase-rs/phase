import type { CSSProperties } from "react";

import { motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { PLAYER_ID } from "../../constants/game.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

interface StackEntryProps {
  entry: StackEntryType;
  index: number;
  isTop: boolean;
  isPending?: boolean;
  cardSize: { width: number; height: number };
  style?: CSSProperties;
}

export function StackEntry({ entry, index, isTop, isPending, cardSize, style }: StackEntryProps) {
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const sourceObj = objects?.[entry.source_id];
  const sourceName = sourceObj?.name ?? "Unknown";

  const { src, isLoading } = useCardImage(sourceName, { size: "normal" });

  const isSpell = entry.kind.type === "Spell";
  const abilityLabel =
    entry.kind.type === "ActivatedAbility" ? "Activated" : "Triggered";
  const controllerLabel = entry.controller === PLAYER_ID ? "You" : "Opp";

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 30, scale: 0.9 }}
      animate={{ opacity: 1, x: 0, scale: 1 }}
      exit={{ opacity: 0, x: 30, scale: 0.9 }}
      transition={{ delay: index * 0.03 }}
      style={style}
      className="relative cursor-pointer"
      onMouseEnter={() => inspectObject(entry.source_id)}
      onMouseLeave={() => inspectObject(null)}
    >
      {/* Card image with explicit inline dimensions (Tailwind can't handle dynamic values) */}
      <div
        style={{ width: cardSize.width, height: cardSize.height }}
        className="overflow-hidden rounded-lg shadow-lg ring-1 ring-white/10"
      >
        {isLoading || !src ? (
          <div
            className="animate-pulse rounded-lg bg-gray-700 border border-gray-600"
            style={{ width: cardSize.width, height: cardSize.height }}
          />
        ) : (
          <img
            src={src}
            alt={sourceName}
            className="h-full w-full object-cover"
            draggable={false}
          />
        )}
      </div>

      {/* Badge: "Casting..." for pending spells, "Next" for top of stack */}
      {isPending ? (
        <span className="absolute -right-1 -top-2 animate-pulse rounded-full bg-cyan-500 px-2 py-0.5 text-[10px] font-bold text-black shadow-md">
          Casting…
        </span>
      ) : isTop && (
        <span className="absolute -right-1 -top-2 rounded-full bg-amber-500 px-2 py-0.5 text-[10px] font-bold text-black shadow-md">
          Next
        </span>
      )}

      {/* Ability badge overlay (non-spell entries: triggered/activated) */}
      {!isSpell && (
        <div className="absolute inset-x-0 bottom-0 rounded-b-lg bg-gradient-to-t from-black/80 to-transparent px-1.5 py-1">
          <div className="truncate text-[10px] font-medium text-gray-100">
            {sourceName}
          </div>
          <div className="text-[9px] text-purple-300">{abilityLabel}</div>
        </div>
      )}

      {/* Controller badge */}
      <span className="absolute bottom-1 left-1 rounded bg-black/60 px-1 py-0.5 text-[9px] font-semibold text-gray-300">
        {controllerLabel}
      </span>
    </motion.div>
  );
}
