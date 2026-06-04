import { useTranslation } from "react-i18next";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type MutateMergeWaitingFor = Extract<WaitingFor, { type: "MutateMergeChoice" }>;

interface MutateMergeChoiceModalProps {
  waitingFor: MutateMergeWaitingFor;
  objects?: GameState["objects"];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 702.140c + CR 730.2a: A mutating creature spell resolved with a legal
 * target. Its controller chooses whether the spell is put on TOP of or UNDER
 * the target creature. The choice only selects which component supplies the
 * copiable characteristics (name, types, power/toughness) — the merged
 * permanent always has the union of every component's abilities (CR 702.140e),
 * keeps the target creature's identity/`ObjectId` (CR 730.2c), and does not
 * re-enter the battlefield.
 *
 * Resolves `WaitingFor::MutateMergeChoice` by dispatching
 * `GameAction::ChooseMutateMergeSide`.
 */
export function MutateMergeChoiceModalContent({
  waitingFor,
  objects,
  dispatch,
}: MutateMergeChoiceModalProps) {
  const { t } = useTranslation("game");
  const { merging_id: mergingId } = waitingFor.data;
  const mergingName = objects?.[mergingId]?.name ?? t("mutateMerge.cardFallback");

  return (
    <ChoiceModal
      title={t("mutateMerge.title", { name: mergingName })}
      subtitle={t("mutateMerge.subtitle")}
      previewCardName={mergingName}
      previewObjectId={mergingId}
      options={[
        {
          id: "top",
          label: t("mutateMerge.top"),
          description: t("mutateMerge.topDesc"),
        },
        {
          id: "bottom",
          label: t("mutateMerge.bottom"),
          description: t("mutateMerge.bottomDesc"),
        },
      ]}
      onChoose={(id) => {
        dispatch({
          type: "ChooseMutateMergeSide",
          data: { side: id === "top" ? "Top" : "Bottom" },
        });
      }}
    />
  );
}
