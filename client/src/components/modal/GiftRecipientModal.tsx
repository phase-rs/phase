import { useTranslation } from "react-i18next";

import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type GiftRecipientWaitingFor = Extract<WaitingFor, { type: "ChooseGiftRecipient" }>;

interface GiftRecipientModalContentProps {
  waitingFor: GiftRecipientWaitingFor;
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 702.174a: After promising a Gift with ≥2 opponents, the caster chooses
 * which opponent receives the gift.
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

  return (
    <ChoiceModal
      title={t("giftRecipient.title")}
      subtitle={t("giftRecipient.subtitle")}
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
