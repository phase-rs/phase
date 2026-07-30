import { useTranslation } from "react-i18next";
import type { GameAction, PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type PileOpponentWaitingFor = Extract<
  WaitingFor,
  { type: "SeparatePilesChooseOpponent" }
>;

interface PileOpponentModalContentProps {
  waitingFor: PileOpponentWaitingFor;
  seatOrder?: PlayerId[];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 700.3: When a spell or ability instructs "an opponent" to separate cards
 * into piles, the controller chooses which opponent performs the separation.
 */
export function PileOpponentModalContent({
  waitingFor,
  seatOrder,
  dispatch,
}: PileOpponentModalContentProps) {
  const { t } = useTranslation("game");
  const candidates = [...waitingFor.data.candidates].sort((a, b) => {
    const aIdx = seatOrder?.indexOf(a) ?? a;
    const bIdx = seatOrder?.indexOf(b) ?? b;
    return aIdx - bIdx;
  });
  return (
    <ChoiceModal
      title={t("pileOpponent.title", "Choose Opponent")}
      subtitle={t(
        "pileOpponent.subtitle",
        "Choose which opponent separates the cards into piles.",
      )}
      options={candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChoosePileOpponent",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function PileOpponentModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  if (waitingFor?.type !== "SeparatePilesChooseOpponent") return null;
  if (!canActForWaitingState) return null;
  return (
    <PileOpponentModalContent
      waitingFor={waitingFor}
      seatOrder={seatOrder}
      dispatch={dispatch}
    />
  );
}
