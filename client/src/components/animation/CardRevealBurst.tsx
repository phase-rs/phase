import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore";

interface CardRevealBurstProps {
  position: { x: number; y: number };
  colors: string[];
  speedMultiplier: number;
  onComplete: () => void;
}

export function CardRevealBurst({
  position,
  speedMultiplier,
  onComplete,
}: CardRevealBurstProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);

  if (vfxQuality === "minimal") {
    return null;
  }

  return (
    <motion.div
      initial={{ scale: 0.8, opacity: 0.8 }}
      animate={{ scale: 1.0, opacity: 0 }}
      transition={{ duration: 0.3 * speedMultiplier }}
      onAnimationComplete={onComplete}
      style={{
        position: "fixed",
        left: position.x,
        top: position.y,
        width: 0,
        height: 0,
        pointerEvents: "none",
        zIndex: 55,
      }}
    />
  );
}
