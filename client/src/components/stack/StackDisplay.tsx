import { useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { StackEntry } from "./StackEntry.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import type { StackEntry as StackEntryType } from "../../adapter/types.ts";

const EMPTY_STACK: StackEntryType[] = [];

const STAGGER_Y = 24;
const STAGGER_X = 2;
const BASE_WIDTH = 140;
const MIN_WIDTH = 90;
const ASPECT_RATIO = 1.4;

function computeCardSize(stackCount: number) {
  const scale = Math.max(0.55, 1 - Math.max(0, stackCount - 2) * 0.08);
  const width = Math.max(MIN_WIDTH, Math.round(BASE_WIDTH * scale));
  const height = Math.round(width * ASPECT_RATIO);
  return { width, height };
}

export function StackDisplay() {
  const stack = useGameStore((s) => s.gameState?.stack ?? EMPTY_STACK);
  const waitingFor = useGameStore((s) => s.waitingFor);

  // During TargetSelection, show the pending spell as a preview on top of the stack
  const pendingEntry: StackEntryType | null = useMemo(() => {
    if (waitingFor?.type !== "TargetSelection") return null;
    const pc = waitingFor.data.pending_cast;
    // Activated abilities use card_id 0 — don't show them as stack spells
    if (pc.card_id === 0) return null;
    return {
      id: pc.object_id,
      source_id: pc.object_id,
      controller: waitingFor.data.player,
      kind: { type: "Spell", data: { card_id: pc.card_id, ability: pc.ability } },
    };
  }, [waitingFor]);

  const fullStack = useMemo(() => {
    if (!pendingEntry) return stack;
    return [...stack, pendingEntry];
  }, [stack, pendingEntry]);

  if (fullStack.length === 0) return null;

  const displayStack = [...fullStack].reverse();
  const cardSize = computeCardSize(fullStack.length);

  const pileWidth = cardSize.width + STAGGER_X * (displayStack.length - 1);
  const pileHeight = cardSize.height + STAGGER_Y * (displayStack.length - 1);

  return (
    <AnimatePresence>
      <motion.div
        key="stack-container"
        initial={{ opacity: 0, x: 60 }}
        animate={{ opacity: 1, x: 0 }}
        exit={{ opacity: 0, x: 60 }}
        transition={{ type: "spring", stiffness: 300, damping: 30 }}
        className="fixed top-1/2 z-30 -translate-y-1/2"
        style={{
          right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
        }}
      >
        {/* Staggered pile — no wrapper chrome, just cards */}
        <div
          className="relative"
          style={{ width: pileWidth, height: pileHeight }}
        >
          <AnimatePresence mode="popLayout">
            {displayStack.map((entry, index) => (
              <StackEntry
                key={entry.id}
                entry={entry}
                index={index}
                isTop={index === 0}
                isPending={pendingEntry != null && entry.id === pendingEntry.id}
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
      </motion.div>
    </AnimatePresence>
  );
}
