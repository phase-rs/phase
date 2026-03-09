import { motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage";

interface CastArcAnimationProps {
  from: { x: number; y: number };
  to: { x: number; y: number };
  cardName: string;
  mode: "cast" | "resolve-permanent" | "resolve-spell";
  onComplete: () => void;
}

const CARD_WIDTH = 80;
const CARD_HEIGHT = 112;
const ARC_HEIGHT = 100;

export function CastArcAnimation({
  from,
  to,
  cardName,
  mode,
  onComplete,
}: CastArcAnimationProps) {
  const { src } = useCardImage(cardName, { size: "small" });

  if (mode === "resolve-spell") {
    // Instant/sorcery: fade out with scale reduction at current position
    return (
      <motion.div
        initial={{ opacity: 1, scale: 1 }}
        animate={{ opacity: 0, scale: 0.3 }}
        transition={{ duration: 0.3, ease: "easeIn" }}
        onAnimationComplete={onComplete}
        style={{
          position: "fixed",
          left: from.x - CARD_WIDTH / 2,
          top: from.y - CARD_HEIGHT / 2,
          width: CARD_WIDTH,
          height: CARD_HEIGHT,
          pointerEvents: "none",
          zIndex: 45,
          borderRadius: 6,
          overflow: "hidden",
          boxShadow: "0 0 12px rgba(59, 130, 246, 0.5)",
        }}
      >
        {src && (
          <img
            src={src}
            alt={cardName}
            style={{ width: "100%", height: "100%", objectFit: "cover" }}
          />
        )}
        {!src && (
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
            {cardName}
          </div>
        )}
      </motion.div>
    );
  }

  // Cast (hand->stack) or resolve-permanent (stack->battlefield): parabolic arc
  const midX = (from.x + to.x) / 2;
  const midY = Math.min(from.y, to.y) - ARC_HEIGHT;
  const duration = mode === "cast" ? 0.4 : 0.3;

  return (
    <motion.div
      initial={{
        x: from.x - CARD_WIDTH / 2,
        y: from.y - CARD_HEIGHT / 2,
        scale: 1,
        opacity: 1,
      }}
      animate={{
        x: [from.x - CARD_WIDTH / 2, midX - CARD_WIDTH / 2, to.x - CARD_WIDTH / 2],
        y: [from.y - CARD_HEIGHT / 2, midY - CARD_HEIGHT / 2, to.y - CARD_HEIGHT / 2],
        scale: [1, 1.1, 1],
        opacity: 1,
      }}
      transition={{
        duration,
        ease: "easeInOut",
        times: [0, 0.5, 1],
      }}
      onAnimationComplete={onComplete}
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
      {src && (
        <img
          src={src}
          alt={cardName}
          style={{ width: "100%", height: "100%", objectFit: "cover" }}
        />
      )}
      {!src && (
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
          {cardName}
        </div>
      )}
      {/* Glow intensifies at destination */}
      <motion.div
        initial={{ boxShadow: "0 0 4px rgba(59, 130, 246, 0.2)" }}
        animate={{
          boxShadow: [
            "0 0 4px rgba(59, 130, 246, 0.2)",
            "0 0 16px rgba(59, 130, 246, 0.6)",
            "0 0 24px rgba(59, 130, 246, 0.8)",
          ],
        }}
        transition={{ duration, times: [0, 0.5, 1] }}
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
