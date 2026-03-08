import { motion } from "framer-motion";

interface FloatingNumberProps {
  value: number;
  position: { x: number; y: number };
  color: string;
  onComplete: () => void;
}

export function FloatingNumber({
  value,
  position,
  color,
  onComplete,
}: FloatingNumberProps) {
  return (
    <motion.div
      initial={{ opacity: 1, y: 0 }}
      animate={{ opacity: 0, y: -60 }}
      transition={{ duration: 0.8 }}
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
