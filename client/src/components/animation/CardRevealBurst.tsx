import type { RefObject } from "react";
import { useEffect } from "react";
import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore";
import type { ParticleCanvasHandle } from "./ParticleCanvas";

interface CardRevealBurstProps {
  position: { x: number; y: number };
  colors: string[];
  speedMultiplier: number;
  onComplete: () => void;
  particleRef: RefObject<ParticleCanvasHandle | null>;
}

const BURST_PARTICLE_COUNT = 20;

export function CardRevealBurst({
  position,
  colors,
  speedMultiplier,
  onComplete,
  particleRef,
}: CardRevealBurstProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);

  useEffect(() => {
    if (vfxQuality === "minimal" || !particleRef.current) return;

    const burstColor = colors[0] ?? "#94a3b8";
    particleRef.current.emitBurst(
      position.x,
      position.y,
      burstColor,
      BURST_PARTICLE_COUNT,
    );

    // Emit additional bursts for multicolor cards
    for (let i = 1; i < colors.length; i++) {
      particleRef.current.emitBurst(
        position.x,
        position.y,
        colors[i],
        Math.ceil(BURST_PARTICLE_COUNT / colors.length),
      );
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  if (vfxQuality === "minimal") {
    return null;
  }

  return (
    <motion.div
      initial={{ scale: 0.8, opacity: 0.8 }}
      animate={{ scale: 1.0, opacity: 0 }}
      transition={{ duration: 0.3 * speedMultiplier }}
      onAnimationComplete={onComplete}
      style={{
        position: "fixed",
        left: position.x,
        top: position.y,
        width: 0,
        height: 0,
        pointerEvents: "none",
        zIndex: 55,
      }}
    />
  );
}
