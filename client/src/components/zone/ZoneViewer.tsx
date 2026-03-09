import { useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";

interface ZoneViewerProps {
  zone: "graveyard" | "exile";
  playerId: number;
  onClose: () => void;
}

const ZONE_TITLES: Record<string, string> = {
  graveyard: "Graveyard",
  exile: "Exile",
};

export function ZoneViewer({ zone, playerId, onClose }: ZoneViewerProps) {
  const gameState = useGameStore((s) => s.gameState);

  const cards = useMemo(() => {
    if (!gameState) return [];

    const ids =
      zone === "graveyard"
        ? (gameState.players[playerId]?.graveyard ?? [])
        : gameState.exile.filter((id) => gameState.objects[id]?.owner === playerId);

    return ids.map((id) => gameState.objects[id]).filter(Boolean);
  }, [gameState, zone, playerId]);

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Backdrop */}
        <div className="absolute inset-0 bg-black/60" onClick={onClose} />

        {/* Modal content */}
        <motion.div
          className="relative z-10 w-full max-w-3xl rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700"
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.9, opacity: 0 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          {/* Header */}
          <div className="mb-4 flex items-center justify-between">
            <h2 className="text-lg font-bold text-white">
              {ZONE_TITLES[zone]} ({cards.length})
            </h2>
            <button
              onClick={onClose}
              className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              aria-label="Close zone viewer"
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5">
                <path d="M6.28 5.22a.75.75 0 0 0-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 1 0 1.06 1.06L10 11.06l3.72 3.72a.75.75 0 1 0 1.06-1.06L11.06 10l3.72-3.72a.75.75 0 0 0-1.06-1.06L10 8.94 6.28 5.22Z" />
              </svg>
            </button>
          </div>

          {/* Card grid */}
          <div className="max-h-[60vh] overflow-y-auto">
            {cards.length === 0 ? (
              <p className="py-8 text-center text-sm italic text-gray-600">
                No cards in {ZONE_TITLES[zone].toLowerCase()}
              </p>
            ) : (
              <div className="grid grid-cols-3 gap-3 sm:grid-cols-4 md:grid-cols-5">
                {cards.map((obj) => (
                  <div key={obj.id} className="rounded-lg border border-gray-700 bg-gray-800/60 p-1">
                    <CardImage cardName={obj.name} size="normal" className="!w-[120px] !h-[168px]" />
                  </div>
                ))}
              </div>
            )}
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
