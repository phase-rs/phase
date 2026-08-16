import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { GameAction, PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type AnnouncingOpponentWaitingFor = Extract<
  WaitingFor,
  { type: "ChooseAnnouncingOpponent" }
>;

interface AnnouncingOpponentModalContentProps {
  waitingFor: AnnouncingOpponentWaitingFor;
  seatOrder?: PlayerId[];
  dispatch: (action: GameAction) => void | Promise<void>;
}

function targetLabel(
  targetType: AnnouncingOpponentWaitingFor["data"]["target_type"],
  t: TFunction<"game">,
) {
  if (targetType === "Land") return t("announcingOpponent.targetLand");
  if (targetType === "Creature") return t("announcingOpponent.targetCreature");
  return t("announcingOpponent.targetChoice");
}

/**
 * CR 601.2c + CR 115.1: For an "of an opponent's choice" target slot, the spell
 * controller chooses which opponent announces that slot's target before target
 * declaration begins.
 */
export function AnnouncingOpponentModalContent({
  waitingFor,
  seatOrder,
  dispatch,
}: AnnouncingOpponentModalContentProps) {
  const { t } = useTranslation("game");
  const candidates = [...waitingFor.data.candidates].sort((a, b) => {
    const aIdx = seatOrder?.indexOf(a) ?? a;
    const bIdx = seatOrder?.indexOf(b) ?? b;
    return aIdx - bIdx;
  });

  return (
    <ChoiceModal
      title={t("announcingOpponent.title", {
        current: waitingFor.data.choice_index,
        total: waitingFor.data.choice_count,
      })}
      subtitle={t("announcingOpponent.subtitle", {
        target: targetLabel(waitingFor.data.target_type, t),
        current: waitingFor.data.choice_index,
        total: waitingFor.data.choice_count,
      })}
      options={candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseAnnouncingOpponent",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function AnnouncingOpponentModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  if (waitingFor?.type !== "ChooseAnnouncingOpponent") return null;
  if (!canActForWaitingState) return null;

  return (
    <AnnouncingOpponentModalContent
      waitingFor={waitingFor}
      seatOrder={seatOrder}
      dispatch={dispatch}
    />
  );
}
