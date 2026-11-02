// ─── VFX particles ───
// The building blocks the card VFX draw light, smoke and dust with: instanced
// particles whose motion runs on the GPU, and the few large sprites the CPU
// moves each frame. Light is colour written with alpha 0, which the browser
// adds over the DOM board; smoke and dust are ordinary alpha.

import {
  AddEquation,
  CustomBlending,
  Group,
  InstancedBufferAttribute,
  InstancedBufferGeometry,
  MathUtils,
  Mesh,
  OneFactor,
  PlaneGeometry,
  ShaderMaterial,
  Vector2,
  Vector3,
  ZeroFactor,
} from "three";

import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect } from "./cardVfxScene.ts";
import { BURN_KEY_GLSL, BURN_SPAN, VALUE_NOISE_GLSL } from "./glslChunks.ts";

/** The share of the lab's particle counts each tier emits. */
export const PARTICLE_SHARE: Record<CardVfxTier, number> = { full: 1, reduced: 0.5 };

export type Vec3 = [number, number, number];
export type Vec2 = [number, number];

export const ADDITIVE = {
  transparent: true,
  depthWrite: false,
  depthTest: false,
  blending: CustomBlending,
  blendEquation: AddEquation,
  blendSrc: OneFactor,
  blendDst: OneFactor,
  blendSrcAlpha: ZeroFactor,
  blendDstAlpha: OneFactor,
} as const;
export const NORMAL = { transparent: true, depthWrite: false, depthTest: false } as const;

export const rand = (lo: number, hi: number) => lo + Math.random() * (hi - lo);
export const clamp01 = (x: number) => MathUtils.clamp(x, 0, 1);

/** `n` scaled by a tier's particle share. */
export const count = (n: number, share: number) => Math.round(n * share);

/** `fbm`: three octaves of `vnoise`, in [0, 1]. */
export const FBM_GLSL = /* glsl */ `
  ${VALUE_NOISE_GLSL}
  float fbm(vec2 p) { return vnoise(p) * 0.55 + vnoise(p * 2.1 + 3.7) * 0.3 + vnoise(p * 4.3 + 9.1) * 0.15; }`;

/** How hot light cools: fire white → yellow → orange → deep red; lightning
 *  white → blue; life warm white → pale gold. */
export type Palette = "fire" | "lightning" | "life";

const PALETTE_INDEX: Record<Palette, number> = { fire: 0, lightning: 1, life: 2 };

/** `ramp(h)`: the colour of light at heat `h` in [0, 1], in the `uPalette` palette. */
export const RAMP_GLSL = /* glsl */ `
  uniform float uPalette;
  vec3 ramp(float h) {
    h = clamp(h, 0.0, 1.0);
    if (uPalette > 1.5) {
      vec3 g = mix(vec3(0.32, 0.2, 0.05), vec3(1.0, 0.8, 0.42), smoothstep(0.0, 0.5, h));
      return mix(g, vec3(1.0, 0.98, 0.9), smoothstep(0.55, 1.0, h));
    }
    if (uPalette > 0.5) {
      vec3 e = mix(vec3(0.08, 0.1, 0.35), vec3(0.35, 0.55, 1.0), smoothstep(0.0, 0.5, h));
      return mix(e, vec3(0.92, 0.96, 1.0), smoothstep(0.55, 1.0, h));
    }
    vec3 c = mix(vec3(0.3, 0.03, 0.01), vec3(1.0, 0.32, 0.04), smoothstep(0.0, 0.45, h));
    c = mix(c, vec3(1.0, 0.72, 0.28), smoothstep(0.4, 0.75, h));
    return mix(c, vec3(1.0, 0.96, 0.84), smoothstep(0.78, 1.0, h));
  }`;

// ---------- Particles: one instanced quad each, all motion on the GPU ----------

/** FLAME and SPARK are light; SMOKE is a soft cloud and DROP a bead of
 *  water, both ordinary alpha in their `tint`. */
export type ParticleKind = "FLAME" | "EMBER" | "SPARK" | "SMOKE" | "DROP" | "ICE";

export interface Particle {
  pos: Vec3;
  vel: Vec3;
  spawn: number;
  life: number;
  drag: number;
  s0: number;
  s1?: number;
  heat?: number;
  stretch?: number;
}

/** Arrival uniforms shared with a card burning from the impact point. */
export interface ParticleBurnKey {
  uSize: { value: Vector2 };
  uKeyImpact: { value: Vector2 };
  uKeyMax: { value: number };
  uKeyMode: { value: number };
  uBurnStart: { value: number };
  uBurnS: { value: number };
}

