// ─── Board sweep ───
// A front a resolving spell sends across the board from its caster's side,
// setting off each permanent it moves or destroys as it reaches it: a wave
// for a blue spell's mass bounce, a wall of light for a white wrath, rolling
// smoke for a black one. The front is the cause, so only the light glows;
// water and smoke draw in ordinary alpha. The permanents' own effects (their
// flights, their shatters) wait for the times its path gives them.

import { Mesh, type Object3D, PlaneGeometry, ShaderMaterial, Vector3 } from "three";

import type { CardVfxTier } from "./cardFlight.ts";
import type { SweepLook } from "./cardFlightSpecs.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { LEAD_PX, SWEEP_TRAVEL_S, type SweepPath } from "./sweepPath.ts";
import {
  ADDITIVE,
  clamp01,
  count,
  type EffectFrame,
  type EffectParts,
  FBM_GLSL,
  NORMAL,
  PARTICLE_SHARE,
  type Particle,
  particleLayer,
  rand,
  TimedEffect,
  type Vec2,
} from "./vfxParticles.ts";
import { MIST, splash, WATER } from "./waterEffects.ts";

/** How far behind the front its body trails, in px. */
const TRAIL_PX = 220;
/** The body fades out over this long once the front has crossed, before pace. */
const FADE_S = 0.45;

const LOOK_DEFINES: Record<SweepLook, string> = { tsunami: "WAVE", light: "LIGHT", smoke: "SMOKE" };

