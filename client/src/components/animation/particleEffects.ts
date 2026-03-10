// ─── Particle Effect Presets ───
// Factory functions that produce visually rich VFX by emitting particles
// and creating timed effects on the ParticleSystem.
// Ported from Alchemy's particle engine, adapted for MTG context.

import type { ParticleSystem, RGB, Particle } from "./particleSystem";

// ─── MTG Colors ───

export const COMBAT_COLOR: RGB = { r: 255, g: 180, b: 60 };
export const DAMAGE_COLOR: RGB = { r: 255, g: 70, b: 50 };
export const HEAL_COLOR: RGB = { r: 80, g: 230, b: 160 };
export const WHITE: RGB = { r: 255, g: 255, b: 255 };
export const SPELL_COLOR: RGB = { r: 80, g: 170, b: 255 };
export const BLOCK_COLOR: RGB = { r: 100, g: 170, b: 255 };

// ─── Easing ───

export function easeOutCubic(t: number): number {
  return 1 - (1 - t) ** 3;
}

export function easeOutQuart(t: number): number {
  return 1 - (1 - t) ** 4;
}

// ─── Helpers ───

export function randRange(min: number, max: number): number {
  return min + Math.random() * (max - min);
}

export function lerpColor(a: RGB, b: RGB, t: number): RGB {
  return {
    r: a.r + (b.r - a.r) * t,
    g: a.g + (b.g - a.g) * t,
    b: a.b + (b.b - a.b) * t,
  };
}

export function hexToRgb(hex: string): RGB {
  const h = hex.replace("#", "");
  return {
    r: parseInt(h.substring(0, 2), 16),
    g: parseInt(h.substring(2, 4), 16),
    b: parseInt(h.substring(4, 6), 16),
  };
}

// ─── Reusable Burst Primitives ───

export interface BurstOpts {
  count: number;
  speedRange: [number, number];
  lifeRange: [number, number];
  sizeRange: [number, number];
  jitter?: number;
  drag?: number;
  glow?: number;
  alpha?: number;
}

/** Radial burst of colored particles from a center point. */
export function radialBurst(x: number, y: number, color: RGB, opts: BurstOpts): Partial<Particle>[] {
  const { count, speedRange, lifeRange, sizeRange, jitter = 0.3, drag = 2.5, glow = 14, alpha = 0.95 } = opts;
  const particles: Partial<Particle>[] = [];
  for (let i = 0; i < count; i++) {
    const angle = (i / count) * Math.PI * 2 + randRange(-jitter, jitter);
    const speed = randRange(speedRange[0], speedRange[1]);
    particles.push({
      x, y,
      vx: Math.cos(angle) * speed,
      vy: Math.sin(angle) * speed,
      life: randRange(lifeRange[0], lifeRange[1]),
      size: randRange(sizeRange[0], sizeRange[1]),
      endSize: 0,
      r: color.r, g: color.g, b: color.b,
      alpha, drag, glow,
    });
  }
  return particles;
}

/** White-hot core sparkles — small count, fast-fading. */
export function coreSparkles(x: number, y: number, count: number, speedRange: [number, number], opts?: { glow?: number; sizeRange?: [number, number] }): Partial<Particle>[] {
  const { glow = 18, sizeRange = [4, 10] } = opts ?? {};
  const particles: Partial<Particle>[] = [];
  for (let i = 0; i < count; i++) {
    const angle = Math.random() * Math.PI * 2;
    const speed = randRange(speedRange[0], speedRange[1]);
    particles.push({
      x, y,
      vx: Math.cos(angle) * speed,
      vy: Math.sin(angle) * speed,
      life: randRange(0.12, 0.35),
      size: randRange(sizeRange[0], sizeRange[1]),
      endSize: 0,
      r: 255, g: 255, b: 255,
      alpha: 0.9, drag: 3, glow,
    });
  }
  return particles;
}

