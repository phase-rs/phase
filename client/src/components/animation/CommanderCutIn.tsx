import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { AnimationStep } from "../../animation/types.ts";
import { getCardColors } from "../../animation/wubrgColors.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import {
  COMMANDER_CUT_IN_DURATION_MS,
  commanderCutInsForStep,
  type CommanderCutInPayload,
} from "./commanderCutInModel.ts";

interface QueuedCommanderCutIn extends CommanderCutInPayload {
  sequence: number;
}

/**
 * Watches the existing spatial animation queue and serializes commander
 * cinematics independently. The game keeps advancing underneath this
 * pointer-transparent presentation layer; it never delays engine state.
 */
export function CommanderCutInHost() {
  const activeStep = useAnimationStore((state) => state.activeStep);
  const animationNewState = useAnimationStore((state) => state.animationNewState);
  const gameState = useGameStore((state) => state.gameState);
  const gameSessionGeneration = useGameStore((state) => state.gameSessionGeneration);
  const speedMultiplier = usePreferencesStore((state) => state.animationSpeedMultiplier);
  const [queue, setQueue] = useState<QueuedCommanderCutIn[]>([]);
  const processedStepRef = useRef<AnimationStep | null>(null);
  const nextSequenceRef = useRef(0);

  useEffect(() => {
    processedStepRef.current = null;
    setQueue([]);
  }, [gameSessionGeneration]);

  useEffect(() => {
    if (!activeStep || processedStepRef.current === activeStep) return;
    processedStepRef.current = activeStep;

    const payloads = commanderCutInsForStep(activeStep, gameState, animationNewState);
    if (payloads.length === 0) return;

    setQueue((current) => [
      ...current,
      ...payloads.map((payload) => ({
        ...payload,
        sequence: ++nextSequenceRef.current,
      })),
    ]);
  }, [activeStep, animationNewState, gameState]);

  const active = queue[0] ?? null;

  useEffect(() => {
    if (!active) return;
    const timeout = setTimeout(
      () => setQueue((current) => current.slice(1)),
      COMMANDER_CUT_IN_DURATION_MS * Math.max(0, speedMultiplier),
    );
    return () => clearTimeout(timeout);
  }, [active, speedMultiplier]);

  return (
    <AnimatePresence mode="wait">
      {active && (
        <CommanderCutIn
          key={active.sequence}
          payload={active}
          speedMultiplier={speedMultiplier}
        />
      )}
    </AnimatePresence>
  );
}

