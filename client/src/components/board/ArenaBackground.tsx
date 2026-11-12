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
    // `cancelled` guards the callback but cannot cancel an `import()` already in
    // flight, so a load that settles after teardown needs a rejection handler or
    // it becomes an unhandled rejection — fatal to the process even though the
    // painting below is still on screen. The scene is decorative, so swallowing
    // is the right answer for both real cases: a chunk fetch failing mid-deploy,
    // and a module graph resolving against an environment that is already gone.
    // Same shape as `BuildBadge.tsx`'s lazy Tauri import.
    void import("./arenaBackgroundScene.ts")
      .then(({ createArenaBackgroundScene }) => {
        if (!cancelled) dispose = createArenaBackgroundScene(canvas, color, quality, intensity, ambiance);
      })
      .catch(() => {});
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
