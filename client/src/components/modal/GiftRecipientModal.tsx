import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { formatAbilityCost } from "../../viewmodel/costLabel.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type GiftRecipientWaitingFor = Extract<WaitingFor, { type: "ChooseGiftRecipient" }>;

interface GiftRecipientModalContentProps {
  waitingFor: GiftRecipientWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 601.2 + CR 115.10a: The caster chooses one opponent while casting. The
 * engine-provided `purpose` says why: the recipient of a promised Gift
 * (CR 702.174a; absent `purpose` = Gift) or the opponent an effect-as-cost acts
 * on (CR 601.2h, e.g. "have an opponent gain 3 life"), whose cost is shown.
 *
 * Candidate order is engine-owned (`players::opponents` seat order) — render as
 * received; do not re-sort on the client.
 */
export function GiftRecipientModalContent({
  waitingFor,
  dispatch,
}: GiftRecipientModalContentProps) {
  const { t } = useTranslation("game");
  const candidates = waitingFor.data.candidates;
  const purpose = waitingFor.data.purpose;
  const effectCost = purpose?.type === "EffectCost" ? purpose.cost : null;
  const title = effectCost ? t("costRecipient.title") : t("giftRecipient.title");
  const subtitle = effectCost
    ? t("costRecipient.subtitle", { cost: formatAbilityCost(effectCost) })
    : t("giftRecipient.subtitle");

  return (
    <ChoiceModal
      title={title}
      subtitle={subtitle}
      options={candidates.map((opponent) => ({
        id: String(opponent),
        label: getOpponentDisplayName(opponent),
      }))}
      onChoose={(id) => {
        dispatch({
          type: "ChooseGiftRecipient",
          data: { opponent: Number(id) },
        });
      }}
    />
  );
}

export function GiftRecipientModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const dispatch = useGameDispatch();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (waitingFor?.type !== "ChooseGiftRecipient") return null;
  if (!canActForWaitingState) return null;

  return (
    <GiftRecipientModalContent waitingFor={waitingFor} dispatch={dispatch} />
  );
}
