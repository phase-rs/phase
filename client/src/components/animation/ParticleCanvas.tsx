import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef } from "react";

import type { RGB } from "./particleSystem";
import { ParticleSystem } from "./particleSystem";
import {
  emitExplosion,
  emitProjectile,
  emitSpellImpact,
  emitDamageFlash,
  emitPlayerDamage,
  emitHealEffect,
  emitSummonBurst,
  emitBlockClash,
  emitAttackBurst,
} from "./particleEffects";

export interface ParticleCanvasHandle {
  explosion: (x: number, y: number, color?: RGB) => void;
  projectile: (fromX: number, fromY: number, toX: number, toY: number, durationMs: number, color?: RGB) => void;
  spellImpact: (x: number, y: number, color?: RGB) => void;
  damageFlash: (x: number, y: number, amount: number) => void;
  playerDamage: (x: number, y: number, amount: number) => void;
  healEffect: (x: number, y: number, amount: number) => void;
  summonBurst: (x: number, y: number, color?: RGB) => void;
  blockClash: (x: number, y: number) => void;
  attackBurst: (x: number, y: number, color?: RGB) => void;
}

export const ParticleCanvas = forwardRef<ParticleCanvasHandle>(
  function ParticleCanvas(_props, ref) {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const systemRef = useRef<ParticleSystem | null>(null);

    useEffect(() => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const system = new ParticleSystem();
      systemRef.current = system;
      system.attach(canvas);

      const handleResize = () => system.resize();
      window.addEventListener("resize", handleResize);

      return () => {
        window.removeEventListener("resize", handleResize);
        system.detach();
        systemRef.current = null;
      };
    }, []);

    const getSystem = useCallback(() => systemRef.current, []);

    useImperativeHandle(
      ref,
      () => ({
        explosion(x, y, color) {
          const s = getSystem();
          if (s) emitExplosion(s, x, y, color);
        },
        projectile(fromX, fromY, toX, toY, durationMs, color) {
          const s = getSystem();
          if (s) emitProjectile(s, fromX, fromY, toX, toY, durationMs, color);
        },
        spellImpact(x, y, color) {
          const s = getSystem();
          if (s) emitSpellImpact(s, x, y, color);
        },
        damageFlash(x, y, amount) {
          const s = getSystem();
          if (s) emitDamageFlash(s, x, y, amount);
        },
        playerDamage(x, y, amount) {
          const s = getSystem();
          if (s) emitPlayerDamage(s, x, y, amount);
        },
        healEffect(x, y, amount) {
          const s = getSystem();
          if (s) emitHealEffect(s, x, y, amount);
        },
        summonBurst(x, y, color) {
          const s = getSystem();
          if (s) emitSummonBurst(s, x, y, color);
        },
        blockClash(x, y) {
          const s = getSystem();
          if (s) emitBlockClash(s, x, y);
        },
        attackBurst(x, y, color) {
          const s = getSystem();
          if (s) emitAttackBurst(s, x, y, color);
        },
      }),
      [getSystem],
    );

    return (
      <canvas
        ref={canvasRef}
        style={{
          position: "fixed",
          inset: 0,
          width: "100%",
          height: "100%",
          pointerEvents: "none",
          zIndex: 55,
        }}
      />
    );
  },
);