/** Slow-falling ember debris with gravity. */
export function emberDebris(x: number, y: number, color: RGB, count: number): Partial<Particle>[] {
  const particles: Partial<Particle>[] = [];
  for (let i = 0; i < count; i++) {
    const angle = Math.random() * Math.PI * 2;
    const speed = randRange(30, 80);
    particles.push({
      x: x + randRange(-10, 10),
      y: y + randRange(-10, 10),
      vx: Math.cos(angle) * speed,
      vy: Math.sin(angle) * speed - randRange(20, 50),
      life: randRange(0.5, 1.0),
      size: randRange(1.5, 3),
      endSize: 0,
      r: color.r, g: color.g * 0.7, b: color.b * 0.3,
      alpha: 0.7, drag: 1, gravity: 80, glow: 6,
    });
  }
  return particles;
}

// ─── Effect: Explosion (creature death) ───

export function emitExplosion(system: ParticleSystem, x: number, y: number, color: RGB = COMBAT_COLOR) {
  const now = performance.now();

  system.emit([
    ...radialBurst(x, y, color, { count: 28, speedRange: [120, 300], lifeRange: [0.4, 0.75], sizeRange: [3, 8] }),
    ...coreSparkles(x, y, 10, [50, 130]),
    ...emberDebris(x, y, color, 8),
  ]);

  // Central flash + double shockwave ring
  system.addEffect({
    startTime: now,
    duration: 600,
    update() {},
    draw(t, ctx) {
      const flashAlpha = 1 - easeOutCubic(t);
      const flashRadius = 15 + easeOutCubic(t) * 70;
      system.drawGlowCircle(ctx, x, y, flashRadius, WHITE, flashAlpha * 0.8, 30);
      system.drawGlowCircle(ctx, x, y, flashRadius * 0.5, color, flashAlpha * 0.6, 20);

      // Inner shockwave ring
      const ringT = easeOutQuart(t);
      const ringRadius = 10 + ringT * 80;
      const ringAlpha = (1 - t) * 0.9;
      system.drawGlowRing(ctx, x, y, ringRadius, color, ringAlpha, 3 - t * 2.5, 14);

      // Outer shockwave ring — delayed, wider
      const r2t = Math.max(0, Math.min((t - 0.1) * 1.2, 1));
      if (r2t > 0) {
        const r2radius = 8 + easeOutQuart(r2t) * 100;
        const r2alpha = (1 - r2t) * 0.5;
        system.drawGlowRing(ctx, x, y, r2radius, lerpColor(color, WHITE, 0.3), r2alpha, 2 - r2t * 1.5, 10);
      }
    },
  });
}

// ─── Effect: Projectile (combat strike) ───

export function emitProjectile(
  system: ParticleSystem,
  fromX: number,
  fromY: number,
  toX: number,
  toY: number,
  durationMs: number,
  color: RGB = COMBAT_COLOR,
) {
  const now = performance.now();
  const dx = toX - fromX;
  const dy = toY - fromY;
  let lastTrailEmitT = -1;

  system.addEffect({
    startTime: now,
    duration: durationMs,
    update(t) {
      if (t - lastTrailEmitT > 0.025) {
        lastTrailEmitT = t;
        const progress = easeOutCubic(t);
        const px = fromX + dx * progress;
        const py = fromY + dy * progress;

        const trail: Partial<Particle>[] = [
          {
            x: px + randRange(-5, 5), y: py + randRange(-5, 5),
            vx: randRange(-35, 35), vy: randRange(-35, 35),
            life: randRange(0.15, 0.3), size: randRange(2.5, 5), endSize: 0,
            r: color.r, g: color.g, b: color.b, alpha: 0.8, drag: 3, glow: 10,
          },
          {
            x: px, y: py,
            vx: randRange(-12, 12), vy: randRange(-12, 12),
            life: 0.1, size: randRange(3, 6), endSize: 1,
            r: 255, g: 255, b: 240, alpha: 0.7, drag: 2, glow: 12,
          },
        ];
        if (Math.random() > 0.4) {
          trail.push({
            x: px + randRange(-3, 3), y: py + randRange(-3, 3),
            vx: randRange(-50, 50), vy: randRange(-50, 50),
            life: randRange(0.1, 0.2), size: randRange(1.5, 3), endSize: 0,
            r: 255, g: color.g * 0.8, b: color.b * 0.5, alpha: 0.6, drag: 5, glow: 6,
          });
        }
        system.emit(trail);
      }
    },
    draw(t, ctx) {
      const progress = easeOutCubic(t);
      const px = fromX + dx * progress;
      const py = fromY + dy * progress;
      const fadeOut = t > 0.85 ? 1 - (t - 0.85) / 0.15 : 1;
      const bodySize = 14 * (1 - t * 0.15) * fadeOut;

      system.drawGlowCircle(ctx, px, py, bodySize + 8, color, 0.25 * fadeOut, 25);
      system.drawGlowCircle(ctx, px, py, bodySize, color, 0.9 * fadeOut, 15);
      system.drawGlowCircle(ctx, px, py, bodySize * 0.5, WHITE, 0.85 * fadeOut, 10);
    },
    onComplete(sys) {
      emitImpact(sys, toX, toY, color);
    },
  });
}

