import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef } from "react";

import type { VfxQuality } from "../../animation/types";
import { usePreferencesStore } from "../../stores/preferencesStore";

interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  alpha: number;
  color: string;
  decay: number;
  radius: number;
  gravity: number;
}

export interface ParticleCanvasHandle {
  emitBurst: (x: number, y: number, color: string, count: number) => void;
  emitTrail: (
    from: { x: number; y: number },
    to: { x: number; y: number },
    color: string,
  ) => void;
}

function getVfxQuality(): VfxQuality {
  return usePreferencesStore.getState().vfxQuality;
}

export const ParticleCanvas = forwardRef<ParticleCanvasHandle>(
  function ParticleCanvas(_props, ref) {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const particlesRef = useRef<Particle[]>([]);
    const rafRef = useRef<number>(0);

    const emitBurst = useCallback(
      (x: number, y: number, color: string, count: number) => {
        const quality = getVfxQuality();
        if (quality === "minimal") return;

        const effectiveCount = quality === "reduced" ? Math.ceil(count / 2) : count;
        for (let i = 0; i < effectiveCount; i++) {
          const angle = Math.random() * Math.PI * 2;
          const speed = 1 + Math.random() * 3;
          particlesRef.current.push({
            x,
            y,
            vx: Math.cos(angle) * speed,
            vy: Math.sin(angle) * speed,
            alpha: 1,
            color,
            decay: 0.015 + Math.random() * 0.01,
            radius: 2 + Math.random() * 4,
            gravity: 0.03 + Math.random() * 0.02,
          });
        }
      },
      [],
    );

    const emitTrail = useCallback(
      (
        from: { x: number; y: number },
        to: { x: number; y: number },
        color: string,
      ) => {
        const quality = getVfxQuality();
        if (quality === "minimal") return;

        const dx = to.x - from.x;
        const dy = to.y - from.y;
        const steps = quality === "reduced" ? 6 : 12;
        for (let i = 0; i < steps; i++) {
          const t = i / steps;
          particlesRef.current.push({
            x: from.x + dx * t,
            y: from.y + dy * t,
            vx: (Math.random() - 0.5) * 0.5,
            vy: (Math.random() - 0.5) * 0.5,
            alpha: 1,
            color,
            decay: 0.02 + Math.random() * 0.01,
            radius: 2 + Math.random() * 4,
            gravity: 0.03 + Math.random() * 0.02,
          });
        }
      },
      [],
    );

    useImperativeHandle(ref, () => ({ emitBurst, emitTrail }), [
      emitBurst,
      emitTrail,
    ]);

    useEffect(() => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      const resize = () => {
        canvas.width = window.innerWidth;
        canvas.height = window.innerHeight;
      };
      resize();
      window.addEventListener("resize", resize);

      const tick = () => {
        const quality = getVfxQuality();

        ctx.clearRect(0, 0, canvas.width, canvas.height);

        if (quality === "minimal") {
          particlesRef.current = [];
          rafRef.current = requestAnimationFrame(tick);
          return;
        }

        const enableGlow = quality === "full";
        if (enableGlow) {
          ctx.shadowBlur = 6;
        }

        const alive: Particle[] = [];

        for (const p of particlesRef.current) {
          p.vy += p.gravity;
          p.x += p.vx;
          p.y += p.vy;
          p.alpha -= p.decay;

          if (p.alpha <= 0) continue;

          ctx.globalAlpha = p.alpha;
          ctx.fillStyle = p.color;
          if (enableGlow) {
            ctx.shadowColor = p.color;
          }
          ctx.beginPath();
          ctx.arc(p.x, p.y, p.radius, 0, Math.PI * 2);
          ctx.fill();
          alive.push(p);
        }

        if (enableGlow) {
          ctx.shadowBlur = 0;
          ctx.shadowColor = "transparent";
        }
        ctx.globalAlpha = 1;
        particlesRef.current = alive;
        rafRef.current = requestAnimationFrame(tick);
      };

      rafRef.current = requestAnimationFrame(tick);

      return () => {
        window.removeEventListener("resize", resize);
        cancelAnimationFrame(rafRef.current);
      };
    }, []);

    return (
      <canvas
        ref={canvasRef}
        style={{
          position: "fixed",
          inset: 0,
          pointerEvents: "none",
          zIndex: 55,
        }}
      />
    );
  },
);
