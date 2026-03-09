import { motion, AnimatePresence } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";

interface OpponentHandProps {
  showCards?: boolean;
}

export function OpponentHand({ showCards = false }: OpponentHandProps) {
  const opponent = useGameStore((s) => s.gameState?.players[1]);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!opponent) return null;

  const cardCount = opponent.hand.length;

  if (cardCount === 0) return null;

  const center = (cardCount - 1) / 2;

  // Base offset pushes cards partially offscreen; rotation compensation keeps fan arc smooth
  const BASE_Y = -40;
  const ROTATION_COMPENSATION = 0.8;

  return (
    <div
      className="flex items-start justify-center overflow-hidden px-4 py-1"
      style={{ perspective: "800px" }}
    >
      <AnimatePresence>
        {opponent.hand.map((id, i) => {
          const obj = showCards && objects ? objects[id] : null;
          // Negate rotation so fan opens toward opponent (top of screen)
          const rotation = -((i - center) * 6);

          return (
            <motion.div
              key={id}
              initial={{ opacity: 0, y: -60 }}
              animate={{
                opacity: 1,
                y: BASE_Y - Math.abs(rotation) * ROTATION_COMPENSATION,
                rotate: rotation,
              }}
              exit={{ opacity: 0, y: -60 }}
              transition={{ delay: i * 0.03, duration: 0.25 }}
              style={{ marginLeft: i > 0 ? "-16px" : undefined, zIndex: i }}
            >
              {obj ? (
                <div style={{ transform: "scale(0.6)", transformOrigin: "top left", width: "calc(var(--card-w) * 0.6)", height: "calc(var(--card-h) * 0.6)" }}>
                  <CardImage cardName={obj.name} size="small" />
                </div>
              ) : (
                <div
                  className="rounded-lg border border-gray-600 bg-gradient-to-br from-gray-800 via-gray-700 to-gray-800 shadow-md"
                  style={{
                    width: "calc(var(--card-w) * 0.6)",
                    height: "calc(var(--card-h) * 0.6)",
                  }}
                >
                  <div className="flex h-full items-center justify-center">
                    <div className="h-[70%] w-[70%] rounded border border-gray-500 bg-gradient-to-br from-amber-900/40 via-amber-800/30 to-amber-900/40" />
                  </div>
                </div>
              )}
            </motion.div>
          );
        })}
      </AnimatePresence>
      {cardCount > 5 && (
        <span className="ml-2 rounded bg-gray-700 px-1.5 py-0.5 text-xs font-medium text-gray-300">
          {cardCount}
        </span>
      )}
    </div>
  );
}
