import { useTranslation } from "react-i18next";

import type { GameAction, GameState, WaitingFor } from "../../adapter/types.ts";
import { ChoiceModal } from "./ChoiceModal.tsx";

type CipherEncodeWaitingFor = Extract<WaitingFor, { type: "CipherEncodeChoice" }>;

interface CipherEncodeChoiceModalProps {
  waitingFor: CipherEncodeWaitingFor;
  objects?: GameState["objects"];
  dispatch: (action: GameAction) => void | Promise<void>;
}

/**
 * CR 702.99a: A resolving Cipher spell may be exiled "encoded" on a creature its
 * controller controls. While the card stays encoded, that creature has "Whenever
 * this creature deals combat damage to a player, you may cast a copy of the
 * encoded card without paying its mana cost" (CR 702.99c). The controller picks a
 * host creature, or declines (the card is put into its owner's graveyard).
 *
 * Resolves `WaitingFor::CipherEncodeChoice` by dispatching
 * `GameAction::CipherEncode` (`creature: id` to encode, `null` to decline).
 */
export function CipherEncodeChoiceModalContent({
  waitingFor,
  objects,
  dispatch,
}: CipherEncodeChoiceModalProps) {
  const { t } = useTranslation("game");
  const { card_id: cardId, creatures } = waitingFor.data;
  const cardName = objects?.[cardId]?.name ?? t("cipher.cardFallback");

  const options = creatures.map((id) => ({
    id: String(id),
    label: objects?.[id]?.name ?? t("cipher.creatureFallback"),
    description: t("cipher.encodeDesc"),
  }));
  options.push({
    id: "decline",
    label: t("cipher.decline"),
    description: t("cipher.declineDesc"),
  });

  return (
    <ChoiceModal
      title={t("cipher.title", { name: cardName })}
      subtitle={t("cipher.subtitle")}
      previewCardName={cardName}
      previewObjectId={cardId}
      options={options}
      onChoose={(id) => {
        dispatch({
          type: "CipherEncode",
          data: { creature: id === "decline" ? null : Number(id) },
        });
      }}
    />
  );
}
