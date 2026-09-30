// ─── Damage strike ───
// A spell or ability's damage travelling to its target, ported from the
// approved Damage lab (`fireCause`, `lightningCause`, `makeHit`): a thrown
// fireball or a forked bolt, an impact burst of flame, smoke and sparks, and —
// on a permanent — a copy of its surface knocked back and scorched where it
// was hit, which rocks back to rest. Light is colour written with alpha 0,
// which the browser adds over the DOM board, so no bloom pass is needed.
// `full` adds the light the fire casts on the board and the fireball's shadow.

import {
  AddEquation,
  BufferGeometry,
  CustomBlending,
  Float32BufferAttribute,
  Group,
  InstancedBufferAttribute,
  InstancedBufferGeometry,
  MathUtils,
  Mesh,
  type Object3D,
  OneFactor,
  PlaneGeometry,
  ShaderMaterial,
  type Texture,
  Vector2,
  Vector3,
  ZeroFactor,
} from "three";

import type { DamageCause } from "../../../animation/damageCause.ts";
import { DAMAGE_CAUSE_IMPACT_MS } from "../../../animation/types.ts";
import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { CORNER_MASK_GLSL, VALUE_NOISE_GLSL } from "./glslChunks.ts";

/** The impact, in seconds before pace: both causes land together. */
const IMPACT_S = DAMAGE_CAUSE_IMPACT_MS / 1000;
/** The fireball gathers at its source this long, then flies until the impact. */
export const FIRE_CHARGE_S = 0.14;
/** A bolt takes this long to draw from its source to its target. */
const STRIKE_REVEAL_S = 0.035;
/** Re-strikes after the first, each on a fresh path, in seconds. */
const RESTRIKES_S = [0, 0.075, 0.16];
/** The burst outlives the impact by this long, by cause. */
const TAIL_S: Record<DamageCause, number> = { fire: 1.4, lightning: 1.2 };
/** The struck card rocks back to rest over this long after the impact. */
export const HIT_S = 0.8;
/** The lab's tuned glow, between the restrained look (0) and the reference (1). */
const GLOW = 0.85;
/** The share of the lab's particle counts each tier emits. */
export const PARTICLE_SHARE: Record<CardVfxTier, number> = { full: 1, reduced: 0.5 };

type Vec3 = [number, number, number];
type Vec2 = [number, number];