export interface ParticleLook {
  key?: ParticleBurnKey;
  accZ: number;
  gain: number;
  cool?: number;
  palette?: Palette;
  /** SMOKE's colour, or the water DROP shows through its body. */
  tint?: Vec3;
  order: number;
}

const particleVert = /* glsl */ `
  attribute vec3 aPos; attribute vec3 aVel; attribute vec4 aTime; attribute vec4 aLook;
  uniform float uTime, uAccZ;
  varying vec2 vUv; varying float vA, vHeat, vSeed;
  #ifdef FROM_KEY
  ${FBM_GLSL}
  ${BURN_KEY_GLSL}
  uniform float uBurnStart, uBurnS;
  #endif
  void main() {
    float spawn = aTime.x;
    #ifdef FROM_KEY
    spawn += uBurnStart + (burnKey(vec2(aPos.x, -aPos.y)) + 0.08) / ${BURN_SPAN.toFixed(2)} * uBurnS;
    #endif
    float age = uTime - spawn;
    float a = age / aTime.y;
    if (age < 0.0 || a >= 1.0) { gl_Position = vec4(0.0, 0.0, 2.0, 1.0); return; }
    float drag = aTime.z;
    vec3 p = aPos + aVel * (1.0 - exp(-drag * age)) / drag;
    p.z += 0.5 * uAccZ * age * age;
    #if defined(SPARK) || defined(DROP) || defined(ICE)
    // Sparks and drops fall to the table and stay on it.
    p.z = max(p.z, 0.0);
    #endif
    #ifdef SPARK
    vec3 v = aVel * exp(-drag * age) + vec3(0.0, 0.0, uAccZ * age);
    vec2 dir = length(v.xy) > 0.001 ? normalize(v.xy) : vec2(1.0, 0.0);
    float len = max(length(v.xy) * aLook.w, aLook.x);
    vec3 local = p + vec3(dir * (position.x - 0.5) * len + vec2(-dir.y, dir.x) * position.y * aLook.x, 0.0);
    #else
    float s = mix(aLook.x, aLook.y, 1.0 - (1.0 - a) * (1.0 - a));
    #ifdef DROP
    // A drop does not turn: its highlight stays on the side facing the light.
    vec3 local = p + vec3(position.xy * s, 0.0);
    #else
    float ang = aTime.w * 6.2831 + age * (aTime.w - 0.5) * 3.0;
    vec3 local = p + vec3(mat2(cos(ang), sin(ang), -sin(ang), cos(ang)) * position.xy * s, 0.0);
    #endif
    #endif
    vUv = position.xy + 0.5; vA = a; vHeat = aLook.z; vSeed = aTime.w;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(local, 1.0);
  }`;

const particleFrag = /* glsl */ `
  uniform float uGain, uCool, uTime; uniform vec3 uTint;
  varying vec2 vUv; varying float vA, vHeat, vSeed;
  ${FBM_GLSL}
  ${RAMP_GLSL}
  void main() {
    vec2 q = vUv * 2.0 - 1.0;
    #if defined(SPARK)
    float across = 1.0 - smoothstep(0.1, 1.0, abs(q.y));
    float along = smoothstep(0.0, 1.0, vUv.x);
    gl_FragColor = vec4(ramp(vHeat * (1.0 - 0.6 * vA)) * across * along * (1.0 - vA * vA) * uGain, 0.0);
    #elif defined(SMOKE)
    float r = length(q) + (vnoise(q * 2.2 + vSeed * 37.0) - 0.5) * 0.5;
    float fade = smoothstep(0.0, 0.2, vA) * (1.0 - smoothstep(0.4, 1.0, vA));
    gl_FragColor = vec4(uTint, (1.0 - smoothstep(0.3, 1.0, r)) * fade * uGain);
    #elif defined(ICE)
    float edge = abs(q.x) * 1.7 + abs(q.y);
    if (edge > 1.0) discard;
    float facet = step(0.0, q.x + q.y * 0.25);
    vec3 col = mix(uTint * 0.65, vec3(0.92, 0.99, 1.0), facet);
    col = mix(col, vec3(1.0), smoothstep(0.82, 0.98, edge));
    gl_FragColor = vec4(col, (1.0 - smoothstep(0.4, 1.0, vA)) * uGain);
    #elif defined(DROP)
    // A bead of water: a clear body, a rim darkened where it bends the light,
    // and the highlight of the light above. It thins out as it spreads.
    float r = length(q);
    if (r >= 1.0) discard;
    float fade = smoothstep(0.0, 0.05, vA) * (1.0 - smoothstep(0.5, 1.0, vA));
    float rim = smoothstep(0.6, 0.95, r) * (1.0 - smoothstep(0.95, 1.0, r));
    float glint = 1.0 - smoothstep(0.05, 0.3, length(q - vec2(-0.3, 0.35)));
    vec3 col = mix(mix(uTint, uTint * 0.4, rim), vec3(1.0), glint);
    gl_FragColor = vec4(col, (0.22 + 0.5 * rim + 0.75 * glint) * fade * uGain);
    #else
    float r = length(q) + (vnoise(q * 2.4 + vSeed * 37.0 + vA * 2.5) - 0.5) * 0.7;
    float shape = pow(max(1.0 - r, 0.0), 1.5);
    float fade = smoothstep(0.0, 0.1, vA) * (1.0 - vA);
    #ifdef EMBER
    shape = 1.0 - smoothstep(0.2, 1.0, length(q));
    fade *= 0.65 + 0.35 * sin(uTime * 40.0 + vSeed * 60.0);
    #endif
    gl_FragColor = vec4(ramp(vHeat * exp(-vA * uCool)) * shape * fade * uGain, 0.0);
    #endif
  }`;

