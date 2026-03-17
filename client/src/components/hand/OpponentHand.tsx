import { motion, AnimatePresence } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import type { ObjectId } from "../../adapter/types.ts";

interface OpponentHandProps {
  showCards?: boolean;
}

export function OpponentHand({ showCards = false }: OpponentHandProps) {
  const myId = usePlayerId();
  const focusedOpponent = useUiStore((s) => s.focusedOpponent);
  const gameState = useGameStore((s) => s.gameState);
  const opponents = gameState
    ? (gameState.seat_order ?? gameState.players.map((p) => p.id)).filter(
        (id) => id !== myId && !(gameState.eliminated_players ?? []).includes(id),
      )
    : [];
  const opponentId = focusedOpponent ?? opponents[0] ?? (myId === 0 ? 1 : 0);
  const opponent = gameState?.players[opponentId];
  const objects = useGameStore((s) => s.gameState?.objects);
  const revealedCards = useGameStore((s) => s.gameState?.revealed_cards);

  if (!opponent) return null;

  const cardCount = opponent.hand.length;
  const center = cardCount > 0 ? (cardCount - 1) / 2 : 0;

  // Cards extend above the container so they peek from the top edge.
  const BASE_Y = -15;

  return (
    <div
      className="flex min-h-[calc(var(--card-h)*0.78)] items-start justify-center overflow-visible px-4 pb-1"
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
  const inspectObject = useUiStore((s) => s.inspectObject);

  if (cardName && src) {
    return (
      <img
        src={src}
        alt={cardName}
        className="rounded-lg border border-gray-600 shadow-md object-cover"
        style={cardStyle}
        onMouseEnter={() => inspectObject(cardId)}
        onMouseLeave={() => inspectObject(null)}
        draggable={false}
      />
    );
  }

  return (
    <img
      src="/card-back.png"
      alt="Card back"
      className="rounded-lg border border-gray-600 shadow-md object-cover"
      style={cardStyle}
      draggable={false}
    />
  );
}
