import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { GameAction, ObjectId, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type SpliceOfferWaitingFor = Extract<WaitingFor, { type: "SpliceOffer" }>;

function respond(card: ObjectId | null): GameAction {
  return { type: "RespondToSpliceOffer", data: { card } };
}

export function SpliceOfferModal() {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (waitingFor?.type !== "SpliceOffer") return null;

  return (
    <SpliceOfferModalContent
      waitingFor={waitingFor}
      objectName={(id) =>
        objects?.[id]?.name ?? t("gamePage.spliceOffer.unknownCard", { id })
      }
      dispatch={dispatch}
      t={t}
    />
  );
}

function SpliceOfferModalContent({
  waitingFor,
  objectName,
  dispatch,
  t,
}: {
  waitingFor: SpliceOfferWaitingFor;
  objectName: (id: ObjectId) => string;
  dispatch: (action: GameAction) => void | Promise<void>;
  t: TFunction<"game">;
}) {
  const options = [
    ...waitingFor.data.eligible.map((card) => {
      const name = objectName(card);
      return {
        id: String(card),
        label: t("gamePage.spliceOffer.spliceCard", { name }),
      };
    }),
    {
      id: "decline",
      label: t("gamePage.spliceOffer.decline"),
      description: t("gamePage.spliceOffer.declineDescription"),
    },
  ];

  const choose = (id: string) => {
    const card = id === "decline" ? null : Number.parseInt(id, 10);
    void dispatch(respond(card));
  };

  return (
    <ChoiceModal
      title={t("gamePage.spliceOffer.title")}
      subtitle={t("gamePage.spliceOffer.subtitle")}
      options={options}
      onChoose={choose}
      onClose={() => void dispatch(respond(null))}
    />
  );
}
