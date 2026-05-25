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
  const objectId =
    waitingFor.type === "TopOrBottomChoice"
      ? waitingFor.data.object_id
      : waitingFor.data.card;
  const cardName = objects?.[objectId]?.name ?? "Card";

  return (
    <ChoiceModal
      title={`Put ${cardName} on top or bottom of library`}
      previewCardName={cardName}
      options={[
        { id: "top", label: "Top of library" },
        { id: "bottom", label: "Bottom of library" },
      ]}
      onChoose={(id) => {
        dispatch({ type: "ChooseTopOrBottom", data: { top: id === "top" } });
      }}
    />
  );
}
