import { useEffect, useRef } from "react";
import { useReducedMotion } from "framer-motion";

import type { VfxQuality } from "../../animation/types.ts";

import type { ManaColor } from "../../adapter/types.ts";
import { ARENA_ART } from "./arenaBackgrounds.ts";

export interface ArenaBackgroundProps {
  color?: ManaColor;
  ambiance?: number;
  animated?: boolean;
  quality?: VfxQuality;
  intensity?: number;
}

/** The painting stays visible when animation is disabled or WebGL is unavailable. */
export function ArenaBackground({ color = "Blue", animated = true, quality = "full", intensity = 1, ambiance = 1 }: ArenaBackgroundProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const reducedMotion = useReducedMotion();

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !animated || reducedMotion || quality === "minimal") return;
    let cancelled = false;
    let dispose: (() => void) | undefined;
    void import("./arenaBackgroundScene.ts").then(({ createArenaBackgroundScene }) => {
      if (!cancelled) dispose = createArenaBackgroundScene(canvas, color, quality, intensity, ambiance);
    });
    return () => {
      cancelled = true;
      dispose?.();
    };
  }, [color, animated, reducedMotion, quality, intensity, ambiance]);

  return (
    <div aria-hidden="true" className="pointer-events-none absolute inset-0 overflow-hidden">
      <img src={ARENA_ART[color]} alt="" className="absolute inset-0 h-full w-full object-cover" />
      <canvas
        key={`${color}-${animated}-${quality}-${intensity}-${ambiance}-${reducedMotion}`}
        ref={canvasRef}
        className="absolute inset-0 h-full w-full"
        style={{ visibility: "hidden" }}
      />
    </div>
  );
}