function CommanderCutIn({
  payload,
  speedMultiplier,
}: {
  payload: CommanderCutInPayload;
  speedMultiplier: number;
}) {
  const { t } = useTranslation("game");
  const shouldReduceMotion = useReducedMotion();
  const { src } = useCardImage(payload.name, {
    size: "art_crop",
    oracleId: payload.oracleId,
    faceName: payload.faceName,
  });
  const palette = useMemo(() => getCardColors(payload.colors), [payload.colors]);
  const primary = palette[0] ?? "#94a3b8";
  const secondary = palette[1] ?? primary;
  const motionScale = Math.max(0, speedMultiplier);
  const enterDuration = shouldReduceMotion ? 0.14 : 0.38 * motionScale;
  const exitDuration = shouldReduceMotion ? 0.12 : 0.3 * motionScale;
  const nameDelay = shouldReduceMotion ? 0 : 0.22 * motionScale;

  return (
    <motion.div
      className="pointer-events-none fixed inset-0 z-[48] isolate overflow-hidden"
      data-testid="commander-cut-in"
      role="status"
      aria-live="polite"
      aria-label={t("zone.commanderTitle", { name: payload.name })}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: exitDuration }}
    >
      <motion.div
        className="absolute inset-0 bg-black/70"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: shouldReduceMotion ? 0.12 : 0.22 * motionScale }}
      />

      <motion.div
        className="absolute inset-x-[-8vw] top-[12dvh] h-[min(68dvh,34rem)] overflow-hidden border-y border-white/25 shadow-[0_0_80px_rgba(0,0,0,0.9)]"
        style={{
          clipPath: "polygon(0 17%, 100% 0, 100% 83%, 0 100%)",
          background: `linear-gradient(112deg, #030712 4%, ${primary}55 47%, ${secondary}88 100%)`,
        }}
        initial={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, x: "-105%" }}
        animate={{ opacity: 1, x: 0 }}
        exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, x: "105%" }}
        transition={{ duration: enterDuration, ease: [0.16, 1, 0.3, 1] }}
      >
        {src && (
          <>
            <img
              src={src}
              alt=""
              aria-hidden="true"
              className="absolute inset-0 h-full w-full scale-110 object-cover opacity-35 blur-xl saturate-150"
            />
            <motion.img
              src={src}
              alt=""
              aria-hidden="true"
              className="absolute inset-y-[-8%] right-[-2%] h-[116%] w-[74%] object-cover saturate-125 contrast-110"
              style={{
                objectPosition: "center 30%",
                maskImage:
                  "linear-gradient(90deg, transparent 0%, rgba(0,0,0,.25) 12%, black 34%, black 88%, transparent 100%)",
                WebkitMaskImage:
                  "linear-gradient(90deg, transparent 0%, rgba(0,0,0,.25) 12%, black 34%, black 88%, transparent 100%)",
              }}
              initial={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, x: "12%", scale: 1.08 }}
              animate={{ opacity: 1, x: 0, scale: 1 }}
              transition={{ duration: shouldReduceMotion ? 0.14 : 0.55 * motionScale, delay: nameDelay * 0.25 }}
            />
          </>
        )}

        <div
          className="absolute inset-0 opacity-60"
          style={{
            background: `linear-gradient(105deg, rgba(2,6,23,.98) 4%, rgba(2,6,23,.9) 27%, rgba(2,6,23,.18) 62%, transparent 82%), repeating-linear-gradient(116deg, transparent 0 56px, ${primary}20 57px 58px)`,
          }}
        />

        <div className="absolute inset-x-[12vw] bottom-[18%] flex flex-col items-start">
          <div className="max-w-[min(76vw,64rem)] overflow-visible py-8">
            <motion.h2
              className="font-display text-[clamp(2rem,6.4vw,5.75rem)] font-black leading-[.88] tracking-[-.045em] text-white"
              style={{
                textShadow: `0 3px 0 rgba(0,0,0,.75), 0 0 28px ${primary}99`,
              }}
              initial={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, x: "-108%" }}
              animate={{ opacity: 1, x: 0 }}
              transition={{ duration: shouldReduceMotion ? 0.14 : 0.46 * motionScale, delay: nameDelay, ease: [0.16, 1, 0.3, 1] }}
            >
              {payload.name}
            </motion.h2>
          </div>

          <motion.div
            className="h-[3px] max-w-[42rem]"
            style={{
              width: "min(64vw, 42rem)",
              originX: 0,
              background: `linear-gradient(90deg, ${primary}, ${secondary}, transparent)`,
              boxShadow: `0 0 16px ${primary}`,
            }}
            initial={{ opacity: 0, scaleX: shouldReduceMotion ? 1 : 0 }}
            animate={{ opacity: 1, scaleX: 1 }}
            transition={{ duration: shouldReduceMotion ? 0.12 : 0.38 * motionScale, delay: nameDelay * 1.35 }}
          />
        </div>
      </motion.div>

      <motion.div
        className="absolute inset-y-0 w-[2px] bg-white/80"
        style={{ boxShadow: `0 0 28px 8px ${primary}` }}
        initial={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, left: "-4%" }}
        animate={shouldReduceMotion ? { opacity: 0.5 } : { opacity: [0, 1, 0], left: ["-4%", "78%", "104%"] }}
        transition={{ duration: shouldReduceMotion ? 0.12 : 0.72 * motionScale, delay: nameDelay * 0.35 }}
      />
    </motion.div>
  );
}
