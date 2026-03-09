import { AnimatePresence, motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore";

interface TurnBannerProps {
  turnNumber: number;
  isPlayerTurn: boolean;
  speedMultiplier: number;
  onComplete?: () => void;
}

export function TurnBanner({
  turnNumber,
  isPlayerTurn,
  speedMultiplier,
  onComplete,
}: TurnBannerProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);

  const text = `Turn ${turnNumber} — ${isPlayerTurn ? "Your Turn" : "Opponent's Turn"}`;

  const isMinimal = vfxQuality === "minimal";

  return (
    <AnimatePresence onExitComplete={onComplete}>
      <motion.div
        key={`turn-${turnNumber}`}
        initial={isMinimal ? { opacity: 0 } : { x: "-100%", opacity: 0 }}
        animate={isMinimal ? { opacity: 1 } : { x: "0%", opacity: 1 }}
        exit={isMinimal ? { opacity: 0 } : { x: "100%", opacity: 0 }}
        transition={{
          duration: 0.3 * speedMultiplier,
          exit: { duration: 0.3 * speedMultiplier, delay: 0.6 * speedMultiplier },
        }}
        style={{
          position: "fixed",
          top: "50%",
          left: 0,
          right: 0,
          transform: "translateY(-50%)",
          zIndex: 50,
          pointerEvents: "none",
          display: "flex",
          justifyContent: "center",
          alignItems: "center",
          padding: "1rem 0",
          backgroundColor: "rgba(0, 0, 0, 0.7)",
        }}
      >
        <span
          style={{
            color: "white",
            fontSize: "1.5rem",
            fontWeight: "bold",
            letterSpacing: "0.05em",
            textTransform: "uppercase",
          }}
        >
          {text}
        </span>
      </motion.div>
    </AnimatePresence>
  );
}