const ADDITIVE = {
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
const NORMAL = { transparent: true, depthWrite: false, depthTest: false } as const;

const rand = (lo: number, hi: number) => lo + Math.random() * (hi - lo);
const clamp01 = (x: number) => MathUtils.clamp(x, 0, 1);
const bezier = (a: number, b: number, c: number, u: number) => (1 - u) * (1 - u) * a + 2 * (1 - u) * u * b + u * u * c;

const noiseChunk = /* glsl */ `
  ${VALUE_NOISE_GLSL}
  float fbm(vec2 p) { return vnoise(p) * 0.55 + vnoise(p * 2.1 + 3.7) * 0.3 + vnoise(p * 4.3 + 9.1) * 0.15; }`;

// Fire cools white → yellow → orange → deep red; lightning cools white → blue.
const rampChunk = /* glsl */ `
  uniform float uPalette;
  vec3 ramp(float h) {
    h = clamp(h, 0.0, 1.0);
    if (uPalette > 0.5) {
      vec3 e = mix(vec3(0.08, 0.1, 0.35), vec3(0.35, 0.55, 1.0), smoothstep(0.0, 0.5, h));
      return mix(e, vec3(0.92, 0.96, 1.0), smoothstep(0.55, 1.0, h));
    }
    vec3 c = mix(vec3(0.3, 0.03, 0.01), vec3(1.0, 0.32, 0.04), smoothstep(0.0, 0.45, h));
    c = mix(c, vec3(1.0, 0.72, 0.28), smoothstep(0.4, 0.75, h));
    return mix(c, vec3(1.0, 0.96, 0.84), smoothstep(0.78, 1.0, h));
  }`;

// ---------- Particles: one instanced quad each, all motion on the GPU ----------

type ParticleKind = "FLAME" | "SPARK" | "SMOKE";

interface Particle {
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

interface ParticleLook {
  accZ: number;
  gain: number;
  cool?: number;
  palette?: number;
  order: number;
}

const particleVert = /* glsl */ `
  attribute vec3 aPos; attribute vec3 aVel; attribute vec4 aTime; attribute vec4 aLook;
  uniform float uTime, uAccZ;
  varying vec2 vUv; varying float vA, vHeat, vSeed;
  void main() {
    float age = uTime - aTime.x;
    float a = age / aTime.y;
    if (age < 0.0 || a >= 1.0) { gl_Position = vec4(0.0, 0.0, 2.0, 1.0); return; }
    float drag = aTime.z;
    vec3 p = aPos + aVel * (1.0 - exp(-drag * age)) / drag;
    p.z += 0.5 * uAccZ * age * age;
    #ifdef SPARK
    p.z = max(p.z, 0.0);
    vec3 v = aVel * exp(-drag * age) + vec3(0.0, 0.0, uAccZ * age);
    vec2 dir = length(v.xy) > 0.001 ? normalize(v.xy) : vec2(1.0, 0.0);
    float len = max(length(v.xy) * aLook.w, aLook.x);
    vec3 local = p + vec3(dir * (position.x - 0.5) * len + vec2(-dir.y, dir.x) * position.y * aLook.x, 0.0);
    #else
    float s = mix(aLook.x, aLook.y, 1.0 - (1.0 - a) * (1.0 - a));
    float ang = aTime.w * 6.2831 + age * (aTime.w - 0.5) * 3.0;
    vec3 local = p + vec3(mat2(cos(ang), sin(ang), -sin(ang), cos(ang)) * position.xy * s, 0.0);
    #endif
    vUv = position.xy + 0.5; vA = a; vHeat = aLook.z; vSeed = aTime.w;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(local, 1.0);
  }`;

const particleFrag = /* glsl */ `
  uniform float uGain, uCool; uniform vec3 uSmoke;
  varying vec2 vUv; varying float vA, vHeat, vSeed;
  ${noiseChunk}
  ${rampChunk}
  void main() {
    vec2 q = vUv * 2.0 - 1.0;
    #if defined(SPARK)
    float across = 1.0 - smoothstep(0.1, 1.0, abs(q.y));
    float along = smoothstep(0.0, 1.0, vUv.x);
    gl_FragColor = vec4(ramp(vHeat * (1.0 - 0.6 * vA)) * across * along * (1.0 - vA * vA) * uGain, 0.0);
    #elif defined(SMOKE)
    float r = length(q) + (vnoise(q * 2.2 + vSeed * 37.0) - 0.5) * 0.5;
    float fade = smoothstep(0.0, 0.2, vA) * (1.0 - smoothstep(0.4, 1.0, vA));
    gl_FragColor = vec4(uSmoke, (1.0 - smoothstep(0.3, 1.0, r)) * fade * uGain);
    #else
    float r = length(q) + (vnoise(q * 2.4 + vSeed * 37.0 + vA * 2.5) - 0.5) * 0.7;
    float shape = pow(max(1.0 - r, 0.0), 1.5);
    float fade = smoothstep(0.0, 0.1, vA) * (1.0 - vA);
    gl_FragColor = vec4(ramp(vHeat * exp(-vA * uCool)) * shape * fade * uGain, 0.0);
    #endif
  }`;

// FLAME and SPARK are additive light; SMOKE is ordinary alpha.
function particleLayer(
  list: readonly Particle[],
  kind: ParticleKind,
  clock: { value: number },
  { accZ, gain, cool = 1.6, palette = 0, order }: ParticleLook,
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
      defines: { [kind]: "" },
      uniforms: {
        uTime: clock,
        uAccZ: { value: accZ },
        uGain: { value: gain },
        uCool: { value: cool },
        uPalette: { value: palette },
        uSmoke: { value: new Vector3(0.09, 0.075, 0.065) },
      },
      ...(kind === "SMOKE" ? NORMAL : ADDITIVE),
    }),
  );
  mesh.frustumCulled = false;
  mesh.renderOrder = order;
  return mesh;
}

// ---------- Sprites: the few large lights the CPU moves each frame ----------

