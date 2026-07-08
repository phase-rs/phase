import { useMemo } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { getOpponentIds, resolveFocusedOpponent } from "../../viewmodel/gameStateView.ts";

interface OpponentHandProps {
  playerId?: PlayerId;
  showCards?: boolean;
  layout?: "default" | "split";
}

export function OpponentHand({ playerId, showCards = false, layout = "default" }: OpponentHandProps) {
  const myId = usePerspectivePlayerId();
  const isCompactHeight = useIsCompactHeight();
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const gameState = useGameStore((s) => s.gameState);
  const players = useGameStore((s) => s.gameState?.players);
  const opponents = useMemo(() => {
    return getOpponentIds(gameState ?? null, myId);
  }, [gameState, myId]);
  const opponentId =
    playerId
    ?? resolveFocusedOpponent(focusedOpponent, opponents)
    ?? (myId === 0 ? 1 : 0);
  const opponent = players?.[opponentId];
  const objects = useGameStore((s) => s.gameState?.objects);
  const revealedCards = useGameStore((s) => s.gameState?.revealed_cards);
  const publicRevealedCards = useGameStore((s) => s.gameState?.public_revealed_cards);

  if (!opponent) return null;

  const cardCount = opponent.hand.length;
  const center = cardCount > 0 ? (cardCount - 1) / 2 : 0;
  const isSplitLayout = layout === "split";

  // Cards extend above the container so they peek from the top edge.
  const baseY = isSplitLayout ? 0 : -15;
  const curveY = isSplitLayout ? 1.4 : 6;
  const rotationStep = isSplitLayout ? 3.5 : 6;
  const overlap = isSplitLayout ? "calc(var(--card-w) * -0.2)" : "-16px";
  const minHeightClass = isSplitLayout
    ? "min-h-[calc(var(--card-h)*0.55)]"
    : isCompactHeight ? "min-h-[32px]" : "min-h-[calc(var(--card-h)*0.7)]";

  return (
    <div
      className={`flex items-start justify-center overflow-visible ${isSplitLayout ? "px-1 pb-0" : "px-4 pb-1"} ${minHeightClass}`}
      style={{ perspective: "800px" }}
    >
      <AnimatePresence>
        {opponent.hand.map((id, i) => {
          const obj = objects ? objects[id] : null;
          const isRevealed = (revealedCards?.includes(id) ?? false)
            || (publicRevealedCards?.includes(id) ?? false);
          const showFace = showCards || isRevealed;
          // Negate rotation so fan opens toward opponent (top of screen)
          const rotation = -((i - center) * rotationStep);

          return (
            <motion.div
              key={id}
              initial={{ opacity: 0, y: -60 }}
              animate={{
                opacity: 1,
                y: baseY - Math.abs(i - center) ** 2 * curveY,
                rotate: rotation,
              }}
              exit={{ opacity: 0, y: -60 }}
              transition={{ delay: i * 0.03, duration: 0.25 }}
              style={{ marginLeft: i > 0 ? overlap : undefined, zIndex: i }}
            >
              <OpponentCardThumbnail
                cardId={id}
                cardName={showFace && obj ? obj.name : null}
              />
            </motion.div>
          );
        })}
      </AnimatePresence>
      {cardCount > 5 && (
        <span className="ml-2 rounded bg-gray-700 px-1.5 py-0.5 text-xs font-medium text-gray-300">
          {cardCount}
        </span>
      )}
    </div>
  );
}

const cardStyle = {
  width: "calc(var(--card-w) * 0.78)",
  height: "calc(var(--card-h) * 0.78)",
  transform: "rotate(180deg)",
} as const;

/** Renders a single opponent hand card — face or back, same sizing either way. */
function OpponentCardThumbnail({ cardId, cardName }: { cardId: ObjectId; cardName: string | null }) {
  const { t } = useTranslation("game");
  const { src } = useCardImage(cardName ?? "", { size: "small" });
  const { handlers: hoverHandlers } = useCardHover(cardName ? cardId : null);

  if (cardName && src) {
    return (
      <img
        src={src}
        alt={cardName}
        className="rounded-lg border border-gray-600 shadow-md object-cover"
        style={cardStyle}
        draggable={false}
        {...hoverHandlers}
      />
    );
  }

  return (
    <img
      src={CARD_BACK_URL}
      alt={t("hand.cardBack")}
      className="rounded-lg border border-gray-600 shadow-md object-cover"
      style={cardStyle}
      draggable={false}
    />
  );
}
