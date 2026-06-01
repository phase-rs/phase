import { useTranslation } from "react-i18next";

import type { GameAction, PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type ClashOpponentWaitingFor = Extract<WaitingFor, { type: "ClashChooseOpponent" }>;

interface ClashOpponentModalContentProps {
  waitingFor: ClashOpponentWaitingFor;
  seatOrder?: PlayerId[];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 701.30b: "Clash with an opponent" requires the controller to choose the
 * opponent before both players reveal their top card.
 */
export function ClashOpponentModalContent({
  waitingFor,
  seatOrder,
  dispatch,
}: ClashOpponentModalContentProps) {
  const { t } = useTranslation("game");
  const candidates = [...waitingFor.data.candidates].sort((a, b) => {
    const aIdx = seatOrder?.indexOf(a) ?? a;
    const bIdx = seatOrder?.indexOf(b) ?? b;
    return aIdx - bIdx;
  });

  return (
    <ChoiceModal
      title={t("clashOpponent.title")}
      subtitle={t("clashOpponent.subtitle")}
      options={candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseClashOpponent",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function ClashOpponentModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  if (waitingFor?.type !== "ClashChooseOpponent") return null;
  if (!canActForWaitingState) return null;

  return (
    <ClashOpponentModalContent
      waitingFor={waitingFor}
      seatOrder={seatOrder}
      dispatch={dispatch}
    />
  );
}
