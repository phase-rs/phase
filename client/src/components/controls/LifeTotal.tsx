import { motion, useMotionValue, useTransform, animate } from "framer-motion";
import { useEffect, useRef, useState } from "react";

import { CARD_SLAM_FLIGHT_MS } from "../../animation/types.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

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

  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const impactTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Animate life total in sync with damage/heal visuals. When a DamageDealt
  // event co-occurs in the same step, delay the counter update to match the
  // card slam flight duration so the number ticks at impact.
  // Pre-updating prevLife suppresses the redundant re-animation from the deferred
  // gameStore state update that follows once all animations complete.
  // Flash timer is managed via ref — returning a cleanup would cancel it when
  // activeStep advances to the next step, preventing the flash from ever clearing.
  useEffect(() => {
    if (!activeStep) return;
    for (const effect of activeStep.effects) {
      if (effect.event.type !== "LifeChanged") continue;
      const lifeEvent = effect.event;
      if (lifeEvent.data.player_id !== playerId) continue;

      const hasDamageDealt = activeStep.effects.some(
        (e) =>
          e.event.type === "DamageDealt" &&
          "Player" in e.event.data.target &&
          e.event.data.target.Player === playerId,
      );

      const newLife = prevLife.current + lifeEvent.data.amount;
      const doAnimate = () => {
        animate(motionLife, newLife, { duration: 0.3 });
        setFlashColor(lifeEvent.data.amount < 0 ? "red" : "green");
        if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
        flashTimerRef.current = setTimeout(() => setFlashColor(null), 400);
      };

      prevLife.current = newLife;

      if (hasDamageDealt) {
        impactTimerRef.current = setTimeout(doAnimate, CARD_SLAM_FLIGHT_MS * speedMultiplier);
      } else {
        doAnimate();
      }
      break;
    }

    return () => {
      if (impactTimerRef.current) {
        clearTimeout(impactTimerRef.current);
        impactTimerRef.current = null;
      }
    };
  }, [activeStep, playerId, motionLife, speedMultiplier]);

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
    <div className="flex items-baseline gap-2">
      {!hideLabel && <span className="text-xs text-slate-400">P{playerId + 1}</span>}
      <motion.span
        key={life}
        initial={{ scale: 1.3 }}
        animate={{ scale: 1 }}
        transition={{ duration: 0.2 }}
        className={`rounded-md px-1.5 py-0.5 font-bold tabular-nums transition-colors duration-400 ${size === "lg" ? "text-2xl lg:text-[2rem]" : "text-lg lg:text-[1.4rem]"} ${colorClass} ${flashBg}`}
      >
        <motion.span>{displayed}</motion.span>
      </motion.span>
    </div>
  );
}
