import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useMemo } from "react";

import type { ManaType } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaBadge } from "./ManaBadge.tsx";

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

export function ManaPaymentUI() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const gameState = useGameStore((s) => s.gameState);
  const dispatch = useGameStore((s) => s.dispatch);

  const isManaPayment = waitingFor?.type === "ManaPayment";
  const playerId = isManaPayment ? waitingFor.data.player : null;
  const player = playerId != null ? gameState?.players[playerId] : null;

  // Summarize mana pool by color
  const manaPoolSummary = useMemo(() => {
    if (!player) return [];
    const counts: Record<ManaType, number> = {
      White: 0, Blue: 0, Black: 0, Red: 0, Green: 0, Colorless: 0,
    };
    for (const unit of player.mana_pool.mana) {
      counts[unit.color]++;
    }
    return MANA_ORDER.filter((c) => counts[c] > 0).map((c) => ({ color: c, amount: counts[c] }));
  }, [player]);

  // Find untapped lands for manual tapping
  const untappedLands = useMemo(() => {
    if (!gameState || playerId == null) return [];
    return gameState.battlefield
      .map((id) => gameState.objects[id])
      .filter(
        (obj) =>
          obj &&
          obj.controller === playerId &&
          obj.card_types.core_types.includes("Land") &&
          !obj.tapped,
      );
  }, [gameState, playerId]);

  const handleTapLand = useCallback(
    (objectId: number) => {
      dispatch({ type: "TapLandForMana", data: { object_id: objectId } });
    },
    [dispatch],
  );

  const handleAutoPay = useCallback(() => {
    dispatch({ type: "PassPriority" });
  }, [dispatch]);

  if (!isManaPayment || !player) return null;

  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-x-0 bottom-0 z-40 flex justify-center pb-4"
        initial={{ y: 80, opacity: 0 }}
        animate={{ y: 0, opacity: 1 }}
        exit={{ y: 80, opacity: 0 }}
        transition={{ duration: 0.25 }}
      >
        <div className="rounded-xl bg-gray-900/95 p-4 shadow-2xl ring-1 ring-gray-700">
          <h3 className="mb-3 text-center text-sm font-semibold text-gray-300">
            Pay Mana Cost
          </h3>

          {/* Current mana pool */}
          <div className="mb-3 flex items-center justify-center gap-2">
            <span className="text-xs text-gray-500">Pool:</span>
            {manaPoolSummary.length > 0 ? (
              manaPoolSummary.map(({ color, amount }) => (
                <ManaBadge key={color} color={color} amount={amount} />
              ))
            ) : (
              <span className="text-xs text-gray-600">Empty</span>
            )}
          </div>

          {/* Untapped lands for manual override */}
          {untappedLands.length > 0 && (
            <div className="mb-3">
              <p className="mb-1 text-center text-xs text-gray-500">
                Tap a land for mana:
              </p>
              <div className="flex flex-wrap justify-center gap-1">
                {untappedLands.map((land) => (
                  <button
                    key={land.id}
                    onClick={() => handleTapLand(land.id)}
                    className="rounded bg-gray-800 px-2 py-1 text-xs text-white ring-1 ring-white/20 transition hover:bg-gray-700 hover:ring-white/50"
                  >
                    {land.name}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Action buttons */}
          <div className="flex justify-center gap-3">
            <button
              onClick={handleAutoPay}
              className="rounded-lg bg-cyan-600 px-5 py-1.5 text-sm font-semibold text-white transition hover:bg-cyan-500"
            >
              Auto Pay
            </button>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
