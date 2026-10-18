// ─── Damage strike ───
// A spell or ability's damage travelling to its target, ported from the
// approved Damage lab (`fireCause`, `lightningCause`, `makeHit`): a thrown
// fireball or a forked bolt, an impact burst of flame, smoke and sparks, and —
// on a permanent — a copy of its surface knocked back and scorched where it
// was hit, which rocks back to rest. Light is colour written with alpha 0,
// which the browser adds over the DOM board, so no bloom pass is needed.
// `full` adds the light the fire casts on the board and the fireball's shadow.
// Blue damage throws an ice lance, which fractures into shards and cold mist;
// the struck copy carries a short-lived frost mark rather than a scorch.
// A creature's blow (`createDamageBlow`) lands where its DOM slam strikes,
// with no light of its own: a puff of dust and a pale shock ring; the struck
// permanent's copy (`createDamageKnockback`) rocks back from it, unmarked.

import {
  BufferGeometry,
  Float32BufferAttribute,
  Group,
  MathUtils,
  Mesh,
  type Object3D,
  PlaneGeometry,
  ShaderMaterial,
  type Texture,
  Vector2,
  Vector3,
} from "three";

import type { DamageCause } from "../../../animation/damageCause.ts";
import { DAMAGE_CAUSE_IMPACT_MS } from "../../../animation/types.ts";
import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { CORNER_MASK_GLSL, SCORCH_GLSL } from "./glslChunks.ts";
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
  place,
  RAMP_GLSL,
  rand,
  sprite,
  TimedEffect,
  type Vec2,
  type Vec3,
} from "./vfxParticles.ts";
import { jetGeometry, jetLength, jetPoint, MIST, WATER } from "./waterEffects.ts";

/** The impact, in seconds before pace: both causes land together. */
const IMPACT_S = DAMAGE_CAUSE_IMPACT_MS / 1000;
/** The fireball gathers at its source this long, then flies until the impact. */
export const FIRE_CHARGE_S = 0.14;
/** A bolt takes this long to draw from its source to its target. */
const STRIKE_REVEAL_S = 0.035;
/** Re-strikes after the first, each on a fresh path, in seconds. */
const RESTRIKES_S = [0, 0.075, 0.16];
/** The burst outlives the impact by this long, by cause. */
const TAIL_S: Record<DamageCause, number> = { fire: 1.4, lightning: 1.2, water: 1.1 };
/** A jet's tail runs into its target over this long after the impact. */
const WATER_DRAIN_S = 0.22;
/** The struck card rocks back to rest over this long after the impact. */
export const HIT_S = 0.8;
/** The lab's tuned glow, between the restrained look (0) and the reference (1). */
const GLOW = 0.85;

const bezier = (a: number, b: number, c: number, u: number) => (1 - u) * (1 - u) * a + 2 * (1 - u) * u * b + u * u * c;

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

// The card lying over the struck one (a slamming attacker, measured each
// frame) hides the copy wherever it covers it, as the DOM card would.
const occluderChunk = /* glsl */ `
  uniform vec2 uOccCenter, uOccHalf; uniform float uOccAngle, uOccRadius;
  varying vec2 vWorld;
  bool occluded() {
    if (uOccHalf.x <= 0.0) return false;
    vec2 d = vWorld - uOccCenter;
    float c = cos(uOccAngle), s = sin(uOccAngle);
    return roundedBox(vec2(c * d.x - s * d.y, s * d.x + c * d.y), uOccHalf, uOccRadius) < 0.0;
  }`;

const hitVert = /* glsl */ `
  uniform vec2 uSize, uPush; uniform float uTiltA, uSink; uniform vec3 uTiltAxis;
  varying vec2 vCard, vUv, vWorld; varying float vShade;
  vec3 rot(vec3 v, vec3 k, float a) { float c = cos(a), s = sin(a); return v * c + cross(k, v) * s + k * dot(k, v) * (1.0 - c); }
  void main() {
    vUv = uv;
    vCard = vec2(uv.x * uSize.x, (1.0 - uv.y) * uSize.y);
    vec3 p = position;
    p.z -= uSink;
    p = rot(p, uTiltAxis, uTiltA);
    p.xy += uPush;
    vWorld = (modelMatrix * vec4(p, 1.0)).xy;
    // Lit relative to rest, so the untouched copy matches the DOM card exactly.
    vec3 L = normalize(vec3(-0.35, 0.55, 0.76));
    vShade = 1.0 + 0.6 * (dot(rot(vec3(0.0, 0.0, 1.0), uTiltAxis, uTiltA), L) - L.z);
    gl_Position = projectionMatrix * modelViewMatrix * vec4(p, 1.0);
  }`;