// ─── Effect: Impact (at projectile destination) ───

export function emitImpact(system: ParticleSystem, x: number, y: number, color: RGB = COMBAT_COLOR) {
  const now = performance.now();

  system.emit([
    ...radialBurst(x, y, color, { count: 14, speedRange: [80, 180], lifeRange: [0.2, 0.4], sizeRange: [2.5, 5], jitter: 0.4, drag: 4, glow: 10, alpha: 0.9 }),
    ...coreSparkles(x, y, 4, [40, 80], { glow: 14, sizeRange: [3, 6] }),
  ]);

  system.addEffect({
    startTime: now,
    duration: 400,
    update() {},
    draw(t, ctx) {
      const flashAlpha = (1 - easeOutCubic(t)) * 0.9;
      const flashSize = 10 + easeOutCubic(t) * 40;
      system.drawGlowCircle(ctx, x, y, flashSize, WHITE, flashAlpha, 20);
      system.drawGlowCircle(ctx, x, y, flashSize * 0.6, color, flashAlpha * 0.7, 12);

      const ringAlpha = (1 - t) * 0.8;
      const ringSize = 8 + easeOutQuart(t) * 55;
      system.drawGlowRing(ctx, x, y, ringSize, color, ringAlpha, 2.5, 12);
    },
  });
}

// ─── Effect: Spell Impact (spell resolves) ───

export function emitSpellImpact(system: ParticleSystem, x: number, y: number, color: RGB = SPELL_COLOR) {
  const now = performance.now();

  system.emit([
    ...radialBurst(x, y, color, { count: 22, speedRange: [80, 220], lifeRange: [0.35, 0.65], sizeRange: [2.5, 6], glow: 12 }),
    ...coreSparkles(x, y, 8, [30, 80], { glow: 16, sizeRange: [4, 8] }),
    ...emberDebris(x, y, color, 6),
  ]);

  // Triple shockwave rings
  system.addEffect({
    startTime: now,
    duration: 800,
    update() {},
    draw(t, ctx) {
      const flashAlpha = (1 - easeOutCubic(t)) * 0.75;
      const flashSize = 15 + easeOutCubic(t) * 55;
      system.drawGlowCircle(ctx, x, y, flashSize, color, flashAlpha, 25);
      system.drawGlowCircle(ctx, x, y, flashSize * 0.4, WHITE, flashAlpha, 15);

      const r1t = Math.min(t * 1.4, 1);
      const r1size = 10 + easeOutQuart(r1t) * 70;
      const r1alpha = (1 - r1t) * 0.9;
      system.drawGlowRing(ctx, x, y, r1size, color, r1alpha, 3.5 - r1t * 3, 16);

      const r2t = Math.max(0, Math.min((t - 0.08) * 1.3, 1));
      if (r2t > 0) {
        const r2size = 8 + easeOutQuart(r2t) * 90;
        const r2alpha = (1 - r2t) * 0.65;
        system.drawGlowRing(ctx, x, y, r2size, lerpColor(color, WHITE, 0.3), r2alpha, 2.5 - r2t * 2, 12);
      }

      const r3t = Math.max(0, Math.min((t - 0.18) * 1.15, 1));
      if (r3t > 0) {
        const r3size = 6 + easeOutQuart(r3t) * 110;
        const r3alpha = (1 - r3t) * 0.45;
        system.drawGlowRing(ctx, x, y, r3size, color, r3alpha, 1.5 - r3t, 8);
      }
    },
  });
}