export function particleLayer(
  list: readonly Particle[],
  kind: ParticleKind,
  clock: { value: number },
  { accZ, gain, cool = 1.6, palette = "fire", tint = [0.09, 0.075, 0.065], order, key }: ParticleLook,
): Mesh<InstancedBufferGeometry, ShaderMaterial> {
  const n = list.length;
  const base = new PlaneGeometry(1, 1);
  const geometry = new InstancedBufferGeometry();
  geometry.setIndex(base.index);
  geometry.setAttribute("position", base.getAttribute("position"));
  const pos = new Float32Array(n * 3);
  const vel = new Float32Array(n * 3);
  const time = new Float32Array(n * 4);
  const look = new Float32Array(n * 4);
  list.forEach((p, i) => {
    pos.set(p.pos, i * 3);
    vel.set(p.vel, i * 3);
    time.set([p.spawn, p.life, Math.max(p.drag, 0.1), Math.random()], i * 4);
    look.set([p.s0, p.s1 ?? p.s0, p.heat ?? 1, p.stretch ?? 0], i * 4);
  });
  geometry.setAttribute("aPos", new InstancedBufferAttribute(pos, 3));
  geometry.setAttribute("aVel", new InstancedBufferAttribute(vel, 3));
  geometry.setAttribute("aTime", new InstancedBufferAttribute(time, 4));
  geometry.setAttribute("aLook", new InstancedBufferAttribute(look, 4));
  geometry.instanceCount = n;
  const mesh = new Mesh(
    geometry,
    new ShaderMaterial({
      vertexShader: particleVert,
      fragmentShader: particleFrag,
      defines: { [kind]: "", ...(key ? { FROM_KEY: "" } : {}) },
      uniforms: {
        uTime: clock,
        uAccZ: { value: accZ },
        uGain: { value: gain },
        uCool: { value: cool },
        uPalette: { value: PALETTE_INDEX[palette] },
        uTint: { value: new Vector3(...tint) },
        ...key,
      },
      ...(kind === "SMOKE" || kind === "DROP" || kind === "ICE" ? NORMAL : ADDITIVE),
    }),
  );
  mesh.frustumCulled = false;
  mesh.renderOrder = order;
  return mesh;
}

// ---------- Sprites: the few large lights the CPU moves each frame ----------

/** GLOW and RING are light; SHADOW darkens the table; RIPPLE is a ring-shaped
 *  swell on water, lit in `color` on its slopes facing the light and shaded on
 *  those facing away. */
export type SpriteKind = "GLOW" | "RING" | "SHADOW" | "RIPPLE";

