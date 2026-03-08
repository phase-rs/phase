import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

interface StackEntryProps {
  entry: StackEntryType;
}

export function StackEntry({ entry }: StackEntryProps) {
  const objects = useGameStore((s) => s.gameState?.objects);

  const sourceObj = objects?.[entry.source_id];
  const sourceName = sourceObj?.name ?? "Unknown";

  const isSpell = entry.kind.type === "Spell";

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 20 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: -20 }}
      className="flex items-center gap-2 rounded border border-gray-600 bg-gray-800 p-1.5"
    >
      {isSpell ? (
        <CardImage cardName={sourceName} size="small" className="!h-10 !w-7" />
      ) : (
        <div className="flex h-10 w-7 items-center justify-center rounded bg-purple-900 text-xs font-bold text-purple-300">
          Ab
        </div>
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate text-xs font-medium text-gray-100">
          {sourceName}
        </div>
        <div className="text-[10px] text-gray-400">
          {isSpell ? "Spell" : entry.kind.type === "ActivatedAbility" ? "Activated" : "Triggered"}
          {" - P"}{entry.controller + 1}
        </div>
      </div>
    </motion.div>
  );
}
