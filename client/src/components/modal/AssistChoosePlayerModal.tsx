import { useTranslation } from "react-i18next";

import type { GameAction, PlayerId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type AssistChoosePlayerWaitingFor = Extract<WaitingFor, { type: "AssistChoosePlayer" }>;

interface AssistChoosePlayerModalContentProps {
  waitingFor: AssistChoosePlayerWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 702.132a: Assist — when a spell with assist has a generic mana component,
 * the caster MAY choose another player to help pay it before paying the total
 * cost. The caster acts on this step; the engine supplies the legal `candidates`
 * verbatim. Declining dispatches `player: null` (the engine rejects PassPriority
 * here, so the explicit decline action is required).
 */
export function AssistChoosePlayerModalContent({
  waitingFor,
  dispatch,
}: AssistChoosePlayerModalContentProps) {
  const { t } = useTranslation("game");
  const { candidates, max_generic } = waitingFor.data;

  const decline = () => {
    dispatch({ type: "ChooseAssistPlayer", data: { player: null } });
  };

  return (
    <ChoiceModal
      title={t("assist.choose.title")}
      subtitle={t("assist.choose.subtitle", { max: max_generic })}
      options={candidates.map((helper: PlayerId) => ({
        id: String(helper),
        label: getOpponentDisplayName(helper),
      }))}
      onChoose={(id) => {
        dispatch({ type: "ChooseAssistPlayer", data: { player: Number(id) } });
      }}
      footer={
        <button
          onClick={decline}
          className={gameButtonClass({ tone: "slate", size: "md" })}
        >
          {t("assist.choose.decline")}
        </button>
      }
    />
  );
}

export function AssistChoosePlayerModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "AssistChoosePlayer") return null;
  if (!canActForWaitingState) return null;

  return (
    <AssistChoosePlayerModalContent
      waitingFor={waitingFor}
      dispatch={dispatch}
    />
  );
}
