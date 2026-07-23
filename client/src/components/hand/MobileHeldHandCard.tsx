import { useLayoutEffect } from "react";
import { createPortal } from "react-dom";
import {
  motion,
  useMotionValue,
  useReducedMotion,
  useSpring,
  useTransform,
  useVelocity,
} from "framer-motion";

import type { GameObject } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { MobileHandGesture } from "../../stores/uiStore.ts";
import { spellCostDisplay } from "../../viewmodel/costLabel.ts";
import { CardImage } from "../card/CardImage.tsx";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";

interface MobileHeldHandCardProps {
  gesture: MobileHandGesture | null;
  object: GameObject | null;
}

/**
 * Hand-sized drag visual for the mobile hold gesture.
 *
 * Portaled to the document so the hand container's `perspective` cannot turn a
 * fixed-position child into a container-relative element. The real HandCard
 * remains keyed in the fan but collapsed until the gesture ends.
 */
export function MobileHeldHandCard({ gesture, object }: MobileHeldHandCardProps) {
  const effectiveCost = useGameStore((s) =>
    object ? s.spellCosts[String(object.id)] : undefined,
  );
  const shouldReduceMotion = useReducedMotion();
  const animationSpeedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const dragOffsetX = useMotionValue(gesture?.offsetX ?? 0);
  const dragOffsetY = useMotionValue(gesture?.offsetY ?? 0);
  const dragVelocityX = useVelocity(dragOffsetX);
  const dragVelocityY = useVelocity(dragOffsetY);
  const rotateTarget = useTransform(
    dragVelocityX,
    [-900, 0, 900],
    [-5.5, 0, 5.5],
    { clamp: true },
  );
  const rotateXTarget = useTransform(
    dragVelocityY,
    [-900, 0, 900],
    [3.5, 0, -3.5],
    { clamp: true },
  );
  const rotate = useSpring(rotateTarget, {
    damping: 18,
    mass: 0.45,
    stiffness: 260,
  });
  const rotateX = useSpring(rotateXTarget, {
    damping: 20,
    mass: 0.45,
    stiffness: 280,
  });
  const dynamicMotionEnabled =
    animationSpeedMultiplier > 0 && !shouldReduceMotion;

  useLayoutEffect(() => {
    if (gesture?.phase !== "drag") return;
    // Position remains a direct mapping from the live pointer. These motion
    // values feed only the visual tilt, so the spring can never add input lag.
    dragOffsetX.set(gesture.offsetX);
    dragOffsetY.set(gesture.offsetY);
  }, [dragOffsetX, dragOffsetY, gesture?.offsetX, gesture?.offsetY, gesture?.phase]);

  if (
    typeof document === "undefined"
    || gesture?.phase !== "drag"
    || object == null
    || gesture.objectId !== object.id
  ) {
    return null;
  }

  const { displayCost, isReduced } = spellCostDisplay(effectiveCost, object.mana_cost);
  const { sourceOrigin } = gesture;
  const highlightClass = gesture.castReady
    ? "ring-2 ring-amber-300 shadow-[0_0_22px_6px_rgba(251,191,36,0.72)]"
    : gesture.playable
      ? "ring-2 ring-cyan-400 shadow-[0_0_16px_4px_rgba(34,211,238,0.6)]"
      : "";

  return createPortal(
    <motion.div
      className={`pointer-events-none fixed z-[180] overflow-visible rounded-lg drop-shadow-[0_16px_22px_rgba(0,0,0,0.58)] ${highlightClass}`}
      style={{
        height: sourceOrigin.height,
        left: sourceOrigin.centerX - sourceOrigin.width / 2 + gesture.offsetX,
        top: sourceOrigin.top + gesture.offsetY,
        rotate: dynamicMotionEnabled ? rotate : 0,
        rotateX: dynamicMotionEnabled ? rotateX : 0,
        transformOrigin: "50% 100%",
        transformPerspective: 700,
        width: sourceOrigin.width,
      }}
      initial={
        animationSpeedMultiplier > 0
          ? {
              opacity: 0,
              scale: 1,
            }
          : false
      }
      animate={{ opacity: 1, scale: 1 }}
      transition={{
        duration: (shouldReduceMotion ? 0.06 : 0.12) * animationSpeedMultiplier,
        ease: [0.22, 1, 0.36, 1],
      }}
      data-mobile-held-card
      data-mobile-held-card-motion={dynamicMotionEnabled ? "velocity" : undefined}
      data-mobile-held-card-state={gesture.castReady ? "cast-ready" : gesture.playable ? "playable" : "held"}
    >
      <CardImage
        cardName={object.name}
        size="normal"
        oracleId={object.printed_ref?.oracle_id}
        faceName={object.printed_ref?.face_name}
        unimplementedMechanics={object.unimplemented_mechanics}
        className="!h-full !w-full"
      />
      <div className="pointer-events-none absolute inset-0 @container">
        <ManaCostPips
          cost={displayCost}
          isReduced={isReduced}
          size="fluid"
          className="absolute right-[4%] top-[2%]"
        />
      </div>
    </motion.div>,
    document.body,
  );
}
