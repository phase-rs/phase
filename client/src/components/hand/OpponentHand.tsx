import { useMemo } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

import { getCardImageSrcSetProps } from "../card/cardImageSrcSet.ts";
import { useCardBackImage, useCardImage } from "../../hooks/useCardImage.ts";
import { useCardHover } from "../../hooks/useCardHover.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import type { GameObject, ObjectId, PlayerId } from "../../adapter/types.ts";
import { getOpponentIds, resolveFocusedOpponent } from "../../viewmodel/gameStateView.ts";
import {
  OPPONENT_CARD_SCALE,
  OPPONENT_HAND_VERTICAL_SCALE,
  handFanGeometry,
  handFanVerticalMetrics,
} from "./handFanPresentation.ts";

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

  if (!opponent) return null;

  const cardCount = opponent.hand.length;
  const isSplitLayout = layout === "split";
  const verticalMetrics = handFanVerticalMetrics(
    isCompactHeight,
    OPPONENT_HAND_VERTICAL_SCALE,
  );
  const fan = handFanGeometry(
    cardCount,
    "--opponent-hand-card-w",
    verticalMetrics.arcScale,
  );

  // Mirror the player's shallow bottom ribbon across the top edge: same arc
  // depth and tilt magnitude, with signs reversed so the hand opens downward.
  const minHeightClass = isSplitLayout
    ? "min-h-[calc(var(--card-h)*0.55)]"
    : isCompactHeight ? "min-h-[32px]" : "min-h-[calc(var(--card-h)*0.7)]";

  return (
    <div
      className={`flex items-start justify-center overflow-visible ${isSplitLayout ? "px-1 pb-0" : "px-4 pb-1"} ${minHeightClass}`}
      style={
        {
          perspective: "800px",
          "--opponent-hand-card-w": `calc(var(--card-w) * ${OPPONENT_CARD_SCALE})`,
          "--opponent-hand-card-h": `calc(var(--card-h) * ${OPPONENT_CARD_SCALE})`,
        } as React.CSSProperties
      }
    >
      <AnimatePresence>
        {opponent.hand.map((id, i) => {
          const obj = objects ? objects[id] : null;
          const showFace = showCards || (obj?.display_visible_to_viewer ?? false);
          const rotation = -fan.rotation(i);
          const arcOffset = fan.arc(i);

          return (
            <motion.div
              key={id}
              initial={{ opacity: 0, y: -60 }}
              animate={{
                opacity: 1,
                y: -(verticalMetrics.restingY + arcOffset),
                rotate: rotation,
              }}
              exit={{ opacity: 0, y: -60 }}
              transition={{ delay: i * 0.03, duration: 0.25 }}
              style={{ marginLeft: i > 0 ? fan.overlap : undefined, zIndex: i }}
              data-opponent-hand-card
              data-hand-rotation={rotation}
              data-hand-arc={arcOffset}
            >
              <OpponentCardThumbnail
                cardId={id}
                card={showFace ? obj ?? null : null}
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
  width: "var(--opponent-hand-card-w)",
  height: "var(--opponent-hand-card-h)",
  transform: "rotate(180deg)",
} as const;

/** Renders a single opponent hand card — face or back, same sizing either way. */
function HiddenOpponentCardThumbnail() {
  const { t } = useTranslation("game");
  const { src, advanceFailedSource } = useCardBackImage();

  return (
    <img
      src={src ?? CARD_BACK_URL}
      alt={t("hand.cardBack")}
      className="rounded-lg border border-gray-600 shadow-md object-cover"
      style={cardStyle}
      draggable={false}
      onError={() => src && advanceFailedSource?.(src)}
    />
  );
}

function VisibleOpponentCardThumbnail({ cardId, card }: { cardId: ObjectId; card: GameObject }) {
  const { src, rungs, advanceFailedSource } = useCardImage(card.name, {
    size: "small",
    oracleId: card.printed_ref?.oracle_id,
    faceName: card.printed_ref?.face_name,
    isToken: card.display_source === "Token",
    tokenImageRef: card.token_image_ref,
  });
  const { handlers: hoverHandlers } = useCardHover(cardId);

  if (src) {
    return (
      <img
        src={src}
        {...getCardImageSrcSetProps(src, rungs)}
        alt={card.name}
        // `pointer-events-auto` so the card is the hit-test target even when an
        // ancestor opts out of pointer events (the split-seat fan wrapper does,
        // so gaps between cards fall through to the seat header beneath). In the
        // default layout the ancestor chain is already interactive, so this is a
        // no-op there.
        className="pointer-events-auto rounded-lg border border-gray-600 shadow-md object-cover"
        style={cardStyle}
        draggable={false}
        {...hoverHandlers}
        onError={() => advanceFailedSource?.(src)}
      />
    );
  }

  return (
    <div
      role="img"
      aria-label={card.name}
      className="flex items-center justify-center rounded-lg border border-gray-600 bg-gray-700 text-xs text-gray-200 shadow-md"
      style={cardStyle}
      {...hoverHandlers}
    >
      {card.name}
    </div>
  );
}

/** Renders a single opponent hand card — face or back, same sizing either way. */
function OpponentCardThumbnail({ cardId, card }: { cardId: ObjectId; card: GameObject | null }) {
  return card
    ? <VisibleOpponentCardThumbnail cardId={cardId} card={card} />
    : <HiddenOpponentCardThumbnail />;
}