type SpriteKind = "GLOW" | "RING" | "SHADOW";

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
    #else
    float a = (exp(-r * r * 6.0) + 0.6 * exp(-r * 14.0)) * (1.0 - smoothstep(0.8, 1.0, r));
    gl_FragColor = vec4(uColor * a * uIntensity, 0.0);
    #endif
  }`;

function sprite(unit: PlaneGeometry, kind: SpriteKind, color: Vec3, order: number): Mesh<PlaneGeometry, ShaderMaterial> {
  const mesh = new Mesh(
    unit,
    new ShaderMaterial({
      vertexShader: spriteVert,
      fragmentShader: spriteFrag,
      defines: kind === "GLOW" ? {} : { [kind]: "" },
      uniforms: { uColor: { value: new Vector3(...color) }, uIntensity: { value: 0 } },
      ...(kind === "SHADOW" ? NORMAL : ADDITIVE),
    }),
  );
  mesh.renderOrder = order;
  mesh.frustumCulled = false;
  mesh.visible = false;
  return mesh;
}

function place(mesh: Mesh<PlaneGeometry, ShaderMaterial>, x: number, y: number, z: number, size: number, intensity: number) {
  mesh.visible = intensity > 0.002;
  if (!mesh.visible) return;
  mesh.position.set(x, y, z);
  mesh.scale.setScalar(size);
  mesh.material.uniforms.uIntensity.value = intensity;
}

// ---------- Lightning bolt ----------

const boltVert = /* glsl */ `
  attribute float aAcross, aAlong; varying float vAcross, vAlong;
  void main() { vAcross = aAcross; vAlong = aAlong; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

const boltFrag = /* glsl */ `
  uniform float uIntensity, uReveal; uniform vec3 uGlow;
  varying float vAcross, vAlong;
  void main() {
    if (vAlong > uReveal) discard;
    float v = abs(vAcross);
    float core = 1.0 - smoothstep(0.06, 0.16, v);
    float glow = exp(-v * 4.0) * (1.0 - smoothstep(0.7, 1.0, v));
    gl_FragColor = vec4((uGlow * glow + vec3(core)) * uIntensity, 0.0);
  }`;

/** Midpoint displacement: split each segment, push the midpoint sideways, halve the push. */
function jag(a: Vec2, b: Vec2, rough: number, depth: number): Vec2[] {
  let pts = [a, b];
  let amp = Math.hypot(b[0] - a[0], b[1] - a[1]) * rough;
  for (let d = 0; d < depth; d++) {
    const next: Vec2[] = [pts[0]];
    for (let i = 1; i < pts.length; i++) {
      const [x0, y0] = pts[i - 1];
      const [x1, y1] = pts[i];
      const l = Math.hypot(x1 - x0, y1 - y0) || 1;
      const off = (Math.random() - 0.5) * amp;
      next.push([(x0 + x1) / 2 - ((y1 - y0) / l) * off, (y0 + y1) / 2 + ((x1 - x0) / l) * off], pts[i]);
    }
    pts = next;
    amp *= 0.55;
  }
  return pts;
}

/** Mitred triangle strips for the main bolt and a few branches; `aAlong` lets
 *  the first strike draw from the source to the target. */
function boltGeometry(S: Vec2, T: Vec2): BufferGeometry {
  const main = jag(S, T, 0.22, 6);
  const lines = [{ pts: main, w: 15, start: 0, span: 1, taper: false }];
  const len = Math.hypot(T[0] - S[0], T[1] - S[1]);
  for (let b = 0, nb = 2 + Math.floor(Math.random() * 3); b < nb; b++) {
    const i = Math.floor(rand(0.15, 0.75) * (main.length - 1));
    const p = main[i];
    const q = main[i + 1];
    const ang = Math.atan2(q[1] - p[1], q[0] - p[0]) + (Math.random() < 0.5 ? -1 : 1) * rand(0.35, 0.9);
    const bl = len * rand(0.1, 0.28);
    lines.push({
      pts: jag(p, [p[0] + Math.cos(ang) * bl, p[1] + Math.sin(ang) * bl], 0.3, 4),
      w: 8,
      start: i / (main.length - 1),
      span: 0.15,
      taper: true,
    });
  }
  const pos: number[] = [];
  const across: number[] = [];
  const along: number[] = [];
  for (const { pts, w, start, span, taper } of lines) {
    const acc = [0];
    for (let i = 1; i < pts.length; i++) {
      acc.push(acc[i - 1] + Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]));
    }
    const total = acc[acc.length - 1] || 1;
    const side = pts.map((p, i) => {
      const p0 = pts[Math.max(i - 1, 0)];
      const p1 = pts[Math.min(i + 1, pts.length - 1)];
      const tx = p1[0] - p0[0];
      const ty = p1[1] - p0[1];
      const tl = Math.hypot(tx, ty) || 1;
      const f = acc[i] / total;
      const ww = w * (taper ? 1 - f * 0.85 : 1 - f * 0.25);
      return {
        l: [p[0] - (ty / tl) * ww, p[1] + (tx / tl) * ww] as Vec2,
        r: [p[0] + (ty / tl) * ww, p[1] - (tx / tl) * ww] as Vec2,
        s: start + f * span,
      };
    });
    for (let i = 0; i < pts.length - 1; i++) {
      const A = side[i];
      const B = side[i + 1];
      const quad: [Vec2, number, number][] = [
        [A.l, 1, A.s], [A.r, -1, A.s], [B.l, 1, B.s],
        [A.r, -1, A.s], [B.r, -1, B.s], [B.l, 1, B.s],
      ];
      for (const [p, c, s] of quad) {
        pos.push(p[0], p[1], 0);
        across.push(c);
        along.push(s);
      }
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new Float32BufferAttribute(pos, 3));
  geometry.setAttribute("aAcross", new Float32BufferAttribute(across, 1));
  geometry.setAttribute("aAlong", new Float32BufferAttribute(along, 1));
  return geometry;
}

function boltMaterial() {
  return new ShaderMaterial({
    vertexShader: boltVert,
    fragmentShader: boltFrag,
    ...ADDITIVE,
    uniforms: { uIntensity: { value: 0 }, uReveal: { value: 0 }, uGlow: { value: new Vector3(0.45, 0.55, 1.0) } },
  });
}

// ---------- The struck card: knockback and scorch ----------

const scorchChunk = /* glsl */ `
  uniform vec2 uImpact; uniform float uScorchR;
  float scorchMask(vec2 c) {
    float r = distance(c, uImpact) / uScorchR;
    return 1.0 - smoothstep(0.3, 1.0, r + (fbm(c * 0.09 + 7.0) - 0.5) * 0.7);
  }`;

