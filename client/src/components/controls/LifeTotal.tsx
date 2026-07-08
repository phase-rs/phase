import { motion, useMotionValue, useTransform, animate } from "framer-motion";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  impactDelayMsForAnimationEvent,
  isPlayerDamageAnimationEvent,
} from "../../animation/types.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

interface LifeTotalProps {
  playerId: number;
  size?: "sm" | "default" | "lg";
  hideLabel?: boolean;
}

export function LifeTotal({ playerId, size = "default", hideLabel = false }: LifeTotalProps) {
  const { t } = useTranslation("game");
  const life = useGameStore(
    (s) => s.gameState?.players[playerId]?.life ?? 20,
  );
  const activeStep = useAnimationStore((s) => s.activeStep);
  // `prevLife` is the event-accumulation base for the step-driven animation
  // (newLife = prevLife + amount). `animatedTo` tracks the value `motionLife`
  // was actually animated toward — updated only when an animation truly runs,
  // never pre-emptively. The authoritative reconcile (Effect 2) gates on
  // `animatedTo`, so if a deferred animation is cancelled (e.g. the queue
  // advances/clears before the impact timer fires) the settled gameStore life
  // still reconciles the displayed number instead of being silently skipped.
  const prevLife = useRef(life);
  const animatedTo = useRef(life);
  const motionLife = useMotionValue(life);
  const displayed = useTransform(motionLife, (v) => Math.round(v));
  const [flashColor, setFlashColor] = useState<"red" | "green" | null>(null);
  const flashTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const impactTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Animate life total in sync with damage/heal visuals. When a DamageDealt
  // event co-occurs in the same step, delay the counter update to match the
  // card slam flight duration so the number ticks at impact.
  // When the animation runs it records `animatedTo`, which suppresses the
  // redundant re-animation from the deferred gameStore state update that
  // follows once all animations complete (Effect 2 sees `animatedTo === life`).
  // Flash timer is managed via ref — returning a cleanup would cancel it when
  // activeStep advances to the next step, preventing the flash from ever clearing.
  useEffect(() => {
    if (!activeStep) return;
    for (const effect of activeStep.effects) {
      if (effect.event.type !== "LifeChanged") continue;
      const lifeEvent = effect.event;
      if (lifeEvent.data.player_id !== playerId) continue;

      const playerDamageEvent = activeStep.effects.find(
        (e) => isPlayerDamageAnimationEvent(e.event, playerId),
      );
      const groupedDamageEvent = effect.displayOnly
        ? activeStep.effects.find((e) => e.event.type === "GroupedDamageFlurry")
        : undefined;
      const impactEvent = playerDamageEvent?.event ?? groupedDamageEvent?.event;

      const newLife = prevLife.current + lifeEvent.data.amount;
      const doAnimate = () => {
        animate(motionLife, newLife, { duration: 0.3 });
        // Issue #1560: advance `prevLife` only when the animation actually
        // commits, not when it is merely scheduled. The damage-dealt branch
        // below DEFERS `doAnimate` via a timer that the cleanup cancels when
        // `activeStep` advances; if `prevLife` were pre-advanced, the authoritative
        // `life` arriving in the store would equal `prevLife.current`, so the
        // fallback effect's guard (`prevLife.current !== life`) would suppress the
        // corrective animation and the displayed total would freeze at the old value.
        prevLife.current = newLife;
        // Record the value actually animated toward only when the animation
        // runs, so a cancelled deferred animation leaves `animatedTo` stale and
        // Effect 2 reconciles it.
        animatedTo.current = newLife;
        setFlashColor(lifeEvent.data.amount < 0 ? "red" : "green");
        if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
        flashTimerRef.current = setTimeout(() => setFlashColor(null), 400);
      };

      if (impactEvent) {
        impactTimerRef.current = setTimeout(
          doAnimate,
          impactDelayMsForAnimationEvent(impactEvent) * speedMultiplier,
        );
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

  // Authoritative reconcile: `gameStore.life` is the source of truth for the
  // settled value. Whenever it changes, if the displayed value (`animatedTo`)
  // doesn't already match it — because no step handled it (instant speed) OR a
  // step-driven animation was skipped/cancelled before completing (issue #1560)
  // — animate to the real life total. Gating on `animatedTo`, not `prevLife`,
  // is what guarantees the display can never get stuck behind the real life.
  useEffect(() => {
    if (animatedTo.current !== life) {
      animate(motionLife, life, { duration: 0.3 });

      if (life < animatedTo.current) {
        setFlashColor("red");
      } else {
        setFlashColor("green");
      }

      const timer = setTimeout(() => setFlashColor(null), 400);
      animatedTo.current = life;
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
  const sizeClass = size === "lg"
    ? "text-lg lg:text-2xl"
    : size === "sm"
      ? "text-sm lg:text-base"
      : "text-base lg:text-lg";

  return (
    <div className={`flex items-baseline ${size === "sm" ? "gap-1" : "gap-2"}`}>
      {!hideLabel && <span className="text-xs text-slate-400">{t("lifeTotal.playerLabel", { seat: playerId + 1 })}</span>}
      <motion.span
        key={life}
        initial={{ scale: 1.3 }}
        animate={{ scale: 1 }}
        transition={{ duration: 0.2 }}
        className={`rounded-md px-1 py-0.5 font-bold tabular-nums transition-colors duration-400 ${size === "sm" ? "" : "lg:px-1.5"} ${sizeClass} ${colorClass} ${flashBg}`}
      >
        <motion.span>{displayed}</motion.span>
      </motion.span>
    </div>
  );
}