const sweepVert = /* glsl */ `
  varying vec2 vUv;
  void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

// The front runs along the quad's length (uv.x, from its back end) and spans
// its width. Ahead of it is clear board; behind it trails its body.
const sweepFrag = /* glsl */ `
  uniform float uFront, uLength, uTrail, uTime, uGain; uniform vec3 uTint;
  varying vec2 vUv;
  ${FBM_GLSL}
  void main() {
    float along = vUv.x * uLength;
    float across = vUv.y * 2.0 - 1.0;
    float sides = 1.0 - smoothstep(0.7, 1.0, abs(across));
    // Behind the front d < 0; its edge churns across its width.
    float d = along - uFront - (vnoise(vec2(across * 5.0, uTime * 1.6)) - 0.5) * 36.0;
    #ifdef WAVE
    // A wall of water: white foam on its crest, clear water behind it that
    // darkens and thins toward its trailing edge, streaked by its flow.
    if (d > 10.0) discard;
    float foam = exp(-pow(d / 16.0, 2.0)) * (0.55 + 0.6 * vnoise(vec2(along * 0.06 - uTime * 3.0, across * 9.0)));
    float body = smoothstep(-uTrail, -uTrail * 0.25, d) * step(d, 0.0);
    float flow = smoothstep(0.55, 0.85, fbm(vec2(along * 0.03 + uTime * 2.4, across * 4.0)));
    vec3 col = mix(uTint * 0.55, uTint, body);
    col = mix(col, vec3(1.0), clamp(foam + flow * 0.35 * body, 0.0, 1.0));
    gl_FragColor = vec4(col, clamp(body * 0.5 + foam * 0.9 + flow * 0.2 * body, 0.0, 1.0) * sides * uGain);
    #endif
    #ifdef LIGHT
    // A wall of light: brightest on its edge, a softer glow left behind it
    // and a little thrown ahead.
    float wall = exp(-pow(d / 34.0, 2.0));
    float wake = d < 0.0 ? exp(d / 140.0) * 0.35 : exp(-d / 26.0) * 0.4;
    float shimmer = 0.75 + 0.45 * fbm(vec2(along * 0.02 - uTime * 1.5, across * 3.0));
    gl_FragColor = vec4(uTint * (wall + wake) * shimmer * sides * uGain, 0.0);
    #endif
    #ifdef SMOKE
    // Rolling smoke: a billowing leading edge, thick behind it, thinning out
    // toward its tail.
    float n = fbm(vec2(along * 0.012 - uTime * 0.5, across * 2.2 + uTime * 0.3));
    float edge = 1.0 - smoothstep(-50.0, 26.0, d + (n - 0.5) * 90.0);
    float tail = smoothstep(-uTrail * 1.2, -uTrail * 0.3, d);
    float density = edge * tail * (0.45 + 0.75 * n);
    // Its billows catch a little light, so it reads even over a dark board.
    vec3 col = mix(uTint, vec3(0.3, 0.25, 0.34), smoothstep(0.45, 0.9, n) * (1.0 - tail * 0.4));
    gl_FragColor = vec4(col, clamp(density, 0.0, 1.0) * 0.85 * sides * uGain);
    #endif
  }`;

const TINTS: Record<SweepLook, Vector3> = {
  tsunami: new Vector3(...WATER),
  light: new Vector3(1, 0.95, 0.82),
  smoke: new Vector3(0.05, 0.03, 0.07),
};

function frontMaterial(look: SweepLook, clock: { value: number }, lengthPx: number): ShaderMaterial {
  return new ShaderMaterial({
    vertexShader: sweepVert,
    fragmentShader: sweepFrag,
    ...(look === "light" ? ADDITIVE : NORMAL),
    defines: { [LOOK_DEFINES[look]]: "" },
    uniforms: {
      uFront: { value: 0 },
      uLength: { value: lengthPx },
      uTrail: { value: TRAIL_PX },
      uTime: clock,
      uGain: { value: 0 },
      uTint: { value: TINTS[look] },
    },
  });
}

export interface BoardSweepParams {
  path: SweepPath;
  look: SweepLook;
  tier: CardVfxTier;
  pace: number;
  /** When the front sets out, on the frame clock (`performance.now()`). */
  startMs: number;
}

const smooth = (u: number) => u * u * (3 - 2 * u);

// World space is canvas px with y up.
const world = ([x, y]: Vec2): Vec2 => [x, -y];

function sweepFrame({ group, unit, clock }: EffectParts, { path, look, tier }: BoardSweepParams): EffectFrame {
  const from = world(path.from);
  const to = world(path.to);
  const travel = Math.hypot(to[0] - from[0], to[1] - from[1]);
  const dir: Vec2 = [(to[0] - from[0]) / travel, (to[1] - from[1]) / travel];
  // The quad runs from a trail's length behind the start to just past the end.
  const back = TRAIL_PX * 1.3;
  const lengthPx = travel + back + LEAD_PX;
  const front = new Mesh(unit, frontMaterial(look, clock, lengthPx));
  const middle = (travel + LEAD_PX - back) / 2;
  front.position.set(from[0] + dir[0] * middle, from[1] + dir[1] * middle, 4);
  front.rotation.z = Math.atan2(dir[1], dir[0]);
  front.scale.set(lengthPx, path.halfWidth * 2, 1);
  front.renderOrder = 4;
  group.add(front);

  const share = PARTICLE_SHARE[tier];
  switch (look) {
    case "tsunami": {
      // The wave throws its water up over every card it breaks on.
      const water = { drops: [] as Particle[], mist: [] as Particle[] };
      for (const hit of path.hits) splash(world(hit.at), hit.atS, dir, 0.8, share / 2, water);
      group.add(
        particleLayer(water.mist, "SMOKE", clock, { accZ: 20, gain: 0.18, tint: MIST, order: 3 }),
        particleLayer(water.drops, "DROP", clock, { accZ: -900, gain: 1, tint: WATER, order: 6 }),
      );
      break;
    }
    case "smoke": {
      // Smoke curls up from every card it engulfs.
      const puffs = path.hits.flatMap((hit) =>
        Array.from({ length: count(10, share) }, (): Particle => {
          const [x, y] = world(hit.at);
          return {
            pos: [x + rand(-30, 30), y + rand(-40, 40), 4],
            vel: [dir[0] * rand(20, 80), dir[1] * rand(20, 80), rand(20, 60)],
            spawn: hit.atS + rand(0, 0.12),
            life: rand(0.7, 1.2),
            drag: 2,
            s0: rand(30, 50),
            s1: rand(90, 140),
          };
        }),
      );
      group.add(particleLayer(puffs, "SMOKE", clock, { accZ: 30, gain: 0.55, tint: [0.04, 0.025, 0.06], order: 3 }));
      break;
    }
    case "light":
      break;
  }

  const uniforms = front.material.uniforms;
  return (t) => {
    // At a steady speed, so it reaches each card when the card's time comes.
    uniforms.uFront.value = back + clamp01(t / SWEEP_TRAVEL_S) * travel;
    uniforms.uGain.value = clamp01(t / 0.08) * (1 - smooth(clamp01((t - SWEEP_TRAVEL_S) / FADE_S)));
  };
}

/** The front `params.path` describes, setting out at `params.startMs`. */
export function createBoardSweep(host: EffectHost, params: BoardSweepParams): SceneEffect {
  const timing = { endS: SWEEP_TRAVEL_S + FADE_S + 0.9, pace: params.pace, impact: null, startMs: params.startMs };
  return new TimedEffect(host, "board-sweep", timing, (parts) => sweepFrame(parts, params));
}

export const boardSweepKind: SceneEffectKind = {
  // Each look's front, and the particles the wave and the smoke throw, built as they build them.
  warmUp() {
    const clock = { value: 0 };
    const unit = new PlaneGeometry(1, 1);
    const dead: Particle[] = [{ pos: [0, 0, 0], vel: [0, 0, 0], spawn: -2, life: 1, drag: 1, s0: 0 }];
    const objects: Object3D[] = [
      ...(["tsunami", "light", "smoke"] as const).map((look) => new Mesh(unit, frontMaterial(look, clock, 1))),
      particleLayer(dead, "SMOKE", clock, { accZ: 0, gain: 0, order: 3 }),
      particleLayer(dead, "DROP", clock, { accZ: 0, gain: 0, order: 6 }),
    ];
    objects.forEach((object, i) => {
      object.name = `board-sweep-warmup-${i}`;
    });
    return objects;
  },
};