const hitVert = /* glsl */ `
  uniform vec2 uSize, uPush; uniform float uTiltA, uSink; uniform vec3 uTiltAxis;
  varying vec2 vCard, vUv; varying float vShade;
  vec3 rot(vec3 v, vec3 k, float a) { float c = cos(a), s = sin(a); return v * c + cross(k, v) * s + k * dot(k, v) * (1.0 - c); }
  void main() {
    vUv = uv;
    vCard = vec2(uv.x * uSize.x, (1.0 - uv.y) * uSize.y);
    vec3 p = position;
    p.z -= uSink;
    p = rot(p, uTiltAxis, uTiltA);
    p.xy += uPush;
    // Lit relative to rest, so the untouched copy matches the DOM card exactly.
    vec3 L = normalize(vec3(-0.35, 0.55, 0.76));
    vShade = 1.0 + 0.6 * (dot(rot(vec3(0.0, 0.0, 1.0), uTiltAxis, uTiltA), L) - L.z);
    gl_Position = projectionMatrix * modelViewMatrix * vec4(p, 1.0);
  }`;

// The scorch where the damage hit: a browned halo around a charred core.
const hitFrag = /* glsl */ `
  uniform sampler2D uMap; uniform float uRadius, uScorch;
  varying vec2 vCard, vUv; varying float vShade;
  uniform vec2 uSize;
  ${noiseChunk}
  ${scorchChunk}
  ${CORNER_MASK_GLSL}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    vec3 col = texture2D(uMap, vUv).rgb;
    float halo = 1.0 - smoothstep(0.0, 1.0, distance(vCard, uImpact) / (uScorchR * 1.7));
    col = mix(col, col * vec3(0.62, 0.42, 0.26), halo * uScorch * 0.6);
    col = mix(col, vec3(0.012, 0.008, 0.006), scorchMask(vCard) * uScorch * 0.88);
    gl_FragColor = vec4(col * clamp(vShade, 0.3, 1.5), corner);
    #include <colorspace_fragment>
  }`;

// A second, additive pass: the light the fresh scorch gives off as it cools.
const hitGlowFrag = /* glsl */ `
  uniform float uRadius, uScorchGlow, uGain, uTime;
  uniform vec2 uSize;
  varying vec2 vCard;
  ${noiseChunk}
  ${scorchChunk}
  ${CORNER_MASK_GLSL}
  ${rampChunk}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    float flick = 0.7 + 0.6 * vnoise(vCard * 0.12 + vec2(uTime * 7.0, -uTime * 5.0));
    float m = scorchMask(vCard);
    vec3 e = ramp(0.65) * (m * (1.0 - m) * 4.0 + m * 0.35) * uScorchGlow * flick;
    gl_FragColor = vec4(e * uGain * corner, 0.0);
  }`;

function hitMaterials(surface: Texture, w: number, h: number, radius: number, impact: Vector2, scale: number, clock: { value: number }) {
  const uniforms = {
    uMap: { value: surface },
    uSize: { value: new Vector2(w, h) },
    uRadius: { value: radius },
    uImpact: { value: impact },
    uScorchR: { value: Math.min(w, h) * 0.3 * Math.min(scale, 1.3) },
    uScorch: { value: 0 },
    uScorchGlow: { value: 0 },
    uGain: { value: 0.6 + 0.7 * GLOW },
    uPalette: { value: 0 },
    uTime: clock,
    uTiltA: { value: 0 },
    uTiltAxis: { value: new Vector3(0, 1, 0) },
    uPush: { value: new Vector2() },
    uSink: { value: 0 },
  };
  return {
    uniforms,
    card: new ShaderMaterial({ vertexShader: hitVert, fragmentShader: hitFrag, uniforms, ...NORMAL }),
    glow: new ShaderMaterial({ vertexShader: hitVert, fragmentShader: hitGlowFrag, uniforms, ...ADDITIVE }),
  };
}

/** The permanent a strike hits: its surface where it lies, and what runs when
 *  its copy has rocked back to rest. */
export interface DamageHitParams {
  pose: CardPose;
  surface: Texture;
  radius: number;
  onDone(): void;
}

export interface DamageStrikeParams {
  cause: DamageCause;
  /** The source's surface: the permanent, or the stack entry. */
  from: CardPose;
  /** The target's surface: the permanent, or the player's HUD. */
  to: CardPose;
  /** The struck permanent; `null` for a player. */
  hit: DamageHitParams | null;
  amount: number;
  tier: CardVfxTier;
  pace: number;
  /** The hit lands; exactly once, early if the strike is cut short. */
  onImpact(): void;
}

/** A point of `pose` at fractions (u, v) of its unrotated box, in world coordinates. */
function worldPoint(pose: CardPose, u: number, v: number): Vec2 {
  const a = MathUtils.degToRad(pose.angleDeg);
  const ox = (u - 0.5) * pose.w;
  const oy = (v - 0.5) * pose.h;
  return [pose.x + ox * Math.cos(a) - oy * Math.sin(a), -(pose.y + ox * Math.sin(a) + oy * Math.cos(a))];
}

