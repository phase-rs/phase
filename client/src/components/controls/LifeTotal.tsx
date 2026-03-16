import { motion, useMotionValue, useTransform, animate } from "framer-motion";
import { useEffect, useRef, useState } from "react";

import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface LifeTotalProps {
  playerId: number;
  size?: "default" | "lg";
  hideLabel?: boolean;
}

export function LifeTotal({ playerId, size = "default", hideLabel = false }: LifeTotalProps) {
  const life = useGameStore(
    (s) => s.gameState?.players[playerId]?.life ?? 20,
  );
  const activeStep = useAnimationStore((s) => s.activeStep);
  const prevLife = useRef(life);
  const motionLife = useMotionValue(life);
  const displayed = useTransform(motionLife, (v) => Math.round(v));
  const [flashColor, setFlashColor] = useState<"red" | "green" | null>(null);
  const flashTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Animate life total immediately when the animation step fires, so the counter
  // updates in sync with the damage/heal visual rather than after all animations.
  // Pre-updating prevLife suppresses the redundant re-animation from the deferred
  // gameStore state update that follows once all animations complete.
  // Flash timer is managed via ref — returning a cleanup would cancel it when
  // activeStep advances to the next step, preventing the flash from ever clearing.
  useEffect(() => {
    if (!activeStep) return;
    for (const effect of activeStep.effects) {
      if (effect.event.type !== "LifeChanged") continue;
      if (effect.event.data.player_id !== playerId) continue;
      const newLife = prevLife.current + effect.event.data.amount;
      animate(motionLife, newLife, { duration: 0.3 });
      setFlashColor(effect.event.data.amount < 0 ? "red" : "green");
      if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
      flashTimerRef.current = setTimeout(() => setFlashColor(null), 400);
      prevLife.current = newLife;
      return;
    }
  }, [activeStep, playerId, motionLife]);

  // Fallback: animate from gameStore update when no animation step handled it
  // (e.g. instant speed, or life changes that arrive without a preceding step).
  useEffect(() => {
    if (prevLife.current !== life) {
      animate(motionLife, life, { duration: 0.3 });

      if (life < prevLife.current) {
        setFlashColor("red");
      } else {
        setFlashColor("green");
      }

      const timer = setTimeout(() => setFlashColor(null), 400);
      prevLife.current = life;
      return () => clearTimeout(timer);
    }
  }, [life, motionLife]);

  const colorClass =
    life >= 10
      ? "text-green-400"
      : life >= 5
        ? "text-yellow-400"
        : "text-red-400";

  const flashBg =
    flashColor === "red"
      ? "bg-red-500/30"
      : flashColor === "green"
        ? "bg-green-500/30"
        : "bg-transparent";

  return (
    <div className="flex items-center gap-2">
      {!hideLabel && <span className="text-xs text-gray-400">P{playerId + 1}</span>}
      <motion.span
        key={life}
        initial={{ scale: 1.3 }}
        animate={{ scale: 1 }}
        transition={{ duration: 0.2 }}
        className={`rounded px-1 font-bold tabular-nums transition-colors duration-400 ${size === "lg" ? "text-2xl" : "text-xl"} ${colorClass} ${flashBg}`}
      >
        <motion.span>{displayed}</motion.span>
      </motion.span>
    </div>
  );
}
