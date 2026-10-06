import { motion } from "framer-motion";
import { useEffect, useRef } from "react";

import { ResolvedAnimationImage } from "./ResolvedAnimationImage.tsx";
import type { MillCard } from "./MillRevealAnimation.tsx";

interface RippleRevealAnimationProps {
  cards: MillCard[];
  /** Origin the fan flies out from (the revealing player's library pile). */
  from: { x: number; y: number };
  onComplete: () => void;
}

const CARD_WIDTH = 88;
const CARD_HEIGHT = 123;
const CARD_GAP = 12;
const STAGGER_MS = 70;
const FLIGHT_DURATION = 0.32;
const HOLD_MS = 1200;

function RippleCardElement({
  card,
  from,
  target,
  index,
  isLast,
  onComplete,
}: {
  card: MillCard;
  from: { x: number; y: number };
  target: { x: number; y: number };
  index: number;
  isLast: boolean;
  onComplete: () => void;
}) {
  const glowColor = card.colors.length > 0 ? card.colors[0] : "#f59e0b";
  const delay = index * (STAGGER_MS / 1000);
  const holdSeconds = HOLD_MS / 1000;

  return (
    <motion.div
      initial={{
        x: from.x - CARD_WIDTH / 2,
        y: from.y - CARD_HEIGHT / 2,
        scale: 0.5,
        opacity: 0,
      }}
      animate={{
        x: [
          from.x - CARD_WIDTH / 2,
          target.x - CARD_WIDTH / 2,
          target.x - CARD_WIDTH / 2,
          target.x - CARD_WIDTH / 2,
        ],
        y: [
          from.y - CARD_HEIGHT / 2,
          target.y - CARD_HEIGHT / 2,
          target.y - CARD_HEIGHT / 2,
          target.y - CARD_HEIGHT / 2,
        ],
        scale: [0.5, 1, 1, 0.85],
        opacity: [0, 1, 1, 0],
      }}
      transition={{
        duration: FLIGHT_DURATION + holdSeconds,
        delay,
        ease: "easeInOut",
        times: [0, FLIGHT_DURATION / (FLIGHT_DURATION + holdSeconds), 0.9, 1],
      }}
      onAnimationComplete={isLast ? onComplete : undefined}
      style={{
        position: "fixed",
        left: 0,
        top: 0,
        width: CARD_WIDTH,
        height: CARD_HEIGHT,
        pointerEvents: "none",
        zIndex: 46,
        borderRadius: 7,
        overflow: "hidden",
        boxShadow: `0 0 14px ${glowColor}88, 0 4px 18px rgba(0,0,0,0.55)`,
      }}
    >
      {card.snapshot ? (
        <ResolvedAnimationImage
          snapshot={card.snapshot}
          size="normal"
          alt={card.snapshot.cardName}
          fallback={(
            <div
              style={{
                width: "100%",
                height: "100%",
                backgroundColor: "rgba(0,0,0,0.75)",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                color: "white",
                fontSize: "0.62rem",
                textAlign: "center",
                padding: 4,
              }}
            >
              {card.snapshot.cardName}
            </div>
          )}
          style={{ width: "100%", height: "100%", objectFit: "cover" }}
        />
      ) : (
        <div
          style={{
            width: "100%",
            height: "100%",
            backgroundColor: "rgba(0,0,0,0.75)",
          }}
        />
      )}
    </motion.div>
  );
}

/**
 * CR 702.60a + CR 701.20b: Ripple reveals the top N cards of the revealing
 * player's library WITHOUT moving them. This overlay fans those cards out from
 * the library pile, holds them face-up for every player to read, then fades —
 * the transient counterpart to `RevealOverlay`'s persistent strip (which only
 * renders while the same-named free-cast offer is open).
 *
 * Modeled on `MillRevealAnimation` but ends in a centered hold + fade rather
 * than a flight into the graveyard.
 */
export function RippleRevealAnimation({
  cards,
  from,
  onComplete,
}: RippleRevealAnimationProps) {
  const completedRef = useRef(false);
  const displayedCards = cards;

  useEffect(() => {
    const expectedMs =
      (displayedCards.length - 1) * STAGGER_MS + FLIGHT_DURATION * 1000 + HOLD_MS + 500;
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

  const spanWidth =
    displayedCards.length * CARD_WIDTH + (displayedCards.length - 1) * CARD_GAP;
  const centerX = typeof window !== "undefined" ? window.innerWidth / 2 : 640;
  const rowY = typeof window !== "undefined" ? window.innerHeight * 0.3 : 240;
  const startX = centerX - spanWidth / 2 + CARD_WIDTH / 2;

  return (
    <>
      {displayedCards.map((card, i) => (
        <RippleCardElement
          key={`ripple-${card.objectId}`}
          card={card}
          from={from}
          target={{ x: startX + i * (CARD_WIDTH + CARD_GAP), y: rowY }}
          index={i}
          isLast={i === displayedCards.length - 1}
          onComplete={handleComplete}
        />
      ))}
    </>
  );
}
