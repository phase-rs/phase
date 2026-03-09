import { AnimatePresence, motion } from "framer-motion";

import { StackEntry } from "./StackEntry.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

const EMPTY_STACK: StackEntryType[] = [];

const STAGGER_Y = 28;
const STAGGER_X = 3;
const BASE_WIDTH = 150;
const MIN_WIDTH = 80;
const ASPECT_RATIO = 1.4;

function computeCardSize(stackCount: number) {
  const scale = Math.max(0.5, 1 - Math.max(0, stackCount - 2) * 0.083);
  const width = Math.max(MIN_WIDTH, Math.round(BASE_WIDTH * scale));
  const height = Math.round(width * ASPECT_RATIO);
  return { width, height };
}

export function StackDisplay() {
  const stack = useGameStore((s) => s.gameState?.stack ?? EMPTY_STACK);

  if (stack.length === 0) return null;

  const displayStack = [...stack].reverse();
  const cardSize = computeCardSize(stack.length);

  return (
    <AnimatePresence>
      <motion.div
        key="stack-container"
        initial={{ opacity: 0, x: 60 }}
        animate={{ opacity: 1, x: 0 }}
        exit={{ opacity: 0, x: 60 }}
        transition={{ type: "spring", stiffness: 300, damping: 30 }}
        className="fixed right-4 top-1/2 z-30 -translate-y-1/2"
      >
        <div className="rounded-xl bg-black/40 p-3 backdrop-blur-sm">
          {/* Header */}
          <div className="mb-2 flex items-center gap-2">
            <span className="text-xs font-semibold uppercase tracking-wider text-gray-300">
              Stack
            </span>
            <span className="rounded-full bg-gray-600 px-1.5 py-0.5 text-[10px] font-bold text-gray-200">
              {stack.length}
            </span>
          </div>

          {/* Staggered pile */}
          <div
            className="relative"
            style={{
              width: cardSize.width + STAGGER_X * (displayStack.length - 1),
              height: cardSize.height + STAGGER_Y * (displayStack.length - 1),
            }}
          >
            <AnimatePresence mode="popLayout">
              {displayStack.map((entry, index) => (
                <StackEntry
                  key={entry.id}
                  entry={entry}
                  index={index}
                  isTop={index === 0}
                  cardSize={cardSize}
                  style={{
                    position: "absolute",
                    top: index * STAGGER_Y,
                    left: index * STAGGER_X,
                    zIndex: displayStack.length - index,
                  }}
                />
              ))}
            </AnimatePresence>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