// The mark where the damage hit: a browned halo around a charred core, or
// (FROST) a pale frozen patch with branching crystalline veins.
const hitFrag = /* glsl */ `
  uniform sampler2D uMap; uniform float uRadius, uScorch;
  varying vec2 vCard, vUv; varying float vShade;
  uniform vec2 uSize;
  ${FBM_GLSL}
  ${SCORCH_GLSL}
  ${CORNER_MASK_GLSL}
  ${occluderChunk}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0 || occluded()) discard;
    vec3 col = texture2D(uMap, vUv).rgb;
    #ifdef FROST
    vec2 d = vCard - uImpact;
    float a = atan(d.y, d.x);
    float veins = 1.0 - smoothstep(0.0, 0.09, abs(sin(a * 6.0 + length(d) * 0.035 + fbm(d * 0.07) * 0.6)));
    float frost = scorchMask(vCard) * uScorch;
    col = mix(col, vec3(0.58, 0.83, 0.94), frost * 0.45);
    col = mix(col, vec3(0.92, 0.98, 1.0), frost * veins * 0.85);
    #else
    float halo = 1.0 - smoothstep(0.0, 1.0, distance(vCard, uImpact) / (uScorchR * 1.7));
    col = mix(col, col * vec3(0.62, 0.42, 0.26), halo * uScorch * 0.6);
    col = mix(col, vec3(0.012, 0.008, 0.006), scorchMask(vCard) * uScorch * 0.88);
    #endif
    gl_FragColor = vec4(col * clamp(vShade, 0.3, 1.5), corner);
    #include <colorspace_fragment>
  }`;

// A second, additive pass: the light the fresh scorch gives off as it cools,
// or (FROST) light caught on the frozen patch's crystals.
const hitGlowFrag = /* glsl */ `
  uniform float uRadius, uScorch, uScorchGlow, uGain, uTime;
  uniform vec2 uSize;
  varying vec2 vCard;
  ${FBM_GLSL}
  ${SCORCH_GLSL}
  ${CORNER_MASK_GLSL}
  ${occluderChunk}
  ${RAMP_GLSL}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0 || occluded()) discard;
    float m = scorchMask(vCard);
    #ifdef FROST
    float sheen = smoothstep(0.68, 0.9, vnoise(vCard * 0.12)) * m;
    gl_FragColor = vec4(vec3(0.42, 0.78, 1.0) * sheen * uScorch * 0.45 * corner, 0.0);
    #else
    float flick = 0.7 + 0.6 * vnoise(vCard * 0.12 + vec2(uTime * 7.0, -uTime * 5.0));
    vec3 e = ramp(0.65) * (m * (1.0 - m) * 4.0 + m * 0.35) * uScorchGlow * flick;
    gl_FragColor = vec4(e * uGain * corner, 0.0);
    #endif
  }`;