// ─── Effect: Damage Flash (creature takes damage) ───

export function emitDamageFlash(system: ParticleSystem, x: number, y: number, amount: number) {
  const now = performance.now();
  const intensity = Math.min(amount / 3, 1);
  const count = 6 + Math.round(intensity * 8);
  const speedScale = 1 + intensity;

  system.emit([
    ...radialBurst(x, y, DAMAGE_COLOR, {
      count, speedRange: [50 * speedScale, 120 * speedScale], lifeRange: [0.2, 0.4],
      sizeRange: [2, 5], jitter: Math.PI, drag: 4, glow: 8, alpha: 0.85,
    }),
    ...coreSparkles(x, y, 3, [30, 60], { glow: 10, sizeRange: [3, 5] }),
  ]);

  system.addEffect({
    startTime: now,
    duration: 300,
    update() {},
    draw(t, ctx) {
      const alpha = (1 - easeOutCubic(t)) * (0.5 + intensity * 0.4);
      const size = 12 + easeOutCubic(t) * 30 * (1 + intensity);
      system.drawGlowCircle(ctx, x, y, size, DAMAGE_COLOR, alpha, 15);
      system.drawGlowCircle(ctx, x, y, size * 0.4, WHITE, alpha * 0.5, 8);

      const ringAlpha = (1 - t) * 0.5 * intensity;
      if (ringAlpha > 0.05) {
        system.drawGlowRing(ctx, x, y, 5 + easeOutQuart(t) * 30, DAMAGE_COLOR, ringAlpha, 1.5, 8);
      }
    },
  });
}

// ─── Effect: Player Damage (sparks at player avatar) ───

export function emitPlayerDamage(system: ParticleSystem, x: number, y: number, amount: number) {
  const now = performance.now();
  const intensity = Math.min(amount / 4, 1);
  const count = 12 + Math.round(intensity * 12);
  const speedScale = 1 + intensity * 0.6;

  system.emit([
    ...radialBurst(x, y, DAMAGE_COLOR, {
      count, speedRange: [80 * speedScale, 200 * speedScale], lifeRange: [0.25, 0.55],
      sizeRange: [2.5, 6], jitter: Math.PI, drag: 2.5, glow: 12,
    }),
    ...coreSparkles(x, y, 6, [50, 120], { glow: 16, sizeRange: [3, 7] }),
    ...(amount >= 3 ? emberDebris(x, y, { r: 255, g: 100, b: 30 }, 6) : []),
  ]);

  system.addEffect({
    startTime: now,
    duration: 450,
    update() {},
    draw(t, ctx) {
      const alpha = (1 - easeOutCubic(t)) * 0.75;
      const size = 20 + easeOutCubic(t) * 45 * (1 + intensity);
      system.drawGlowCircle(ctx, x, y, size, DAMAGE_COLOR, alpha, 25);
      system.drawGlowCircle(ctx, x, y, size * 0.4, WHITE, alpha * 0.6, 12);

      const ringAlpha = (1 - t) * 0.7;
      const ringSize = 12 + easeOutQuart(t) * 65;
      system.drawGlowRing(ctx, x, y, ringSize, DAMAGE_COLOR, ringAlpha, 2.5, 12);
    },
  });
}

