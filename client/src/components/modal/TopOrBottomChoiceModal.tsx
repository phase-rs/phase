import { useTranslation } from "react-i18next";
import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type TopOrBottomWaitingFor = Extract<
  WaitingFor,
  { type: "TopOrBottomChoice" | "ClashCardPlacement" }
>;

interface TopOrBottomChoiceModalProps {
  waitingFor: TopOrBottomWaitingFor;
  objects?: GameState["objects"];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 401.4: The owner of the targeted permanent puts it on the top or
 * bottom of their library. This modal presents that binary choice.
 *
 * Also handles ClashCardPlacement (CR 702.11b) which uses the same
 * ChooseTopOrBottom game action.
 */
export function TopOrBottomChoiceModalContent({
  waitingFor,
  objects,
  dispatch,
}: TopOrBottomChoiceModalProps) {
  const { t } = useTranslation("game");
  const objectId =
    waitingFor.type === "TopOrBottomChoice"
      ? waitingFor.data.object_id
      : waitingFor.data.card;
  const cardName = objects?.[objectId]?.name ?? t("topOrBottom.cardFallback");

  return (
    <ChoiceModal
      title={t("topOrBottom.title", { name: cardName })}
      previewCardName={cardName}
      previewObjectId={objectId}
      options={[
        { id: "top", label: t("topOrBottom.top") },
        { id: "bottom", label: t("topOrBottom.bottom") },
      ]}
      onChoose={(id) => {
        dispatch({ type: "ChooseTopOrBottom", data: { top: id === "top" } });
      }}
    />
  );
}
