// ─── Life and counter changes ───
// A player gaining life: warm motes rising off their HUD in a soft light. A
// player losing life other than to damage: dark wisps drawn down out of it,
// with no light. A counter put on a permanent: a ring in the counter's tint
// closing onto the card as motes settle on it; one removed: the ring opening
// off the card as the motes lift away.

import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect } from "./cardVfxScene.ts";
import {
  clamp01,
  count,
  type EffectFrame,
  type EffectParts,
  PARTICLE_SHARE,
  type Particle,
  particleLayer,
  place,
  rand,
  sprite,
  TimedEffect,
  type Vec3,
} from "./vfxParticles.ts";

/** How long a life change plays, before pace. */
export const LIFE_CHANGE_S = 1.4;
/** How long a counter change plays, before pace. */
export const COUNTER_CHANGE_S = 0.9;

/** Life lost other than to damage: the dark red of drawn blood. */
const DRAWN: Vec3 = [0.24, 0.03, 0.05];

/** A counter's tint, by its serde key, as the board's counter badges colour
 *  them; any other counter is a pale neutral. */
const COUNTER_TINTS: Record<string, Vec3> = {
  P1P1: [0.36, 0.78, 0.38],
  M1M1: [0.82, 0.24, 0.22],
  loyalty: [0.95, 0.66, 0.2],
};
const NEUTRAL_TINT: Vec3 = [0.82, 0.8, 0.74];

export function counterTint(counterType: string): Vec3 {
  return COUNTER_TINTS[counterType] ?? NEUTRAL_TINT;
}

/** World coordinates of a pose's centre (world y runs up the screen). */
const centre = (pose: CardPose): [number, number] => [pose.x, -pose.y];

export interface LifeChangeParams {
  /** The player's HUD. */
  at: CardPose;
  /** Life gained (positive) or lost (negative). */
  amount: number;
  tier: CardVfxTier;
  pace: number;
}

export function createLifeChange(host: EffectHost, { at, amount, tier, pace }: LifeChangeParams): SceneEffect {
  const [x, y] = centre(at);
  const share = PARTICLE_SHARE[tier];
  const size = Math.min(Math.abs(amount), 10);
  const timing = { endS: LIFE_CHANGE_S, pace, impact: null, startMs: null };
  const look = ({ group, unit, clock }: EffectParts): EffectFrame => {
    const motes: Particle[] = [];
    if (amount > 0) {
      for (let i = 0; i < count(18 + 3 * size, share); i++) {
        motes.push({ pos: [x + rand(-0.45, 0.45) * at.w, y + rand(-0.4, 0.3) * at.h, 6], vel: [rand(-18, 18), rand(45, 115), 0], spawn: rand(0, 0.35), life: rand(0.7, 1.05), drag: 0.8, s0: rand(3, 5), s1: rand(6, 10), heat: rand(0.75, 1) });
      }
      group.add(particleLayer(motes, "FLAME", clock, { accZ: 0, gain: 0.7, cool: 0.9, palette: "life", order: 5 }));
      const halo = sprite(unit, "GLOW", [1, 0.84, 0.55], 4);
      group.add(halo);
      return (t) => place(halo, x, y, 0, Math.max(at.w, at.h) * 1.6, 0.32 * clamp01(t / 0.1) * Math.exp(-Math.max(t - 0.1, 0) / 0.45));
    }
    for (let i = 0; i < count(14 + 2 * size, share); i++) {
      motes.push({ pos: [x + rand(-0.4, 0.4) * at.w, y + rand(-0.2, 0.3) * at.h, 6], vel: [rand(-12, 12), -rand(35, 80), 0], spawn: rand(0, 0.3), life: rand(0.8, 1.1), drag: 1.2, s0: rand(6, 10), s1: rand(24, 38) });
    }
    group.add(particleLayer(motes, "SMOKE", clock, { accZ: 0, gain: 0.55, tint: DRAWN, order: 3 }));
    return () => {};
  };
  return new TimedEffect(host, "life-change", timing, look);
}

/** Whether counters were put on the permanent or removed from it. */
export type CounterChange = "added" | "removed";

export interface CounterChangeParams {
  /** The permanent's surface. */
  at: CardPose;
  counterType: string;
  change: CounterChange;
  /** How many counters changed. */
  count: number;
  tier: CardVfxTier;
  pace: number;
}

export function createCounterChange(
  host: EffectHost,
  { at, counterType, change, count: changed, tier, pace }: CounterChangeParams,
): SceneEffect {
  const [x, y] = centre(at);
  const tint = counterTint(counterType);
  const span = Math.min(at.w, at.h);
  const share = PARTICLE_SHARE[tier];
  const timing = { endS: COUNTER_CHANGE_S, pace, impact: null, startMs: null };
  const look = ({ group, unit, clock }: EffectParts): EffectFrame => {
    const motes: Particle[] = [];
    for (let i = 0; i < count(10 + 4 * Math.min(changed, 5), share); i++) {
      const a = rand(0, Math.PI * 2);
      const r = rand(0.6, 0.95) * span;
      const [cx, cy] = [Math.cos(a), Math.sin(a)];
      const speed = rand(60, 120);
      // Added: from the ring in onto the card. Removed: off the card, outward and up.
      motes.push(
        change === "added"
          ? { pos: [x + cx * r, y + cy * r, 6], vel: [-cx * speed, -cy * speed, 0], spawn: rand(0, 0.15), life: rand(0.35, 0.55), drag: 2.5, s0: rand(4, 7), s1: rand(2, 4) }
          : { pos: [x + cx * r * 0.3, y + cy * r * 0.3, 6], vel: [cx * speed, cy * speed + 30, 0], spawn: rand(0, 0.12), life: rand(0.45, 0.7), drag: 2, s0: rand(3, 5), s1: rand(8, 12) },
      );
    }
    group.add(particleLayer(motes, "SMOKE", clock, { accZ: 0, gain: 0.8, tint: tint, order: 3 }));
    const ring = sprite(unit, "RING", tint, 6);
    group.add(ring);
    return (t) => {
      const k = clamp01(t / 0.4);
      const eased = 1 - (1 - k) ** 3;
      const scale = change === "added" ? 2.4 - 1.4 * eased : 1 + 1.4 * eased;
      const intensity = change === "added" ? 0.55 * Math.sin(Math.PI * k) : 0.55 * (1 - k) ** 2;
      place(ring, x, y, 2, scale * span, intensity);
    };
  };
  return new TimedEffect(host, "counter-change", timing, look);
}