// ─── Effect: Heal (rising sparkles) ───

export function emitHealEffect(system: ParticleSystem, x: number, y: number, amount: number) {
  const now = performance.now();
  const count = 8 + Math.min(amount, 4) * 3;

  const particles: Partial<Particle>[] = [];
  for (let i = 0; i < count; i++) {
    particles.push({
      x: x + randRange(-18, 18), y: y + randRange(-5, 12),
      vx: randRange(-20, 20), vy: randRange(-70, -150),
      life: randRange(0.45, 0.8), size: randRange(2, 5), endSize: 0,
      r: HEAL_COLOR.r, g: HEAL_COLOR.g, b: HEAL_COLOR.b,
      alpha: 0.9, drag: 0.8, glow: 10,
    });
  }
  for (let i = 0; i < 5; i++) {
    particles.push({
      x: x + randRange(-12, 12), y,
      vx: randRange(-15, 15), vy: randRange(-90, -170),
      life: randRange(0.3, 0.55), size: randRange(2, 4), endSize: 0,
      r: 255, g: 255, b: 255,
      alpha: 0.8, drag: 0.8, glow: 12,
    });
  }
  system.emit(particles);

  system.addEffect({
    startTime: now,
    duration: 500,
    update() {},
    draw(t, ctx) {
      const alpha = (1 - easeOutCubic(t)) * 0.5;
      const size = 18 + easeOutCubic(t) * 28;
      system.drawGlowCircle(ctx, x, y, size, HEAL_COLOR, alpha, 18);
      system.drawGlowCircle(ctx, x, y, size * 0.4, WHITE, alpha * 0.5, 10);
    },
  });
}

// ─── Effect: Summon Burst (creature enters battlefield) ───

export function emitSummonBurst(system: ParticleSystem, x: number, y: number, color: RGB = COMBAT_COLOR) {
  const now = performance.now();

  // Ring of outward-then-upward sparkles
  const risingRing: Partial<Particle>[] = [];
  for (let i = 0; i < 16; i++) {
    const angle = (i / 16) * Math.PI * 2;
    const speed = randRange(50, 110);
    risingRing.push({
      x: x + Math.cos(angle) * randRange(5, 20), y: y + randRange(-5, 5),
      vx: Math.cos(angle) * speed * 0.5,
      vy: -Math.abs(Math.sin(angle) * speed) - randRange(40, 90),
      life: randRange(0.35, 0.6), size: randRange(2, 5), endSize: 0,
      r: color.r, g: color.g, b: color.b,
      alpha: 0.85, drag: 1.5, glow: 10,
    });
  }

  // White accent sparks rising fast
  const risingSparks: Partial<Particle>[] = [];
  for (let i = 0; i < 6; i++) {
    risingSparks.push({
      x: x + randRange(-12, 12), y,
      vx: randRange(-25, 25), vy: randRange(-100, -180),
      life: randRange(0.25, 0.4), size: randRange(2, 4), endSize: 0,
      r: 255, g: 255, b: 255, alpha: 0.8, drag: 1, glow: 12,
    });
  }

  system.emit([
    ...risingRing,
    ...risingSparks,
    ...radialBurst(x, y + 10, color, { count: 8, speedRange: [60, 120], lifeRange: [0.2, 0.35], sizeRange: [1.5, 3], jitter: Math.PI, drag: 4, glow: 6, alpha: 0.7 }),
  ]);

  // Rising energy column + expanding ground ring
  system.addEffect({
    startTime: now,
    duration: 500,
    update() {},
    draw(t, ctx) {
      // Vertical energy column
      const columnAlpha = (1 - easeOutCubic(t)) * 0.6;
      const columnHeight = easeOutCubic(t) * 120;
      const columnWidth = 8 * (1 - t * 0.5);
      if (columnAlpha > 0.05) {
        const grad = ctx.createLinearGradient(x, y, x, y - columnHeight);
        grad.addColorStop(0, `rgba(${color.r}, ${color.g}, ${color.b}, ${columnAlpha})`);
        grad.addColorStop(0.5, `rgba(${color.r}, ${color.g}, ${color.b}, ${columnAlpha * 0.5})`);
        grad.addColorStop(1, `rgba(${color.r}, ${color.g}, ${color.b}, 0)`);
        ctx.fillStyle = grad;
        ctx.fillRect(x - columnWidth / 2, y - columnHeight, columnWidth, columnHeight);

        const coreWidth = columnWidth * 0.4;
        const coreGrad = ctx.createLinearGradient(x, y, x, y - columnHeight * 0.7);
        coreGrad.addColorStop(0, `rgba(255, 255, 255, ${columnAlpha * 0.8})`);
        coreGrad.addColorStop(1, `rgba(255, 255, 255, 0)`);
        ctx.fillStyle = coreGrad;
        ctx.fillRect(x - coreWidth / 2, y - columnHeight * 0.7, coreWidth, columnHeight * 0.7);
      }

      // Center flash
      const flashAlpha = (1 - easeOutCubic(t)) * 0.65;
      const flashSize = 12 + easeOutCubic(t) * 35;
      system.drawGlowCircle(ctx, x, y, flashSize, color, flashAlpha, 18);
      system.drawGlowCircle(ctx, x, y, flashSize * 0.4, WHITE, flashAlpha * 0.7, 10);

      // Ground ring expanding outward
      const ringAlpha = (1 - t) * 0.7;
      system.drawGlowRing(ctx, x, y, 6 + easeOutQuart(t) * 50, color, ringAlpha, 2.5 - t * 2, 12);
    },
  });
}

