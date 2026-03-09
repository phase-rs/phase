import { motion } from "framer-motion";

interface FloatingNumberProps {
  value: number;
  position: { x: number; y: number };
  color: string;
  onComplete: () => void;
  speedMultiplier?: number;
}

export function FloatingNumber({
  value,
  position,
  color,
  onComplete,
  speedMultiplier = 1.0,
}: FloatingNumberProps) {
  return (
    <motion.div
      initial={{ opacity: 1, y: 0, scale: 1.2 }}
      animate={{ opacity: 0, y: -60, scale: 1.0 }}
      transition={{ duration: 0.8 * speedMultiplier }}
      onAnimationComplete={onComplete}
      style={{
        position: "fixed",
        left: position.x,
        top: position.y,
        pointerEvents: "none",
        color,
        fontSize: "1.5rem",
        fontWeight: "bold",
        textShadow: "0 1px 4px rgba(0,0,0,0.8)",
        zIndex: 60,
      }}
    >
      {value > 0 ? `+${value}` : value}
    </motion.div>
  );
}