/** A struck permanent's copy. It stays hidden until the impact unless it
 *  takes over from a copy already showing, whose veil it inherits. */
export interface DamageHitEffect extends SceneEffect {
  /** Shows the copy at rest from now until its own impact. */
  showAtRest(): void;
}

class DamageHit implements DamageHitEffect {
  private readonly group = new Group();
  private readonly geometry: PlaneGeometry;
  private readonly materials: ReturnType<typeof hitMaterials>;
  private readonly clock = { value: 0 };
  private readonly local: Vec2;
  private startMs: number | null = null;
  private atRest = false;

  constructor(
    private readonly host: EffectHost,
    private readonly params: DamageHitParams,
    impact: { u: number; v: number },
    dir: Vec2,
    private readonly scale: number,
    private readonly amount: number,
    private readonly pace: number,
  ) {
    const { pose, surface, radius } = params;
    const a = MathUtils.degToRad(pose.angleDeg);
    // The group turns by −a, so the hit direction in card space is the world direction turned by +a.
    this.local = [dir[0] * Math.cos(a) - dir[1] * Math.sin(a), dir[0] * Math.sin(a) + dir[1] * Math.cos(a)];
    this.materials = hitMaterials(
      surface,
      pose.w,
      pose.h,
      radius,
      new Vector2(impact.u * pose.w, impact.v * pose.h),
      scale,
      this.clock,
    );
    this.materials.uniforms.uTiltAxis.value.set(-this.local[1], this.local[0], 0);
    this.geometry = new PlaneGeometry(pose.w, pose.h);
    const card = new Mesh(this.geometry, this.materials.card);
    const glow = new Mesh(this.geometry, this.materials.glow);
    card.renderOrder = 1;
    glow.renderOrder = 2;
    this.group.name = "damage-hit";
    this.group.position.set(pose.x, -pose.y, 0);
    this.group.rotation.z = -a;
    this.group.visible = false;
    this.group.add(card, glow);
    host.scene.add(this.group);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = (nowMs - this.startMs) / 1000 / this.pace;
    this.clock.value = t;
    const k = t - IMPACT_S;
    this.group.visible = this.atRest || k >= 0;
    if (k < 0) return true;
    const U = this.materials.uniforms;
    // Knocked back along the damage's path: the far edge dips, then a damped rock back to rest.
    U.uTiltA.value = Math.min(0.1 + 0.03 * this.amount, 0.3) * Math.exp(-k * 8) * Math.sin(k * 22);
    const push = 9 * this.scale * (1 - Math.exp(-k * 40)) * Math.exp(-k * 6);
    U.uPush.value.set(this.local[0] * push, this.local[1] * push);
    U.uSink.value = 8 * this.scale * (1 - Math.exp(-k * 50)) * Math.exp(-k * 9);
    U.uScorch.value = Math.min(k / 0.05, 1) * (1 - MathUtils.smoothstep(k, 0.25, 0.75));
    U.uScorchGlow.value = Math.exp(-k / 0.2) * Math.min(k / 0.02, 1);
    return k < HIT_S;
  }

  showAtRest() {
    this.atRest = true;
    this.group.visible = true;
  }

  dispose(silent: boolean) {
    this.host.scene.remove(this.group);
    this.geometry.dispose();
    this.materials.card.dispose();
    this.materials.glow.dispose();
    this.params.surface.dispose();
    if (!silent) this.params.onDone();
  }
}

// ---------- The strike: the cause's travel and the impact burst ----------

interface Layers {
  flames: Particle[];
  smoke: Particle[];
  sparks: Particle[];
}

interface BurstCounts {
  core: number;
  fire: number;
  smoke: number;
  sparks: number;
}

function burst(T: Vec2, ti: number, scale: number, L: Layers, n: BurstCounts) {
  const radial = (lo: number, hi: number): Vec3 => {
    const a = rand(0, Math.PI * 2);
    return [Math.cos(a), Math.sin(a), rand(lo, hi) * scale];
  };
  for (let i = 0; i < n.core; i++) {
    const [cx, cy, s] = radial(40, 140);
    L.flames.push({ pos: [T[0], T[1], 8], vel: [cx * s, cy * s, rand(20, 80)], spawn: ti + rand(0, 0.02), life: rand(0.12, 0.2), drag: 4, s0: rand(28, 40) * scale, s1: rand(60, 95) * scale, heat: 1 });
  }
  for (let i = 0; i < n.fire; i++) {
    const [cx, cy, s] = radial(200, 640);
    L.flames.push({ pos: [T[0] + cx * rand(0, 8), T[1] + cy * rand(0, 8), 8], vel: [cx * s, cy * s, rand(60, 320)], spawn: ti + rand(0, 0.04), life: rand(0.25, 0.55), drag: rand(5, 7), s0: rand(9, 16) * scale, s1: rand(26, 54) * scale, heat: rand(0.75, 1) });
  }
  for (let i = 0; i < n.smoke; i++) {
    const [cx, cy, s] = radial(70, 220);
    L.smoke.push({ pos: [T[0] + cx * rand(0, 14), T[1] + cy * rand(0, 14), 10], vel: [cx * s, cy * s, rand(20, 80)], spawn: ti + rand(0.05, 0.14), life: rand(0.7, 1.2), drag: 3, s0: rand(20, 30) * scale, s1: rand(70, 110) * scale });
  }
  for (let i = 0; i < n.sparks; i++) {
    const [cx, cy, s] = radial(450, 1100);
    L.sparks.push({ pos: [T[0], T[1], 6], vel: [cx * s, cy * s, rand(120, 420)], spawn: ti + rand(0, 0.03), life: rand(0.35, 0.7), drag: 2.2, s0: rand(1.4, 2.4), heat: rand(0.85, 1), stretch: 0.03 });
  }
}

