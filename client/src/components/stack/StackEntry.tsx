import type { CSSProperties } from "react";

import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

interface StackEntryProps {
  entry: StackEntryType;
  index: number;
  isTop: boolean;
  cardSize: { width: number; height: number };
  style?: CSSProperties;
}

export function StackEntry({ entry, index, isTop, cardSize, style }: StackEntryProps) {
  const objects = useGameStore((s) => s.gameState?.objects);

  const sourceObj = objects?.[entry.source_id];
  const sourceName = sourceObj?.name ?? "Unknown";

  const isSpell = entry.kind.type === "Spell";
  const abilityLabel =
    entry.kind.type === "ActivatedAbility" ? "Activated" : "Triggered";

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 30, scale: 0.9 }}
      animate={{ opacity: 1, x: 0, scale: 1 }}
      exit={{ opacity: 0, x: 30, scale: 0.9 }}
      transition={{ delay: index * 0.03 }}
      style={style}
      className="relative"
    >
      <div
        style={{ width: cardSize.width, height: cardSize.height }}
        className="overflow-hidden rounded-lg shadow-lg"
      >
        <CardImage
          cardName={sourceName}
          size="normal"
          className={`!w-[${cardSize.width}px] !h-[${cardSize.height}px] object-cover`}
        />
      </div>

      {/* "Resolves Next" badge */}
      {isTop && (
        <span className="absolute -right-2 -top-2 rounded-full bg-amber-500 px-2 py-0.5 text-[10px] font-bold text-black">
          Resolves Next
        </span>
      )}

      {/* Ability badge overlay */}
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
        P{entry.controller + 1}
      </span>
    </motion.div>
  );
}
