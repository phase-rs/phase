import { motion } from "framer-motion";
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
  const [flashColor, setFlashColor] = useState<"red" | "green" | null>(null);
  const flashTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const impactTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // The snapshot is the sole authority for the displayed number. Events only
  // provide presentation feedback (flash timing/color); they must never be
  // accumulated into a second client-side life total.
  // Flash timer is managed via ref — returning it from this effect would cancel
  // the flash when activeStep advances to the next step.
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

      const flashLifeChange = () => {
        setFlashColor(lifeEvent.data.amount < 0 ? "red" : "green");
        if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
        flashTimerRef.current = setTimeout(() => setFlashColor(null), 400);
      };

      if (impactEvent) {
        impactTimerRef.current = setTimeout(
          flashLifeChange,
          impactDelayMsForAnimationEvent(impactEvent) * speedMultiplier,
        );
      } else {
        flashLifeChange();
      }
      break;
    }

    return () => {
      if (impactTimerRef.current) {
        clearTimeout(impactTimerRef.current);
        impactTimerRef.current = null;
      }
    };
  }, [activeStep, playerId, speedMultiplier]);

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
        {life}
      </motion.span>
    </div>
  );
}