/** What a cause lends the strike: its per-frame update, given time in seconds. */
type CauseFrame = (t: number) => void;

interface CauseContext {
  group: Group;
  unit: PlaneGeometry;
  clock: { value: number };
  S: Vec2;
  T: Vec2;
  /** The target's size for the impact ring: a card's width, a HUD's height. */
  span: number;
  scale: number;
  share: number;
  boardLight: boolean;
}

const count = (n: number, share: number) => Math.round(n * share);

// The fireball gathers at its source, flies a bowed path, and bursts on the target.
function fireCause({ group, unit, clock, S, T, span, scale, share, boardLight }: CauseContext): CauseFrame {
  const dx = T[0] - S[0];
  const dy = T[1] - S[1];
  const dist = Math.hypot(dx, dy) || 1;
  const dir: Vec2 = [dx / dist, dy / dist];
  // Bow the path toward the top of the screen and lift it off the table.
  const up: Vec2 = dx >= 0 ? [-dir[1], dir[0]] : [dir[1], -dir[0]];
  const bow = dist * 0.1;
  const lift = 30 + dist * 0.07;
  const C: Vec2 = [(S[0] + T[0]) / 2 + up[0] * bow, (S[1] + T[1]) / 2 + up[1] * bow];
  const headAt = (t: number): Vec3 => {
    const u = clamp01((t - FIRE_CHARGE_S) / (IMPACT_S - FIRE_CHARGE_S)) ** 1.35;
    return [bezier(S[0], C[0], T[0], u), bezier(S[1], C[1], T[1], u), 14 * (1 - u) + Math.sin(Math.PI * u) * lift];
  };

  const L: Layers = { flames: [], smoke: [], sparks: [] };
  for (let i = 0; i < count(26, share); i++) {
    const a = rand(0, Math.PI * 2);
    const r = rand(0, 10) * scale;
    L.flames.push({ pos: [S[0] + Math.cos(a) * r, S[1] + Math.sin(a) * r, 10], vel: [-Math.cos(a) * r * 2, -Math.sin(a) * r * 2, rand(10, 40)], spawn: rand(0, FIRE_CHARGE_S), life: rand(0.1, 0.2), drag: 3, s0: rand(4, 8) * scale, s1: rand(10, 18) * scale, heat: rand(0.85, 1) });
  }
  // The head sheds the trail, so each particle starts where the head is at its
  // spawn time. Flames die fast (a short hot tail); their smoke carries the long trail.
  const nTrail = count(200, share);
  for (let i = 0; i < nTrail; i++) {
    const ts = FIRE_CHARGE_S + ((i + Math.random()) / nTrail) * (IMPACT_S - FIRE_CHARGE_S);
    const [px, py, pz] = headAt(ts);
    const back = rand(40, 110);
    L.flames.push({ pos: [px + rand(-3, 3), py + rand(-3, 3), pz], vel: [-dir[0] * back + rand(-40, 40), -dir[1] * back + rand(-40, 40), rand(0, 50)], spawn: ts, life: rand(0.08, 0.18), drag: 3.5, s0: rand(11, 16) * scale, s1: rand(16, 24) * scale, heat: rand(0.85, 1) });
    // No smoke over the source card itself.
    if (i % 2 === 0 && i > nTrail * 0.2) {
      L.smoke.push({ pos: [px, py, pz], vel: [rand(-25, 25), rand(-25, 25), rand(10, 40)], spawn: ts + 0.05, life: rand(0.5, 0.9), drag: 2, s0: 10 * scale, s1: rand(30, 46) * scale });
    }
  }
  burst(T, IMPACT_S, scale, L, { core: count(14, share), fire: count(90 + 30 * scale, share), smoke: count(30, share), sparks: count(50 * scale, share) });
  group.add(
    particleLayer(L.smoke, "SMOKE", clock, { accZ: 40, gain: 0.34, order: 3 }),
    particleLayer(L.flames, "FLAME", clock, { accZ: 220, gain: 0.55 + 0.75 * GLOW, cool: 1.7, order: 5 }),
    particleLayer(L.sparks, "SPARK", clock, { accZ: -1600, gain: 0.8 + 0.5 * GLOW, order: 6 }),
  );
  const core = sprite(unit, "GLOW", [1, 0.86, 0.6], 8);
  const halo = sprite(unit, "GLOW", [1, 0.42, 0.1], 8);
  const flash = sprite(unit, "GLOW", [1, 0.72, 0.4], 7);
  const ring = sprite(unit, "RING", [1, 0.8, 0.58], 6);
  group.add(core, halo, flash, ring);
  const headLight = boardLight ? sprite(unit, "GLOW", [1, 0.45, 0.12], 4) : null;
  const headShadow = boardLight ? sprite(unit, "SHADOW", [0, 0, 0], 0) : null;
  const light = boardLight ? sprite(unit, "GLOW", [1, 0.45, 0.14], 4) : null;
  for (const mesh of [headLight, headShadow, light]) if (mesh) group.add(mesh);

  return (t) => {
    const kc = clamp01(t / FIRE_CHARGE_S);
    const flying = t < IMPACT_S;
    const [hx, hy, hz] = headAt(t);
    place(core, hx, hy, hz + 1, 24 * scale * (0.35 + 0.65 * kc) * rand(0.9, 1.1), flying ? 1.1 * kc : 0);
    place(halo, hx, hy, hz, 130 * scale * (0.5 + 0.5 * kc), flying ? 0.5 * GLOW * kc : 0);
    if (headLight) place(headLight, hx, hy, 0, 420 * scale, flying ? 0.14 * GLOW * kc : 0);
    if (headShadow) place(headShadow, hx + 0.32 * hz, hy - 0.42 * hz, 0, 30 * scale + hz * 0.25, flying ? 0.28 * kc : 0);
    const k = t - IMPACT_S;
    const env = k < 0 ? 0 : k < 0.03 ? k / 0.03 : Math.exp(-(k - 0.03) / 0.09);
    place(flash, T[0], T[1], 24, 320 * scale, env * (0.35 + 0.9 * GLOW));
    if (light) place(light, T[0], T[1], 0, 760 * scale, k >= 0 ? 0.32 * GLOW * Math.exp(-k / 0.35) * Math.min(k / 0.03, 1) : 0);
    const rp = clamp01(k / 0.34);
    place(ring, T[0], T[1], 2, (0.5 + 2.6 * (1 - (1 - rp) ** 3)) * span * scale, k >= 0 && rp < 1 ? (1 - rp) ** 2 * (0.35 + 0.65 * GLOW) : 0);
  };
}