// ─── Effect: Block Clash (sparks at block midpoint) ───

export function emitBlockClash(system: ParticleSystem, x: number, y: number) {
  const now = performance.now();

  system.emit([
    ...radialBurst(x, y, BLOCK_COLOR, { count: 14, speedRange: [60, 150], lifeRange: [0.2, 0.4], sizeRange: [2, 5], drag: 4, glow: 10, alpha: 0.9 }),
    ...coreSparkles(x, y, 4, [30, 70], { glow: 14, sizeRange: [3, 6] }),
  ]);

  system.addEffect({
    startTime: now,
    duration: 400,
    update() {},
    draw(t, ctx) {
      const alpha = (1 - easeOutCubic(t)) * 0.7;
      system.drawGlowCircle(ctx, x, y, 8 + easeOutCubic(t) * 30, BLOCK_COLOR, alpha, 16);
      system.drawGlowCircle(ctx, x, y, 4 + easeOutCubic(t) * 12, WHITE, alpha * 0.6, 8);
      system.drawGlowRing(ctx, x, y, 5 + easeOutQuart(t) * 45, BLOCK_COLOR, alpha * 0.8, 2.5, 10);
    },
  });
}

// ─── Effect: Attack Burst (attacker declared) ───

export function emitAttackBurst(system: ParticleSystem, x: number, y: number, color: RGB = COMBAT_COLOR) {
  const now = performance.now();

  system.emit([
    ...radialBurst(x, y, color, { count: 16, speedRange: [60, 160], lifeRange: [0.25, 0.5], sizeRange: [2, 5], drag: 3, glow: 12, alpha: 0.9 }),
    ...coreSparkles(x, y, 5, [30, 80], { glow: 14, sizeRange: [3, 6] }),
  ]);

  system.addEffect({
    startTime: now,
    duration: 400,
    update() {},
    draw(t, ctx) {
      const alpha = (1 - easeOutCubic(t)) * 0.7;
      const size = 10 + easeOutCubic(t) * 35;
      system.drawGlowCircle(ctx, x, y, size, color, alpha, 18);
      system.drawGlowRing(ctx, x, y, 6 + easeOutQuart(t) * 45, color, alpha * 0.8, 2.5, 12);
    },
  });
}