function hitMaterials(
  surface: Texture,
  w: number,
  h: number,
  radius: number,
  impact: Vector2,
  scale: number,
  clock: { value: number },
  mark: DamageCause | null,
) {
  // Frost spreads over a wider patch than fire scorches.
  const frost = mark === "water";
  const defines = frost ? { FROST: "" } : {};
  const uniforms = {
    uMap: { value: surface },
    uSize: { value: new Vector2(w, h) },
    uRadius: { value: radius },
    uImpact: { value: impact },
    uScorchR: { value: Math.min(w, h) * (frost ? 0.42 : 0.3) * Math.min(scale, 1.3) },
    uScorch: { value: 0 },
    uScorchGlow: { value: 0 },
    uGain: { value: 0.6 + 0.7 * GLOW },
    uPalette: { value: 0 },
    uTime: clock,
    uTiltA: { value: 0 },
    uTiltAxis: { value: new Vector3(0, 1, 0) },
    uPush: { value: new Vector2() },
    uSink: { value: 0 },
    uOccCenter: { value: new Vector2() },
    uOccHalf: { value: new Vector2() },
    uOccAngle: { value: 0 },
    uOccRadius: { value: 0 },
  };
  return {
    uniforms,
    card: new ShaderMaterial({ vertexShader: hitVert, fragmentShader: hitFrag, defines, uniforms, ...NORMAL }),
    glow: new ShaderMaterial({ vertexShader: hitVert, fragmentShader: hitGlowFrag, defines, uniforms, ...ADDITIVE }),
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

/** A card lying over the struck one, and its corner radius, in canvas px. */
export interface Occluder {
  pose: CardPose;
  radius: number;
}

/** How a struck copy is hit. */
interface HitMotion {
  /** Where on the card it is hit, as fractions of its width and height. */
  impact: { u: number; v: number };
  /** The hit's direction, in world space. */
  dir: Vec2;
  scale: number;
  amount: number;
  pace: number;
  /** The mark the damage leaves; `null` (a creature's blow) leaves none. */
  mark: DamageCause | null;
  /** When the hit lands, in seconds after the copy starts, before pace. */
  impactS: number;
  /** When the copy starts, on the frame clock; `null` starts it on its first frame. */
  startMs: number | null;
  /** The card lying over this one now, if any. */
  occluder: ((origin: DOMRectReadOnly) => Occluder | null) | null;
  /** Runs once, on the frame the hit lands. */
  onImpact: (() => void) | null;
}

class DamageHit implements DamageHitEffect {
  private readonly group = new Group();
  private readonly geometry: PlaneGeometry;
  private readonly materials: ReturnType<typeof hitMaterials>;
  private readonly clock = { value: 0 };
  private readonly local: Vec2;
  private startMs: number | null;
  private atRest = false;
  private landed = false;

  constructor(
    private readonly host: EffectHost,
    private readonly params: DamageHitParams,
    private readonly motion: HitMotion,
  ) {
    const { pose, surface, radius } = params;
    const { impact, dir, scale, mark } = motion;
    this.startMs = motion.startMs;
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
      mark,
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
    const { amount, scale, pace, impactS, mark } = this.motion;
    const t = (nowMs - this.startMs) / 1000 / pace;
    this.clock.value = t;
    const k = t - impactS;
    this.group.visible = this.atRest || k >= 0;
    if (this.group.visible) this.occlude(this.materials.uniforms);
    if (k < 0) return true;
    if (!this.landed) {
      this.landed = true;
      this.motion.onImpact?.();
    }
    const U = this.materials.uniforms;
    // Knocked back along the damage's path: the far edge dips, then a damped rock back to rest.
    U.uTiltA.value = Math.min(0.1 + 0.03 * amount, 0.3) * Math.exp(-k * 8) * Math.sin(k * 22);
    const push = 9 * scale * (1 - Math.exp(-k * 40)) * Math.exp(-k * 6);
    U.uPush.value.set(this.local[0] * push, this.local[1] * push);
    U.uSink.value = 8 * scale * (1 - Math.exp(-k * 50)) * Math.exp(-k * 9);
    if (mark) {
      U.uScorch.value = Math.min(k / 0.05, 1) * (1 - MathUtils.smoothstep(k, 0.25, 0.75));
      U.uScorchGlow.value = Math.exp(-k / 0.2) * Math.min(k / 0.02, 1);
    }
    return k < HIT_S;
  }

  private occlude(U: ReturnType<typeof hitMaterials>["uniforms"]) {
    const over = this.motion.occluder?.(this.host.canvasOrigin()) ?? null;
    if (!over) {
      U.uOccHalf.value.set(0, 0);
      return;
    }
    const { pose, radius } = over;
    U.uOccCenter.value.set(pose.x, -pose.y);
    U.uOccHalf.value.set(pose.w / 2, pose.h / 2);
    U.uOccAngle.value = MathUtils.degToRad(pose.angleDeg);
    U.uOccRadius.value = radius;
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

interface CauseContext extends EffectParts {
  S: Vec2;
  T: Vec2;
  /** The target's size for the impact ring: a card's width, a HUD's height. */
  span: number;
  scale: number;
  share: number;
  boardLight: boolean;
}

// The fireball gathers at its source, flies a bowed path, and bursts on the target.
const fireTrailFrag = /* glsl */ `
  uniform float uHead, uTail, uTime; varying float vAcross, vAlong;
  ${FBM_GLSL}
  ${RAMP_GLSL}
  void main() {
    if (vAlong > uHead || vAlong < uTail) discard;
    float along = (vAlong - uTail) / max(uHead - uTail, 0.001);
    float churn = vnoise(vec2(vAlong * 80.0 - uTime * 18.0, vAcross * 3.0));
    float width = pow(along, 0.65) * (0.75 + 0.25 * churn);
    float edge = abs(vAcross) / max(width, 0.001);
    float body = 1.0 - smoothstep(0.35, 1.0, edge);
    float heat = mix(0.5, 1.0, along) * (1.0 - 0.35 * edge);
    gl_FragColor = vec4(ramp(heat) * body * smoothstep(0.0, 0.15, along), 0.0);
  }`;

function fireTrailMaterial(clock: { value: number }) {
  return new ShaderMaterial({
    vertexShader: boltVert,
    fragmentShader: fireTrailFrag,
    ...ADDITIVE,
    uniforms: { uHead: { value: 0 }, uTail: { value: 0 }, uTime: clock },
  });
}

function fireCause({ group, unit, clock, S, T, span, scale, share, boardLight }: CauseContext): EffectFrame {
  const dx = T[0] - S[0];
  const dy = T[1] - S[1];
  const dist = Math.hypot(dx, dy) || 1;
  const dir: Vec2 = [dx / dist, dy / dist];
  // Bow the path toward the top of the screen and lift it off the table.
  const up: Vec2 = dx >= 0 ? [-dir[1], dir[0]] : [dir[1], -dir[0]];
  const bow = dist * 0.1;
  const lift = 30 + dist * 0.07;
  const C: Vec2 = [(S[0] + T[0]) / 2 + up[0] * bow, (S[1] + T[1]) / 2 + up[1] * bow];
  const progress = (t: number) => clamp01((t - FIRE_CHARGE_S) / (IMPACT_S - FIRE_CHARGE_S)) ** 1.35;
  const heightAt = (u: number) => 14 * (1 - u) + Math.sin(Math.PI * u) * lift;
  const headAt = (t: number): Vec3 => {
    const u = progress(t);
    return [bezier(S[0], C[0], T[0], u), bezier(S[1], C[1], T[1], u), heightAt(u)];
  };
  // A continuous flame body shares the head's exact path and height. Shed
  // particles alone fade in behind a fast head and leave a visible gap.
  const path = { S, C, T };
  const trailSpan = Math.min(1, 120 * scale / jetLength(path));
  const trailGeometry = jetGeometry(path, 24 * scale);
  const positions = trailGeometry.getAttribute("position");
  const along = trailGeometry.getAttribute("aAlong");
  for (let i = 0; i < positions.count; i++) positions.setZ(i, heightAt(along.getX(i)));
  const trail = new Mesh(trailGeometry, fireTrailMaterial(clock));
  trail.renderOrder = 7;
  trail.frustumCulled = false;
  group.add(trail);

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
    trail.material.uniforms.uHead.value = progress(t);
    trail.material.uniforms.uTail.value = Math.max(0, progress(t) - trailSpan);
    trail.visible = t >= FIRE_CHARGE_S && flying;
    const [hx, hy, hz] = headAt(t);
    place(core, hx, hy, hz + 1, 42 * scale * (0.35 + 0.65 * kc), flying ? 1.1 * kc : 0);
    place(halo, hx, hy, hz, 80 * scale * (0.5 + 0.5 * kc), flying ? 0.35 * GLOW * kc : 0);
    if (headLight) place(headLight, hx, hy, 0, 180 * scale, flying ? 0.08 * GLOW * kc : 0);
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
function lightningCause({ group, unit, clock, S, T, span, scale, share, boardLight }: CauseContext): EffectFrame {
  const strikes = RESTRIKES_S.map((s) => IMPACT_S - STRIKE_REVEAL_S + s);
  const tStrike = strikes[0];
  const dist = Math.hypot(T[0] - S[0], T[1] - S[1]);
  const L: Layers = { flames: [], smoke: [], sparks: [] };
  burst(T, IMPACT_S, scale * 0.8, L, { core: count(6, share), fire: count(30, share), smoke: count(12, share), sparks: count(70 * scale, share) });
  group.add(
    particleLayer(L.smoke, "SMOKE", clock, { accZ: 40, gain: 0.3, order: 3 }),
    particleLayer(L.flames, "FLAME", clock, { accZ: 220, gain: 0.5 + 0.6 * GLOW, cool: 1.9, order: 5 }),
    particleLayer(L.sparks, "SPARK", clock, { accZ: -1600, gain: 0.9 + 0.5 * GLOW, palette: "lightning", order: 6 }),
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

const frostBoltFrag = /* glsl */ `
  uniform float uHead, uTail, uLength;
  varying float vAcross, vAlong;
  void main() {
    if (vAlong > uHead || vAlong < uTail) discard;
    float along = (vAlong - uTail) / max(uHead - uTail, 0.001);
    float width = min((1.0 - along) / 0.35, 1.0) * min(along / 0.6, 1.0);
    float across = abs(vAcross) / max(width, 0.001);
    if (across > 1.0) discard;
    vec3 col = mix(vec3(0.12, 0.4, 0.64), vec3(0.76, 0.94, 1.0), step(0.0, vAcross));
    float core = 1.0 - smoothstep(0.04, 0.15, across);
    col = mix(col, vec3(0.96, 1.0, 1.0), core);
    float vein = pow(max(0.0, sin(vAlong * uLength * 0.3 + abs(vAcross) * 12.0)), 16.0);
    col = mix(col, vec3(0.85, 0.97, 1.0), vein * 0.35);
    gl_FragColor = vec4(col, 0.95);
  }`;

function frostBoltMaterial(length: number) {
  return new ShaderMaterial({
    vertexShader: boltVert,
    fragmentShader: frostBoltFrag,
    ...NORMAL,
    uniforms: { uHead: { value: 0 }, uTail: { value: 0 }, uLength: { value: length } },
  });
}

// A compact ice lance leaves a cold vapour trail and fractures on impact.
function waterCause({ group, unit, clock, S, T, span, scale, share }: CauseContext): EffectFrame {
  const dx = T[0] - S[0];
  const dy = T[1] - S[1];
  const dist = Math.hypot(dx, dy) || 1;
  const dir: Vec2 = [dx / dist, dy / dist];
  const up: Vec2 = dx >= 0 ? [-dir[1], dir[0]] : [dir[1], -dir[0]];
  const bow = dist * 0.025;
  const path = { S, C: [(S[0] + T[0]) / 2 + up[0] * bow, (S[1] + T[1]) / 2 + up[1] * bow] as Vec2, T };
  const headAt = (t: number) => clamp01((t - FIRE_CHARGE_S) / (IMPACT_S - FIRE_CHARGE_S)) ** 1.15;
  const tailAt = (t: number) => {
    const tail = Math.max(0, headAt(t) - Math.min(1, 110 * scale / dist));
    return tail + (1 - tail) * clamp01((t - IMPACT_S) / WATER_DRAIN_S);
  };

  const drops: Particle[] = [];
  const mist: Particle[] = [];
  for (let i = 0; i < count(18, share); i++) {
    const a = rand(0, Math.PI * 2);
    const r = rand(14, 30) * scale;
    drops.push({ pos: [S[0] + Math.cos(a) * r, S[1] + Math.sin(a) * r, 8], vel: [-Math.cos(a) * r * 3, -Math.sin(a) * r * 3, 0], spawn: rand(0, FIRE_CHARGE_S * 0.7), life: rand(0.15, 0.22), drag: 3, s0: rand(2, 3.5) * scale });
  }
  // Small splinters and a narrow vapour wake shed off the lance.
  const nSpray = count(70, share);
  for (let i = 0; i < nSpray; i++) {
    const ts = FIRE_CHARGE_S + ((i + Math.random()) / nSpray) * (IMPACT_S - FIRE_CHARGE_S);
    const [px, py] = jetPoint(path, headAt(ts));
    const side = (Math.random() < 0.5 ? -1 : 1) * rand(15, 55);
    drops.push({ pos: [px, py, 12], vel: [up[0] * side - dir[0] * 35, up[1] * side - dir[1] * 35, rand(5, 30)], spawn: ts, life: rand(0.2, 0.4), drag: 4, s0: rand(3, 6) * scale });
    mist.push({ pos: [px, py, 5], vel: [up[0] * side * 0.2, up[1] * side * 0.2, 8], spawn: ts, life: 0.4, drag: 3, s0: 16 * scale, s1: 38 * scale });
  }
  for (let i = 0; i < count(85, share); i++) {
    const a = rand(0, Math.PI * 2);
    const speed = rand(140, 500) * scale;
    drops.push({ pos: [T[0], T[1], 8], vel: [Math.cos(a) * speed, Math.sin(a) * speed, rand(50, 180)], spawn: IMPACT_S + rand(0, 0.04), life: rand(0.35, 0.75), drag: 3, s0: rand(9, 20) * scale, s1: 2 });
    if (i % 2 === 0) mist.push({ pos: [T[0], T[1], 3], vel: [Math.cos(a) * 130, Math.sin(a) * 130, 12], spawn: IMPACT_S, life: 0.8, drag: 3, s0: 28 * scale, s1: 80 * scale });
  }
  group.add(
    particleLayer(mist, "SMOKE", clock, { accZ: 20, gain: 0.4, tint: MIST, order: 3 }),
    particleLayer(drops, "ICE", clock, { accZ: -600, gain: 1, tint: WATER, order: 6 }),
  );
  const jet = new Mesh(jetGeometry(path, 24 * scale), frostBoltMaterial(jetLength(path)));
  jet.renderOrder = 5;
  jet.frustumCulled = false;
  const swell = sprite(unit, "RIPPLE", [0.92, 0.96, 1], 4);
  const flash = sprite(unit, "GLOW", [0.65, 0.88, 1], 7);
  const rings = RIPPLE_RINGS_S.map(() => sprite(unit, "RING", [0.65, 0.88, 1], 4));
  group.add(jet, swell, flash, ...rings);
  const U = jet.material.uniforms;

  return (t) => {
    U.uHead.value = headAt(t);
    U.uTail.value = tailAt(t);
    jet.visible = t >= FIRE_CHARGE_S && U.uTail.value < 1;
    const kc = clamp01(t / FIRE_CHARGE_S);
    const gathered = 1 - clamp01((t - FIRE_CHARGE_S) / 0.2);
    place(swell, S[0], S[1], 2, (0.6 + 0.8 * kc) * 48 * scale, 0.6 * kc * gathered);
    const impactAge = t - IMPACT_S;
    place(flash, T[0], T[1], 14, 180 * scale, impactAge >= 0 ? 0.8 * Math.exp(-impactAge / 0.12) : 0);
    rings.forEach((ring, i) => {
      const rp = clamp01((t - IMPACT_S - RIPPLE_RINGS_S[i]) / 0.6);
      const on = t >= IMPACT_S + RIPPLE_RINGS_S[i] && rp < 1;
      place(ring, T[0], T[1], 2, (0.2 + 2.2 * (1 - (1 - rp) ** 2)) * span * scale, on ? (1 - rp) ** 2 * 0.7 : 0);
    });
  };
}

/** When each ring of ripples starts spreading from a jet's impact, after it. */
const RIPPLE_RINGS_S = [0, 0.09, 0.2];

const CAUSES: Record<DamageCause, (context: CauseContext) => EffectFrame> = {
  fire: fireCause,
  lightning: lightningCause,
  water: waterCause,
};

// ---------- The blow: a creature's slam landing ----------

/** How long a blow's dust hangs after its impact, before pace. */
const BLOW_TAIL_S = 1.2;

/** A blow's dust and grit: the dry grey-brown of the table. */
const DUST: Vec3 = [0.46, 0.41, 0.34];
const GRIT: Vec3 = [0.19, 0.16, 0.13];

interface BlowContext extends EffectParts {
  T: Vec2;
  /** The blow's direction, or `null` (a flurry of hits) to throw dust all round. */
  dir: Vec2 | null;
  span: number;
  scale: number;
  share: number;
  impactS: number;
}

// Dust thrown off the struck surface, most of it on along the blow, grit
// flicked out with it, and a pale ring where the shock runs out.
function blowFrame({ group, unit, clock, T, dir, span, scale, share, impactS }: BlowContext): EffectFrame {
  // Unit headings: any way round, or on along the blow within `spread`.
  const around = (): Vec2 => {
    const a = rand(0, Math.PI * 2);
    return [Math.cos(a), Math.sin(a)];
  };
  const along = (spread: number): Vec2 => {
    if (!dir) return around();
    const [ox, oy] = around();
    const x = dir[0] + ox * spread;
    const y = dir[1] + oy * spread;
    const length = Math.hypot(x, y) || 1;
    return [x / length, y / length];
  };
  const dust: Particle[] = [];
  for (let i = 0; i < count(40, share); i++) {
    // Every other mote rings the impact; the rest follow the blow.
    const [hx, hy] = i % 2 === 0 ? along(0.7) : around();
    const speed = rand(50, 210) * scale;
    dust.push({ pos: [T[0] + rand(-6, 6), T[1] + rand(-6, 6), 4], vel: [hx * speed, hy * speed, rand(10, 60)], spawn: impactS + rand(0, 0.05), life: rand(0.6, 1.1), drag: rand(3, 4.5), s0: rand(8, 14) * scale, s1: rand(38, 70) * scale });
  }
  const grit: Particle[] = [];
  for (let i = 0; i < count(24, share); i++) {
    const [hx, hy] = along(0.9);
    const speed = rand(180, 420) * scale;
    grit.push({ pos: [T[0], T[1], 4], vel: [hx * speed, hy * speed, 0], spawn: impactS + rand(0, 0.03), life: rand(0.25, 0.45), drag: 5, s0: rand(2, 3.5) * scale, s1: rand(1.5, 2.5) * scale });
  }
  group.add(
    particleLayer(dust, "SMOKE", clock, { accZ: 30, gain: 0.55, tint: DUST, order: 3 }),
    particleLayer(grit, "SMOKE", clock, { accZ: 0, gain: 0.9, tint: GRIT, order: 4 }),
  );
  const ring = sprite(unit, "RING", [0.92, 0.88, 0.8], 6);
  group.add(ring);

  return (t) => {
    const k = t - impactS;
    const rp = clamp01(k / 0.3);
    place(ring, T[0], T[1], 2, (0.5 + 2 * (1 - (1 - rp) ** 3)) * span * scale, k >= 0 && rp < 1 ? (1 - rp) ** 2 * 0.4 : 0);
  };
}

/** Creates a strike and, on a permanent, its hit. They share one clock start:
 *  the scene adds both in the same frame. */
export function createDamageStrike(
  host: EffectHost,
  { hit, ...params }: DamageStrikeParams,
): { strike: SceneEffect; hit: DamageHitEffect | null; impact: { u: number; v: number } } {
  // The hit lands somewhere central on a card, and in the middle of a HUD.
  const impact = hit ? { u: rand(0.35, 0.65), v: rand(0.3, 0.55) } : { u: 0.5, v: 0.5 };
  const S = worldPoint(params.from, 0.5, 0.5);
  const T = worldPoint(params.to, impact.u, impact.v);
  const scale = hit ? 0.8 + Math.min(params.amount, 8) * 0.07 : 0.7;
  const dist = Math.hypot(T[0] - S[0], T[1] - S[1]) || 1;
  const dir: Vec2 = [(T[0] - S[0]) / dist, (T[1] - S[1]) / dist];
  const { cause, to, tier, pace, onImpact } = params;
  const timing = { endS: IMPACT_S + TAIL_S[cause], pace, impact: { atS: IMPACT_S, land: onImpact }, startMs: null };
  const look = (parts: EffectParts) =>
    CAUSES[cause]({
      ...parts,
      S,
      T,
      span: Math.min(to.w, to.h),
      scale: params.cause === "lightning" ? scale : scale * 1.65,
      share: PARTICLE_SHARE[tier],
      boardLight: tier === "full",
    });
  return {
    strike: new TimedEffect(host, "damage-strike", timing, look),
    impact,
    hit:
      hit &&
      new DamageHit(host, hit, {
        impact,
        dir,
        scale,
        amount: params.amount,
        pace: params.pace,
        mark: cause,
        impactS: IMPACT_S,
        startMs: null,
        occluder: null,
        onImpact: null,
      }),
  };
}

export interface DamageBlowParams {
  /** The striking creature's surface; `null` (a flurry of hits) has no one direction. */
  from: CardPose | null;
  /** The struck surface: the creature, or the player's HUD. */
  to: CardPose;
  amount: number;
  tier: CardVfxTier;
  pace: number;
  /** When the slam started, on the frame clock (`performance.now()`). */
  startMs: number;
  /** When the slam lands, in seconds after it started, before pace. */
  impactS: number;
}

/** Creates a creature's blow landing on `to` as its slam strikes. The slam
 *  lands the hit itself, so the blow has no impact of its own to report. */
export function createDamageBlow(host: EffectHost, { from, to, amount, tier, pace, startMs, impactS }: DamageBlowParams): SceneEffect {
  const T = worldPoint(to, 0.5, 0.5);
  const S = from && worldPoint(from, 0.5, 0.5);
  const dist = S ? Math.hypot(T[0] - S[0], T[1] - S[1]) : 0;
  const dir: Vec2 | null = S && dist > 0 ? [(T[0] - S[0]) / dist, (T[1] - S[1]) / dist] : null;
  const timing = { endS: impactS + BLOW_TAIL_S, pace, impact: null, startMs };
  const look = (parts: EffectParts) =>
    blowFrame({
      ...parts,
      T,
      dir,
      span: Math.min(to.w, to.h),
      scale: 0.8 + Math.min(amount, 8) * 0.07,
      share: PARTICLE_SHARE[tier],
      impactS,
    });
  return new TimedEffect(host, "damage-blow", timing, look);
}

export interface DamageKnockbackParams {
  /** The struck permanent. */
  hit: DamageHitParams;
  /** The striking creature where its slam set out; `null` has no direction. */
  from: CardPose | null;
  /** The striking creature as it lies over the struck one, measured each frame. */
  occluder: ((origin: DOMRectReadOnly) => Occluder | null) | null;
  amount: number;
  pace: number;
  /** When the slam started, on the frame clock (`performance.now()`). */
  startMs: number;
  /** When the slam lands, in seconds after it started, before pace. */
  impactS: number;
  /** Runs once, on the frame the slam lands. */
  onImpact(): void;
}

/** Creates the copy of a permanent a creature's slam strikes: hidden until the
 *  slam lands, then rocking back from the blow, unmarked. */
export function createDamageKnockback(host: EffectHost, params: DamageKnockbackParams): DamageHitEffect {
  const { hit, from, amount } = params;
  const T = worldPoint(hit.pose, 0.5, 0.5);
  const S = from ? worldPoint(from, 0.5, 0.5) : T;
  const dist = Math.hypot(T[0] - S[0], T[1] - S[1]);
  // With no direction, it is pressed straight down the screen.
  const dir: Vec2 = dist > 0 ? [(T[0] - S[0]) / dist, (T[1] - S[1]) / dist] : [0, -1];
  return new DamageHit(host, hit, {
    impact: { u: 0.5, v: 0.5 },
    dir,
    scale: 0.8 + Math.min(amount, 8) * 0.07,
    amount,
    pace: params.pace,
    mark: null,
    impactS: params.impactS,
    startMs: params.startMs,
    occluder: params.occluder,
    onImpact: params.onImpact,
  });
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
      particleLayer(dead, "DROP", clock, { accZ: 0, gain: 0, order: 6 }),
      particleLayer(dead, "ICE", clock, { accZ: 0, gain: 0, order: 6 }),
      sprite(unit, "GLOW", [0, 0, 0], 8),
      sprite(unit, "RING", [0, 0, 0], 6),
      sprite(unit, "SHADOW", [0, 0, 0], 0),
      sprite(unit, "RIPPLE", [0, 0, 0], 4),
      new Mesh(boltGeometry([0, 0], [1, 0]), boltMaterial()),
      new Mesh(jetGeometry({ S: [0, 0], C: [1, 0], T: [2, 0] }, 1), fireTrailMaterial(clock)),
      new Mesh(jetGeometry({ S: [0, 0], C: [1, 0], T: [2, 0] }, 1), frostBoltMaterial(2)),
    ];
    for (const cause of ["fire", "water"] as const) {
      const { card, glow } = hitMaterials(host.placeholderTexture, 2, 2, 0, new Vector2(), 1, clock, cause);
      objects.push(new Mesh(unit, card), new Mesh(unit, glow));
    }
    objects.forEach((object, i) => {
      object.name = `damage-strike-warmup-${i}`;
    });
    return objects;
  },
};