// The source crackles, then a bolt strikes the target and re-strikes twice on
// fresh paths, like the flicker of a real discharge.
function lightningCause({ group, unit, clock, S, T, span, scale, share, boardLight }: CauseContext): CauseFrame {
  const strikes = RESTRIKES_S.map((s) => IMPACT_S - STRIKE_REVEAL_S + s);
  const tStrike = strikes[0];
  const dist = Math.hypot(T[0] - S[0], T[1] - S[1]);
  const L: Layers = { flames: [], smoke: [], sparks: [] };
  burst(T, IMPACT_S, scale * 0.8, L, { core: count(6, share), fire: count(30, share), smoke: count(12, share), sparks: count(70 * scale, share) });
  group.add(
    particleLayer(L.smoke, "SMOKE", clock, { accZ: 40, gain: 0.3, order: 3 }),
    particleLayer(L.flames, "FLAME", clock, { accZ: 220, gain: 0.5 + 0.6 * GLOW, cool: 1.9, order: 5 }),
    particleLayer(L.sparks, "SPARK", clock, { accZ: -1600, gain: 0.9 + 0.5 * GLOW, palette: 1, order: 6 }),
  );
  const material = boltMaterial();
  const bolt = new Mesh(new BufferGeometry(), material);
  bolt.position.z = 10;
  bolt.renderOrder = 7;
  bolt.frustumCulled = false;
  const spark = sprite(unit, "GLOW", [0.6, 0.72, 1], 8);
  const flash = sprite(unit, "GLOW", [0.75, 0.85, 1], 7);
  const ring = sprite(unit, "RING", [0.8, 0.9, 1], 6);
  group.add(bolt, spark, flash, ring);
  const light = boardLight ? sprite(unit, "GLOW", [0.5, 0.6, 1], 4) : null;
  const sky = boardLight ? sprite(unit, "GLOW", [0.45, 0.5, 0.9], 4) : null;
  for (const mesh of [light, sky]) if (mesh) group.add(mesh);

  let strikeIndex = -1;
  return (t) => {
    const index = strikes.filter((s) => t >= s).length - 1;
    if (index !== strikeIndex) {
      strikeIndex = index;
      bolt.geometry.dispose();
      bolt.geometry = boltGeometry(S, T);
    }
    const env = index < 0 ? 0 : Math.exp(-(t - strikes[index]) / 0.05);
    material.uniforms.uIntensity.value = env * (0.75 + 0.5 * GLOW) * rand(0.85, 1.15);
    material.uniforms.uReveal.value = clamp01((t - tStrike) / STRIKE_REVEAL_S);
    bolt.visible = env > 0.01;
    place(spark, S[0], S[1], 12, 70 * scale, t < tStrike ? Math.random() * 0.9 * (t / tStrike) : env * 0.8);
    const k = t - IMPACT_S;
    const fenv = k < 0 ? 0 : k < 0.02 ? k / 0.02 : Math.exp(-(k - 0.02) / 0.08);
    place(flash, T[0], T[1], 24, 300 * scale, Math.max(fenv, k >= 0 ? env * 0.6 : 0) * (0.35 + 0.9 * GLOW));
    if (light) place(light, T[0], T[1], 0, 700 * scale, k >= 0 ? 0.3 * GLOW * Math.exp(-k / 0.3) : 0);
    if (sky) place(sky, (S[0] + T[0]) / 2, (S[1] + T[1]) / 2, 0, dist * 1.6, 0.1 * GLOW * env);
    const rp = clamp01(k / 0.3);
    place(ring, T[0], T[1], 2, (0.5 + 2.2 * (1 - (1 - rp) ** 3)) * span * scale, k >= 0 && rp < 1 ? (1 - rp) ** 2 * (0.3 + 0.6 * GLOW) : 0);
  };
}

