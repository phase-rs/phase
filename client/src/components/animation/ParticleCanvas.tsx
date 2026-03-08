import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef } from "react";

interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  alpha: number;
  color: string;
  decay: number;
}

export interface ParticleCanvasHandle {
  emitBurst: (x: number, y: number, color: string, count: number) => void;
  emitTrail: (
    from: { x: number; y: number },
    to: { x: number; y: number },
    color: string,
  ) => void;
}

export const ParticleCanvas = forwardRef<ParticleCanvasHandle>(
  function ParticleCanvas(_props, ref) {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const particlesRef = useRef<Particle[]>([]);
    const rafRef = useRef<number>(0);

    const emitBurst = useCallback(
      (x: number, y: number, color: string, count: number) => {
        for (let i = 0; i < count; i++) {
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
        const dx = to.x - from.x;
        const dy = to.y - from.y;
        const steps = 12;
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
        ctx.clearRect(0, 0, canvas.width, canvas.height);
        const alive: Particle[] = [];

        for (const p of particlesRef.current) {
          p.x += p.vx;
          p.y += p.vy;
          p.alpha -= p.decay;

          if (p.alpha <= 0) continue;

          ctx.globalAlpha = p.alpha;
          ctx.fillStyle = p.color;
          ctx.beginPath();
          ctx.arc(p.x, p.y, 3, 0, Math.PI * 2);
          ctx.fill();
          alive.push(p);
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