const spriteVert = /* glsl */ `
  varying vec2 vUv;
  void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

const spriteFrag = /* glsl */ `
  uniform vec3 uColor; uniform float uIntensity;
  varying vec2 vUv;
  void main() {
    float r = length(vUv * 2.0 - 1.0);
    #if defined(RING)
    float a = exp(-pow((r - 0.82) / 0.05, 2.0)) * (1.0 - smoothstep(0.95, 1.0, r));
    gl_FragColor = vec4(uColor * a * uIntensity, 0.0);
    #elif defined(SHADOW)
    gl_FragColor = vec4(0.0, 0.0, 0.0, exp(-r * r * 4.5) * (1.0 - smoothstep(0.9, 1.0, r)) * uIntensity);
    #elif defined(RIPPLE)
    // The swell's slope across the ring, turned toward or away from the light
    // above the table's top left: the cards' own light.
    float d = (r - 0.8) / 0.08;
    vec2 outward = r > 0.0 ? (vUv * 2.0 - 1.0) / r : vec2(0.0);
    float lit = 1.7 * d * exp(-d * d) * (0.3 + 0.7 * dot(outward, normalize(vec2(-0.55, 0.83))));
    float a = clamp(abs(lit), 0.0, 1.0) * (1.0 - smoothstep(0.95, 1.0, r)) * uIntensity;
    gl_FragColor = vec4(lit > 0.0 ? uColor : vec3(0.0), a);
    #else
    float a = (exp(-r * r * 6.0) + 0.6 * exp(-r * 14.0)) * (1.0 - smoothstep(0.8, 1.0, r));
    gl_FragColor = vec4(uColor * a * uIntensity, 0.0);
    #endif
  }`;

export function sprite(unit: PlaneGeometry, kind: SpriteKind, color: Vec3, order: number): Mesh<PlaneGeometry, ShaderMaterial> {
  const mesh = new Mesh(
    unit,
    new ShaderMaterial({
      vertexShader: spriteVert,
      fragmentShader: spriteFrag,
      defines: kind === "GLOW" ? {} : { [kind]: "" },
      uniforms: { uColor: { value: new Vector3(...color) }, uIntensity: { value: 0 } },
      ...(kind === "SHADOW" || kind === "RIPPLE" ? NORMAL : ADDITIVE),
    }),
  );
  mesh.renderOrder = order;
  mesh.frustumCulled = false;
  mesh.visible = false;
  return mesh;
}

export function place(mesh: Mesh<PlaneGeometry, ShaderMaterial>, x: number, y: number, z: number, size: number, intensity: number) {
  mesh.visible = intensity > 0.002;
  if (!mesh.visible) return;
  mesh.position.set(x, y, z);
  mesh.scale.setScalar(size);
  mesh.material.uniforms.uIntensity.value = intensity;
}

// ---------- A timed look: particles and sprites played once on one clock ----------

/** A look's per-frame update, given its time in seconds before pace. */
export type EffectFrame = (t: number) => void;

/** What a look draws with: its effect's group, shared unit quad and clock. */
export interface EffectParts {
  group: Group;
  unit: PlaneGeometry;
  clock: { value: number };
}

/** When an effect ends, in seconds before pace, and the moment it lands, if
 *  it reports one: `land` runs once, at `atS`. */
export interface EffectTiming {
  endS: number;
  pace: number;
  impact: { atS: number; land(): void } | null;
  /** When the effect's clock starts, on the frame clock (`performance.now()`);
   *  `null` starts it on its first frame. */
  startMs: number | null;
}

/** A look played once on its own clock. It removes itself after `endS`. */
export class TimedEffect implements SceneEffect {
  private readonly group = new Group();
  private readonly unit = new PlaneGeometry(1, 1);
  private readonly clock = { value: 0 };
  private readonly frame: EffectFrame;
  private startMs: number | null;
  private landed = false;

  constructor(
    private readonly host: EffectHost,
    name: string,
    private readonly timing: EffectTiming,
    look: (parts: EffectParts) => EffectFrame,
  ) {
    this.group.name = name;
    this.startMs = timing.startMs;
    this.frame = look({ group: this.group, unit: this.unit, clock: this.clock });
    host.scene.add(this.group);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = (nowMs - this.startMs) / 1000 / this.timing.pace;
    this.clock.value = t;
    this.frame(t);
    if (this.timing.impact && t >= this.timing.impact.atS) this.land();
    return t < this.timing.endS;
  }

  private land() {
    if (this.landed || !this.timing.impact) return;
    this.landed = true;
    this.timing.impact.land();
  }

  // An effect cut short (context loss, unmount) still lands once: the step
  // it belongs to plays on.
  dispose() {
    this.land();
    this.host.scene.remove(this.group);
    this.group.traverse((object) => {
      if (!(object instanceof Mesh)) return;
      if (object.geometry !== this.unit) object.geometry.dispose();
      (object.material as ShaderMaterial).dispose();
    });
    this.unit.dispose();
  }
}