const CAUSES: Record<DamageCause, (context: CauseContext) => CauseFrame> = {
  fire: fireCause,
  lightning: lightningCause,
};

class DamageStrike implements SceneEffect {
  private readonly group = new Group();
  private readonly unit = new PlaneGeometry(1, 1);
  private readonly clock = { value: 0 };
  private readonly frame: CauseFrame;
  private startMs: number | null = null;
  private impacted = false;

  constructor(
    private readonly host: EffectHost,
    private readonly params: Omit<DamageStrikeParams, "hit">,
    S: Vec2,
    T: Vec2,
    scale: number,
  ) {
    const { cause, to, tier } = params;
    this.group.name = "damage-strike";
    this.frame = CAUSES[cause]({
      group: this.group,
      unit: this.unit,
      clock: this.clock,
      S,
      T,
      span: Math.min(to.w, to.h),
      scale,
      share: PARTICLE_SHARE[tier],
      boardLight: tier === "full",
    });
    host.scene.add(this.group);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = (nowMs - this.startMs) / 1000 / this.params.pace;
    this.clock.value = t;
    this.frame(t);
    if (t >= IMPACT_S) this.land();
    return t < IMPACT_S + TAIL_S[this.params.cause];
  }

  private land() {
    if (this.impacted) return;
    this.impacted = true;
    this.params.onImpact();
  }

  // A strike cut short (context loss, unmount) still lands its hit once: the
  // step it belongs to plays on.
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

/** Creates a strike and, on a permanent, its hit. They share one clock start:
 *  the scene adds both in the same frame. */
export function createDamageStrike(
  host: EffectHost,
  { hit, ...params }: DamageStrikeParams,
): { strike: SceneEffect; hit: DamageHitEffect | null } {
  // The hit lands somewhere central on a card, and in the middle of a HUD.
  const impact = hit ? { u: rand(0.35, 0.65), v: rand(0.3, 0.55) } : { u: 0.5, v: 0.5 };
  const S = worldPoint(params.from, 0.5, 0.5);
  const T = worldPoint(params.to, impact.u, impact.v);
  const scale = hit ? 0.8 + Math.min(params.amount, 8) * 0.07 : 0.7;
  const dist = Math.hypot(T[0] - S[0], T[1] - S[1]) || 1;
  const dir: Vec2 = [(T[0] - S[0]) / dist, (T[1] - S[1]) / dist];
  return {
    strike: new DamageStrike(host, params, S, T, scale),
    hit: hit && new DamageHit(host, hit, impact, dir, scale, params.amount, params.pace),
  };
}

export const damageStrikeKind: SceneEffectKind = {
  // Every program a strike or hit can use, built as they build them.
  warmUp(host) {
    const clock = { value: 0 };
    const unit = new PlaneGeometry(1, 1);
    const dead: Particle[] = [{ pos: [0, 0, 0], vel: [0, 0, 0], spawn: -2, life: 1, drag: 1, s0: 0 }];
    const objects: Object3D[] = [
      particleLayer(dead, "FLAME", clock, { accZ: 0, gain: 0, order: 5 }),
      particleLayer(dead, "SPARK", clock, { accZ: 0, gain: 0, order: 6 }),
      particleLayer(dead, "SMOKE", clock, { accZ: 0, gain: 0, order: 3 }),
      sprite(unit, "GLOW", [0, 0, 0], 8),
      sprite(unit, "RING", [0, 0, 0], 6),
      sprite(unit, "SHADOW", [0, 0, 0], 0),
      new Mesh(boltGeometry([0, 0], [1, 0]), boltMaterial()),
    ];
    const { card, glow } = hitMaterials(host.placeholderTexture, 2, 2, 0, new Vector2(), 1, clock);
    objects.push(new Mesh(unit, card), new Mesh(unit, glow));
    objects.forEach((object, i) => {
      object.name = `damage-strike-warmup-${i}`;
    });
    return objects;
  },
};
