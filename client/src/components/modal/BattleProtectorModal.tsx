import { useCallback, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";

type BattleProtectorChoice = Extract<WaitingFor, { type: "BattleProtectorChoice" }>;

/**
 * CR 310.10 + CR 704.5w + CR 704.5x: Protector choice for a battle that
 * isn't being attacked. The battle's controller picks a legal opponent as
 * the new protector; the engine emits this modal only when `candidates.len()
 * > 1` (singleton is auto-applied engine-side).
 *
 * The picker is player-based (not card-based), so we render a simple button
 * grid of opponent names drawn from `gameState.players`. The battle's name
 * (source object) is shown for context, since this prompt can fire while
 * multiple battles are in play.
 */
export function BattleProtectorModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "BattleProtectorChoice") return null;
  if (!canActForWaitingState) return null;

  return <BattleProtectorContent data={waitingFor.data} />;
}

function BattleProtectorContent({ data }: { data: BattleProtectorChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const battleName = useGameStore(
    (s) => s.gameState?.objects[data.battle_id]?.name ?? t("battleProtector.battleFallback"),
  );
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const [selected, setSelected] = useState<PlayerId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected != null) {
      dispatch({ type: "ChooseBattleProtector", data: { protector: selected } });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title={t("battleProtector.title")}
      subtitle={t("battleProtector.subtitle", { name: battleName })}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected == null} />}
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {[...data.candidates].sort((a, b) => {
          const aIdx = seatOrder?.indexOf(a) ?? a;
          const bIdx = seatOrder?.indexOf(b) ?? b;
          return aIdx - bIdx;
        }).map((candidateId, index) => {
          const isSelected = selected === candidateId;
          const color = getSeatColor(candidateId, seatOrder);
          return (
            <motion.button
              key={candidateId}
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : candidateId)}
            >
              <span style={{ color }}>{getPlayerDisplayName(candidateId)}</span>
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
