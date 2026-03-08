import { motion, useMotionValue, useTransform, animate } from "framer-motion";
import { useEffect, useRef } from "react";

import { useGameStore } from "../../stores/gameStore.ts";

interface LifeTotalProps {
  playerId: number;
}

export function LifeTotal({ playerId }: LifeTotalProps) {
  const life = useGameStore(
    (s) => s.gameState?.players[playerId]?.life ?? 20,
  );
  const prevLife = useRef(life);
  const motionLife = useMotionValue(life);
  const displayed = useTransform(motionLife, (v) => Math.round(v));

  useEffect(() => {
    if (prevLife.current !== life) {
      animate(motionLife, life, { duration: 0.3 });
      prevLife.current = life;
    }
  }, [life, motionLife]);

  const colorClass =
    life >= 10
      ? "text-green-400"
      : life >= 5
        ? "text-yellow-400"
        : "text-red-400";

  return (
    <div className="flex items-center gap-2">
      <span className="text-xs text-gray-400">P{playerId + 1}</span>
      <motion.span
        key={life}
        initial={{ scale: 1.3 }}
        animate={{ scale: 1 }}
        transition={{ duration: 0.2 }}
        className={`text-xl font-bold tabular-nums ${colorClass}`}
      >
        <motion.span>{displayed}</motion.span>
      </motion.span>
    </div>
  );
}
