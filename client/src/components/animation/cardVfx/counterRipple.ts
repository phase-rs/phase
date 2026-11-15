// ─── Counter ripple ───
// A counter's ripple: rings spread from the countering spell or ability, a
// swell runs across to the spell it counters, leaving a wake of rings, and
// rings break over it. A blue counter's ripple is water, throwing drops and
// mist where it breaks; any other is a paler disturbance with no water in it.
// Nothing here is light: the countered spell's own flight washes it out.

import { type Object3D, PlaneGeometry } from "three";

import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import { COUNTER_RIPPLE_MS, type RippleLook } from "./cardFlightSpecs.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import {
  clamp01,
  type EffectFrame,
  type EffectParts,
  PARTICLE_SHARE,
  type Particle,
  particleLayer,
  place,
  sprite,
  TimedEffect,
  type Vec2,
  type Vec3,
} from "./vfxParticles.ts";
import { MIST, splash, WATER } from "./waterEffects.ts";

/** The swell reaches the countered spell after this long, before pace. */
const TRAVEL_S = COUNTER_RIPPLE_MS / 1000;
/** The rings over the countered spell outlive the swell's arrival by this long. */
const TAIL_S = 0.9;
/** The swell leaves a ring of its wake this often. */
const WAKE_EVERY_S = 0.05;

const LOOKS: Record<RippleLook, { color: Vec3; strength: number }> = {
  water: { color: [0.88, 0.95, 1], strength: 1 },
  pale: { color: [1, 1, 0.96], strength: 0.7 },
};

export interface CounterRippleParams {
  /** The countering spell's stack entry, or the permanent whose ability counters. */
  from: CardPose;
  /** The countered spell or ability's stack entry. */
  to: CardPose;
  look: RippleLook;
  tier: CardVfxTier;
  pace: number;
}

interface Ring {
  atS: number;
  mesh: ReturnType<typeof sprite>;
}

const smooth = (u: number) => u * u * (3 - 2 * u);

function rippleFrame(
  { group, unit, clock }: EffectParts,
  S: Vec2,
  T: Vec2,
  span: number,
  { look, tier }: CounterRippleParams,
): EffectFrame {
  const { color, strength } = LOOKS[look];
  const ring = (atS: number): Ring => ({ atS, mesh: sprite(unit, "RIPPLE", color, 4) });
  const at = (u: number): Vec2 => [S[0] + (T[0] - S[0]) * u, S[1] + (T[1] - S[1]) * u];
  const released = [0, 0.1].map(ring);
  const wake = Array.from({ length: Math.floor(TRAVEL_S / WAKE_EVERY_S) }, (_, i) => ring(i * WAKE_EVERY_S));
  const breaking = [0, 0.08, 0.18].map((s) => ring(TRAVEL_S + s));
  const swell = sprite(unit, "RIPPLE", color, 5);
  group.add(swell, ...[...released, ...wake, ...breaking].map(({ mesh }) => mesh));
  if (look === "water") {
    const dist = Math.hypot(T[0] - S[0], T[1] - S[1]) || 1;
    const water = { drops: [] as Particle[], mist: [] as Particle[] };
    splash(T, TRAVEL_S, [(T[0] - S[0]) / dist, (T[1] - S[1]) / dist], 0.9, PARTICLE_SHARE[tier], water);
    group.add(
      particleLayer(water.mist, "SMOKE", clock, { accZ: 20, gain: 0.26, tint: MIST, order: 3 }),
      particleLayer(water.drops, "DROP", clock, { accZ: -900, gain: 1, tint: WATER, order: 6 }),
    );
  }

  /** A ring spreading from `atS` over `lifeS`, from `from` to `to` spans across. */
  const spread = ({ atS, mesh }: Ring, [x, y]: Vec2, t: number, lifeS: number, from: number, to: number, gain: number) => {
    const rp = clamp01((t - atS) / lifeS);
    const on = t >= atS && rp < 1;
    place(mesh, x, y, 2, (from + (to - from) * (1 - (1 - rp) ** 2)) * span, on ? (1 - rp) ** 1.5 * gain * strength : 0);
  };

  return (t) => {
    const u = smooth(clamp01(t / TRAVEL_S));
    place(swell, ...at(u), 3, span * 0.8, t < TRAVEL_S ? 0.9 * strength * Math.min(t / 0.06, 1) : 0);
    for (const r of released) spread(r, S, t, 0.5, 0.5, 2, 0.8);
    for (const r of wake) spread(r, at(smooth(r.atS / TRAVEL_S)), t, 0.35, 0.3, 1, 0.5);
    for (const r of breaking) spread(r, T, t, 0.6, 0.4, 2.4, 0.9);
  };
}

function center(pose: CardPose): Vec2 {
  return [pose.x, -pose.y];
}

/** Creates a counter's ripple from `from` to `to`. It reports no impact: the
 *  countered spell's flight keeps its own time. */
export function createCounterRipple(host: EffectHost, params: CounterRippleParams): SceneEffect {
  const span = Math.min(params.to.w, params.to.h);
  const timing = { endS: TRAVEL_S + TAIL_S, pace: params.pace, impact: null, startMs: null };
  return new TimedEffect(host, "counter-ripple", timing, (parts) =>
    rippleFrame(parts, center(params.from), center(params.to), span, params),
  );
}

export const counterRippleKind: SceneEffectKind = {
  // The ripple's sprite and the splash's drops and mist, built as it builds them.
  warmUp() {
    const clock = { value: 0 };
    const dead: Particle[] = [{ pos: [0, 0, 0], vel: [0, 0, 0], spawn: -2, life: 1, drag: 1, s0: 0 }];
    const objects: Object3D[] = [
      sprite(new PlaneGeometry(1, 1), "RIPPLE", [0, 0, 0], 4),
      particleLayer(dead, "SMOKE", clock, { accZ: 0, gain: 0, order: 3 }),
      particleLayer(dead, "DROP", clock, { accZ: 0, gain: 0, order: 6 }),
    ];
    objects.forEach((object, i) => {
      object.name = `counter-ripple-warmup-${i}`;
    });
    return objects;
  },
};
