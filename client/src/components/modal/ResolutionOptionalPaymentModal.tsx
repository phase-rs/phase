import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { formatAbilityCost } from "../../viewmodel/costLabel.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type ResolutionOptionalPaymentWaitingFor = Extract<
  WaitingFor,
  { type: "ResolutionOptionalPaymentChoice" }
>;

interface Props {
  waitingFor: ResolutionOptionalPaymentWaitingFor;
  canActForWaitingState: boolean;
  dispatch: (action: GameAction) => void | Promise<void>;
}

export function ResolutionOptionalPaymentModalContent({
  waitingFor,
  canActForWaitingState,
  dispatch,
}: Props) {
  const { t } = useTranslation("game");
  if (!canActForWaitingState) return null;

  return (
    <ChoiceModal
      title={t("resolutionOptionalPayment.title")}
      previewObjectId={waitingFor.data.source_id}
      options={[
        { id: "decline", label: t("resolutionOptionalPayment.decline") },
        ...waitingFor.data.costs.map((option) => ({
          id: `pay:${option.index}`,
          label: t("resolutionOptionalPayment.pay", {
            cost: formatAbilityCost(option.cost),
          }),
        })),
      ]}
      onChoose={(id) => {
        const choice = id === "decline"
          ? { type: "Decline" as const }
          : { type: "Pay" as const, data: { index: Number(id.slice(4)) } };
        dispatch({
          type: "ChooseResolutionOptionalPaymentBranch",
          data: { choice },
        });
      }}
    />
  );
}
