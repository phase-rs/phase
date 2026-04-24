import { useMemo } from "react";
import { motion, AnimatePresence } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import type { ObjectId } from "../../adapter/types.ts";

interface OpponentHandProps {
  showCards?: boolean;
}

export function OpponentHand({ showCards = false }: OpponentHandProps) {
  const myId = usePerspectivePlayerId();
  const isCompactHeight = useIsCompactHeight();
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const eliminatedPlayers = useGameStore((s) => s.gameState?.eliminated_players);
  const players = useGameStore((s) => s.gameState?.players);
  const opponents = useMemo(() => {
    const orderedPlayers = seatOrder ?? players?.map((player) => player.id) ?? [];
    const eliminated = new Set(eliminatedPlayers ?? []);
    return orderedPlayers.filter((id) => id !== myId && !eliminated.has(id));
  }, [eliminatedPlayers, myId, players, seatOrder]);
  const opponentId = focusedOpponent ?? opponents[0] ?? (myId === 0 ? 1 : 0);
  const opponent = players?.[opponentId];
  const objects = useGameStore((s) => s.gameState?.objects);
  const revealedCards = useGameStore((s) => s.gameState?.revealed_cards);

  if (!opponent) return null;

  const cardCount = opponent.hand.length;
  const center = cardCount > 0 ? (cardCount - 1) / 2 : 0;

  // Cards extend above the container so they peek from the top edge.
  const BASE_Y = -15;

  return (
    <div
      className={`flex shrink-0 items-start justify-center overflow-visible px-4 pb-1 ${
        isCompactHeight ? "min-h-[32px]" : "min-h-[calc(var(--card-h)*1.2)]"
      }`}
      style={{ perspective: "800px" }}
    >
      <AnimatePresence>
        {opponent.hand.map((id, i) => {
          const obj = objects ? objects[id] : null;
          const isRevealed = revealedCards?.includes(id) ?? false;
          const showFace = showCards || isRevealed;
          // Negate rotation so fan opens toward opponent (top of screen)
          const rotation = -((i - center) * 6);

          return (
            <motion.div
              key={id}
              initial={{ opacity: 0, y: -60 }}
              animate={{
                opacity: 1,
                y: BASE_Y - Math.abs(i - center) ** 2 * 6,
                rotate: rotation,
              }}
              exit={{ opacity: 0, y: -60 }}
              transition={{ delay: i * 0.03, duration: 0.25 }}
              style={{ marginLeft: i > 0 ? "-16px" : undefined, zIndex: i }}
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
      alt="Card back"
      className="rounded-lg border border-gray-600 shadow-md object-cover"
      style={cardStyle}
      draggable={false}
    />
  );
}
