import { motion } from "framer-motion";
import { useEffect, useRef } from "react";

import { useCardImage } from "../../hooks/useCardImage";

interface MillCard {
  objectId: number;
  cardName: string;
  colors: string[];
}

interface MillRevealAnimationProps {
  cards: MillCard[];
  from: { x: number; y: number };
  to: { x: number; y: number };
  onComplete: () => void;
}

const CARD_WIDTH = 80;
const CARD_HEIGHT = 112;
const STAGGER_MS = 80;
const FLIGHT_DURATION = 0.35;

function MillCardElement({
  card,
  from,
  to,
  index,
  isLast,
  onComplete,
}: {
  card: MillCard;
  from: { x: number; y: number };
  to: { x: number; y: number };
  index: number;
  isLast: boolean;
  onComplete: () => void;
}) {
  // `normal`, not `small`: this renders a bare image element with no ladder at
  // CARD_WIDTH 80 (160 device px at DPR 2), so the real 146px asset would
  // upscale. Requesting `normal` keeps this overlay byte-identical to before
  // `small` became a distinct asset.
  const { src } = useCardImage(card.cardName, { size: "normal" });
  const glowColor = card.colors.length > 0
    ? card.colors[0]
    : "#6366f1";
  const delay = index * (STAGGER_MS / 1000);

  const midX = (from.x + to.x) / 2;
  const midY = Math.min(from.y, to.y) - 60;

  return (
    <motion.div
      initial={{
        x: from.x - CARD_WIDTH / 2,
        y: from.y - CARD_HEIGHT / 2,
        scale: 0.6,
        opacity: 0,
      }}
      animate={{
        x: [from.x - CARD_WIDTH / 2, midX - CARD_WIDTH / 2, to.x - CARD_WIDTH / 2],
        y: [from.y - CARD_HEIGHT / 2, midY - CARD_HEIGHT / 2, to.y - CARD_HEIGHT / 2],
        scale: [0.6, 1, 0.8],
        opacity: [0, 1, 0],
      }}
      transition={{
        duration: FLIGHT_DURATION,
        delay,
        ease: "easeInOut",
        times: [0, 0.4, 1],
      }}
      onAnimationComplete={isLast ? onComplete : undefined}
      style={{
        position: "fixed",
        left: 0,
        top: 0,
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        pointerEvents: "none",
        zIndex: 45,
        borderRadius: 6,
        overflow: "hidden",
      }}
    >
      {src ? (
        <img
          src={src}
          alt={card.cardName}
          style={{ width: "100%", height: "100%", objectFit: "cover" }}
        />
      ) : (
        <div
          style={{
            width: "100%",
            height: "100%",
            backgroundColor: "rgba(0,0,0,0.7)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: "white",
            fontSize: "0.6rem",
            textAlign: "center",
            padding: 4,
          }}
        >
          {card.cardName}
        </div>
      )}
      <motion.div
        initial={{ boxShadow: `0 0 4px ${glowColor}33` }}
        animate={{
          boxShadow: [
            `0 0 4px ${glowColor}33`,
            `0 0 16px ${glowColor}99`,
            `0 0 8px ${glowColor}66`,
          ],
        }}
        transition={{ duration: FLIGHT_DURATION, delay, times: [0, 0.4, 1] }}
        style={{
          position: "absolute",
          inset: 0,
          borderRadius: 6,
          pointerEvents: "none",
        }}
      />
    </motion.div>
  );
}

export function MillRevealAnimation({
  cards,
  from,
  to,
  onComplete,
}: MillRevealAnimationProps) {
  const completedRef = useRef(false);
  const displayedCards = cards;

  // Safety timeout: if onAnimationComplete never fires, clean up after expected duration + buffer
  useEffect(() => {
    const expectedMs = (displayedCards.length - 1) * STAGGER_MS + FLIGHT_DURATION * 1000 + 500;
    const timer = setTimeout(() => {
      if (!completedRef.current) {
        completedRef.current = true;
        onComplete();
      }
    }, expectedMs);
    return () => clearTimeout(timer);
  }, [displayedCards.length, onComplete]);

  const handleComplete = () => {
    if (!completedRef.current) {
      completedRef.current = true;
      onComplete();
    }
  };

  return (
    <>
      {displayedCards.map((card, i) => (
        <MillCardElement
          key={`mill-${card.objectId}`}
          card={card}
          from={from}
          to={to}
          index={i}
          isLast={i === displayedCards.length - 1}
          onComplete={handleComplete}
        />
      ))}
    </>
  );
}
