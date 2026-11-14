import { motion } from "framer-motion";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { lifeChangeImpactDelayMs } from "../../animation/types.ts";
import {
  formatLifeReadout,
  lifeReadoutTitle,
  lifeTierClass,
  useDisplayedLife,
} from "../../hooks/useDisplayedLife.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { damageCauseState } from "../animation/cardVfx/cardVfxSpecs.ts";

interface LifeTotalProps {
  playerId: number;
  size?: "sm" | "default" | "lg";
  hideLabel?: boolean;
}

export function LifeTotal({ playerId, size = "default", hideLabel = false }: LifeTotalProps) {
  const { t } = useTranslation("game");
  const readout = useDisplayedLife(playerId);
  const life = formatLifeReadout(readout);
  // Engine-derived: this seat has no life total (the Horde). Never inferred from
  // format/archenemy on the client — the engine publishes the fact.
  const hasNoLifeTotal = useGameStore(
    (s) => s.gameState?.players[playerId]?.has_no_life_total ?? false,
  );
  const libraryCount = useGameStore(
    (s) => s.gameState?.players[playerId]?.library?.length ?? 0,
  );
  const activeStep = useAnimationStore((s) => s.activeStep);
  const [flashColor, setFlashColor] = useState<"red" | "green" | null>(null);
  const flashTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const impactTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Engine-supplied totals are the sole authority for the displayed number —
  // the committed snapshot, or, while its animation is still playing, the
  // `LifeChanged.new_total` of a hit that has already landed (`useDisplayedLife`).
  // Events only provide presentation feedback here (flash timing/color); an
  // amount must never be accumulated into a second client-side life total.
  // Flash timer is managed via ref — returning it from this effect would cancel
  // the flash when activeStep advances to the next step.
  useEffect(() => {
    if (!activeStep) return;
    for (const effect of activeStep.effects) {
      if (effect.event.type !== "LifeChanged") continue;
      const lifeEvent = effect.event;
      if (lifeEvent.data.player_id !== playerId) continue;

      const flashLifeChange = () => {
        setFlashColor(lifeEvent.data.amount < 0 ? "red" : "green");
        if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
        flashTimerRef.current = setTimeout(() => setFlashColor(null), 400);
      };

      // Same delay the overlay records the new total on, so the flash and the
      // number it is flashing about land together.
      const impactDelay = lifeChangeImpactDelayMs(effect, activeStep.effects, playerId, damageCauseState());
      if (impactDelay > 0) {
        impactTimerRef.current = setTimeout(flashLifeChange, impactDelay * speedMultiplier);
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

  const colorClass = lifeTierClass(readout);
  const title = lifeReadoutTitle(t, readout);

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

  // A seat with no life total (the Horde) has no life worth showing: damage and
  // life loss are redirected into milling, and it is defeated when its library
  // runs out. Render that library count — its real clock — so the player gets
  // feedback that their attacks are landing, instead of a life total frozen at
  // its starting value. Placed after every hook so the rules of hooks hold.
  if (hasNoLifeTotal) {
    // Shrinking library = closer to defeat, so the scale runs the same direction
    // as life: healthy while deep, alarming when nearly decked.
    const libraryColor =
      libraryCount >= 60
        ? "text-green-400"
        : libraryCount >= 25
          ? "text-yellow-400"
          : "text-red-400";
    return (
      <div
        className={`flex items-baseline ${size === "sm" ? "gap-1" : "gap-2"}`}
        title={t("lifeTotal.libraryTooltip")}
      >
        {!hideLabel && (
          <span className="text-xs text-slate-400">{t("lifeTotal.libraryLabel")}</span>
        )}
        <motion.span
          key={libraryCount}
          initial={{ scale: 1.3 }}
          animate={{ scale: 1 }}
          transition={{ duration: 0.2 }}
          className={`rounded-md px-1 py-0.5 font-bold tabular-nums transition-colors duration-400 ${size === "sm" ? "" : "lg:px-1.5"} ${sizeClass} ${libraryColor}`}
        >
          {libraryCount}
        </motion.span>
      </div>
    );
  }

  return (
    <div className={`flex items-baseline ${size === "sm" ? "gap-1" : "gap-2"}`}>
      {!hideLabel && <span className="text-xs text-slate-400">{t("lifeTotal.playerLabel", { seat: playerId + 1 })}</span>}
      <motion.span
        key={life}
        initial={{ scale: 1.3 }}
        animate={{ scale: 1 }}
        transition={{ duration: 0.2 }}
        title={title}
        aria-label={title}
        className={`rounded-md px-1 py-0.5 font-bold tabular-nums transition-colors duration-400 ${size === "sm" ? "" : "lg:px-1.5"} ${sizeClass} ${colorClass} ${flashBg}`}
      >
        {life}
      </motion.span>
    </div>
  );
}
