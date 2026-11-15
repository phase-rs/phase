import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type EntryControllerWaitingFor = Extract<
  WaitingFor,
  { type: "EntryControllerChoice" }
>;

interface EntryControllerModalContentProps {
  waitingFor: EntryControllerWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/** CR 614.12a: choose an opponent before the permanent enters the battlefield. */
export function EntryControllerModalContent({
  waitingFor,
  dispatch,
}: EntryControllerModalContentProps) {
  const { t } = useTranslation("game");

  return (
    <ChoiceModal
      title={t("entryController.title")}
      subtitle={t("entryController.subtitle")}
      options={waitingFor.data.candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseEntryController",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function EntryControllerModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "EntryControllerChoice") return null;
  if (!canActForWaitingState) return null;

  return <EntryControllerModalContent waitingFor={waitingFor} dispatch={dispatch} />;
}
