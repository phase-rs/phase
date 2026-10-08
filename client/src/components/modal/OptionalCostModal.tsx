import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { additionalCostChoices } from "../../viewmodel/costLabel.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type OptionalCostWaitingFor = Extract<WaitingFor, { type: "OptionalCostChoice" }>;

interface OptionalCostModalProps {
  waitingFor: OptionalCostWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

function giftKindLabel(
  giftKind: OptionalCostWaitingFor["data"]["gift_kind"],
  t: TFunction<"game">,
): string {
  switch (giftKind?.type) {
    case "Treasure":
      return t("optionalCost.gift.kind.treasure");
    case "Food":
      return t("optionalCost.gift.kind.food");
    case "TappedFish":
      return t("optionalCost.gift.kind.tappedFish");
    // CR 702.174g: the chosen player takes an extra turn after this one.
    case "ExtraTurn":
      return t("optionalCost.gift.kind.extraTurn");
    default:
      return t("optionalCost.gift.kind.card");
  }
}

/**
 * Modal for `WaitingFor::OptionalCostChoice` — kicker / multikicker / Casualty
 * / Gift / "or pay" additional-cost prompts.
 *
 * The decline option is an explicit, descriptively-labelled primary button
 * (`pay: false` → finish the cast), kept distinct from the X / backdrop abort
 * affordance (`CancelCast` → cancel the cast, CR 601.2). For repeatable
 * multikicker (CR 702.33c/d) the `times_kicked` count drives the title and
 * the "Done — finish casting (kicked N×)" decline label.
 */
export function OptionalCostModalContent({
  waitingFor,
  dispatch,
}: OptionalCostModalProps) {
  const { t } = useTranslation("game");
  const { cost, times_kicked, origin, gift_kind } = waitingFor.data;
  // CR 702.174a: Gift promise copy is engine-identified via origin/gift_kind;
  // localize here so chrome strings go through `t()` (not card/Oracle text).
  const { title, options } =
    origin === "Gift"
      ? {
          title: t("optionalCost.gift.title"),
          options: [
            {
              id: "pay" as const,
              label: t("optionalCost.gift.pay", {
                gift: giftKindLabel(gift_kind, t),
              }),
            },
            {
              id: "decline" as const,
              label: t("optionalCost.gift.decline"),
            },
          ],
        }
      : additionalCostChoices(cost, times_kicked);
  // Mandatory Choice costs (e.g. "discard a card or pay 3 life") require
  // picking one — no abort allowed. All other costs allow aborting the cast.
  const isMandatoryChoice = cost.type === "Choice";

  return (
    <ChoiceModal
      title={title}
      options={options}
      onChoose={(id) =>
        dispatch({ type: "DecideOptionalCost", data: { pay: id === "pay" } })
      }
      onClose={isMandatoryChoice ? undefined : () => dispatch({ type: "CancelCast" })}
    />
  );
}
