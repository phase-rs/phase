import { useTranslation } from "react-i18next";

import type { GameAction, GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type ReplacementDecisionOrderWaitingFor = Extract<
  WaitingFor,
  { type: "ReplacementDecisionOrder" }
>;

interface ReplacementDecisionOrderModalContentProps {
  waitingFor: ReplacementDecisionOrderWaitingFor;
  objects: Record<string, GameObject> | undefined;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 101.4c: the player has several simultaneous replacement decisions and
 * chooses which to make first. Each option is the engine-provided object the
 * decision is about.
 */
export function ReplacementDecisionOrderModalContent({
  waitingFor,
  objects,
  dispatch,
}: ReplacementDecisionOrderModalContentProps) {
  const { t } = useTranslation("game");
  const label = (object: ObjectId) =>
    objects?.[String(object)]?.name ?? t("replacementDecisionOrder.unknownObject");

  return (
    <ChoiceModal
      title={t("replacementDecisionOrder.title")}
      subtitle={t("replacementDecisionOrder.subtitle")}
      options={waitingFor.data.objects.map((object) => ({
        id: String(object),
        label: label(object),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "SelectCards",
          data: { cards: [Number(id)] },
        });
      }}
    />
  );
}

export function ReplacementDecisionOrderModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "ReplacementDecisionOrder") return null;
  if (!canActForWaitingState) return null;

  return (
    <ReplacementDecisionOrderModalContent
      waitingFor={waitingFor}
      objects={objects}
      dispatch={dispatch}
    />
  );
}
