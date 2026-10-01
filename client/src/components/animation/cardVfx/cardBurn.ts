// A scorched permanent burns outward from the strike: paper curls and chars,
// flames rise in screen space, and embers leave along the same burn front.

import { type BufferGeometry, Group, MathUtils, Mesh, PlaneGeometry, ShaderMaterial, type Texture, Vector2 } from "three";

import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { BURN_KEY_GLSL, BURN_SPAN, CORNER_MASK_GLSL, SCORCH_GLSL } from "./glslChunks.ts";
import { ADDITIVE, count, FBM_GLSL, NORMAL, PARTICLE_SHARE, type Particle, particleLayer, RAMP_GLSL, rand } from "./vfxParticles.ts";

export const BURN_S = 0.9;
export const BURN_TAIL_S = 1;

const cardVert = /* glsl */ `
  ${FBM_GLSL}
  ${BURN_KEY_GLSL}
  uniform float uFront, uCurl, uChar;
  varying vec2 vCard, vUv;
  void main() {
    vUv = uv;
    vCard = vec2(uv.x * uSize.x, (1.0 - uv.y) * uSize.y);
    vec3 p = position;
    float d = burnKey(vCard) - uFront;
    p.z += uCurl * (d >= 0.0 ? 1.0 - smoothstep(0.0, 0.16, d) : 1.0 - 0.5 * smoothstep(0.0, uChar, -d));
    gl_Position = projectionMatrix * modelViewMatrix * vec4(p, 1.0);
  }`;

const cardFrag = /* glsl */ `
  uniform sampler2D uMap; uniform float uRadius, uFront, uScorch, uChar;
  varying vec2 vCard, vUv; 
  ${FBM_GLSL}
  ${BURN_KEY_GLSL}
  ${SCORCH_GLSL}
  ${CORNER_MASK_GLSL}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    float d = burnKey(vCard) - uFront;
    // Charred paper holds together for a while behind the front, then crumbles unevenly.
    float crumble = -uChar + (vnoise(vCard * 0.22 + 11.0) - 0.5) * 0.1;
    if (d < crumble) discard;
    vec3 col = texture2D(uMap, vUv).rgb;
    // Scorch where the fire hit: browned halo, charred core.
    float halo = 1.0 - smoothstep(0.0, 1.0, distance(vCard, uKeyImpact) / (uScorchR * 1.7));
    col = mix(col, col * vec3(0.62, 0.42, 0.26), halo * uScorch * 0.6);
    col = mix(col, vec3(0.012, 0.008, 0.006), scorchMask(vCard) * uScorch * 0.88);
    // Burn front: the paper browns, chars, then glows at the edge before it goes.
    col = mix(col, col * vec3(0.5, 0.33, 0.2), (1.0 - smoothstep(0.05, 0.22, d)) * 0.85);
    vec3 charCol = vec3(0.018, 0.013, 0.01) * (0.6 + 0.8 * vnoise(vCard * 0.35));
    col = mix(col, charCol, 1.0 - smoothstep(-0.01, 0.06, d));
    // Grey ash right before it falls apart.
    col = mix(col, vec3(0.1, 0.095, 0.09), (1.0 - smoothstep(crumble, crumble + 0.06, d)));
    col = mix(col, vec3(1.0, 0.25, 0.03), exp(-abs(d) * 90.0));
    gl_FragColor = vec4(col, corner);
    #include <colorspace_fragment>
  }`;

const fireVert = /* glsl */ `
  varying vec2 vWorld;
  void main() {
    vec4 w = modelMatrix * vec4(position, 1.0);
    vWorld = w.xy;
    gl_Position = projectionMatrix * viewMatrix * w;
  }`;

const fireFrag = /* glsl */ `
  uniform vec2 uCenter; uniform float uAngle, uFront, uBand, uFlameH, uGain, uTime;
  varying vec2 vWorld;
  ${FBM_GLSL}
  ${BURN_KEY_GLSL}
  // World (y-up) to card pixels (y-down), undoing the CSS rotate() of a tapped card.
  vec2 toCard(vec2 w) {
    vec2 s = vec2(w.x - uCenter.x, uCenter.y - w.y);
    float c = cos(uAngle), sn = sin(uAngle);
    return vec2(c * s.x + sn * s.y, -sn * s.x + c * s.y) + uSize * 0.5;
  }
  float fuel(vec2 w) {
    vec2 c = toCard(w);
    if (c.x < 0.0 || c.y < 0.0 || c.x > uSize.x || c.y > uSize.y) return 0.0;
    float d = burnKey(c) - uFront;
    return (1.0 - smoothstep(0.0, 0.03, d)) * smoothstep(-uBand, -uBand * 0.35, d);
  }
  vec3 blackbody(float f) {
    vec3 c = mix(vec3(0.0), vec3(0.5, 0.05, 0.0), smoothstep(0.0, 0.25, f));
    c = mix(c, vec3(1.0, 0.34, 0.03), smoothstep(0.2, 0.5, f));
    c = mix(c, vec3(1.0, 0.7, 0.24), smoothstep(0.5, 0.8, f));
    return mix(c, vec3(1.0, 0.93, 0.76), smoothstep(0.82, 1.0, f));
  }
  void main() {
    const int N = 28;
    float dh = uFlameH / float(N);
    float acc = 0.0;
    for (int i = 0; i < N; i++) {
      float hgt = float(i) * dh;
      float k = hgt / uFlameH;
      // Flames lean and wave more the higher they reach.
      float sway = (vnoise(vec2(vWorld.x * 0.012 + hgt * 0.008, uTime * 1.4)) - 0.5) * hgt * 0.7;
      acc = max(acc, fuel(vWorld - vec2(sway, hgt)) * pow(1.0 - k, 1.3));
    }
    if (acc <= 0.001) discard;
    vec2 q = vWorld * vec2(0.06, 0.032) - vec2(0.0, uTime * 2.6);
    q += (vec2(fbm(q + vec2(0.0, uTime * 0.9)), fbm(q * 1.3 + 5.2)) - 0.5) * 1.7;
    float n = fbm(q * 1.4);
    float f = clamp(acc * (0.25 + 1.1 * n) - 0.18, 0.0, 1.0);
    // Light scales with density too, so thin flame stays see-through instead of a solid sheet.
    gl_FragColor = vec4(blackbody(f) * (0.25 + 0.75 * f) * uGain, 0.0);
  }`;

const emberFrag = /* glsl */ `
  uniform float uRadius, uFront, uScorchGlow, uGain, uTime, uChar;
  varying vec2 vCard;
  ${FBM_GLSL}
  ${BURN_KEY_GLSL}
  ${SCORCH_GLSL}
  ${CORNER_MASK_GLSL}
  ${RAMP_GLSL}
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    float d = burnKey(vCard) - uFront;
    float band = exp(-abs(d) * 70.0);
    float flick = 0.7 + 0.6 * vnoise(vCard * 0.12 + vec2(uTime * 7.0, -uTime * 5.0));
    // Ember veins glow through the char and cool as it ages.
    float vein = (1.0 - smoothstep(0.0, 0.05, abs(fbm(vCard * 0.1 + 3.0) - 0.5))) * step(d, 0.0) * smoothstep(-uChar, 0.0, d);
    band += vein * 0.7;
    float m = scorchMask(vCard);
    vec3 e = ramp(0.8) * band * flick + ramp(0.65) * (m * (1.0 - m) * 4.0 + m * 0.35) * uScorchGlow * flick;
    gl_FragColor = vec4(e * uGain * corner, 0.0);
  }`;

export interface CardBurnParams {
  pose: CardPose;
  surface: Texture;
  radius: number;
  impact: { u: number; v: number };
  amount: number;
  tier: CardVfxTier;
  pace: number;
  startMs: number | null;
  onDone(): void;
}

function burnParts({ pose, surface, radius, impact, amount, tier }: CardBurnParams) {
  const { w, h } = pose;
  const a = MathUtils.degToRad(pose.angleDeg);
  const ix = impact.u * w, iy = impact.v * h;
  const scale = 0.8 + Math.min(amount, 8) * 0.07;
  const clock = { value: 0 };
  const key = {
    uSize: { value: new Vector2(w, h) },
    uKeyImpact: { value: new Vector2(ix, iy) },
    uKeyMax: { value: Math.hypot(Math.max(ix, w - ix), Math.max(iy, h - iy)) },
    uKeyMode: { value: 0 },
    uBurnStart: { value: 0 },
    uBurnS: { value: BURN_S },
  };
  const uniforms = {
    ...key, uImpact: key.uKeyImpact, uMap: { value: surface }, uRadius: { value: radius }, uTime: clock,
    uFront: { value: -0.08 }, uCurl: { value: 10 }, uChar: { value: 0.24 },
    uScorch: { value: 1 }, uScorchR: { value: Math.min(w, h) * 0.3 * Math.min(scale, 1.3) },
    uScorchGlow: { value: 0.2 }, uGain: { value: 1.195 }, uPalette: { value: 0 },
  };
  const group = new Group();
  group.name = "card-burn";
  group.position.set(pose.x, -pose.y, 0);
  group.rotation.z = -a;
  const geometry = new PlaneGeometry(w, h, 32, 44);
  const card = new Mesh(geometry, new ShaderMaterial({ vertexShader: cardVert, fragmentShader: cardFrag, uniforms, ...NORMAL }));
  const glow = new Mesh(geometry, new ShaderMaterial({ vertexShader: cardVert, fragmentShader: emberFrag, uniforms, ...ADDITIVE }));
  card.renderOrder = 1; glow.renderOrder = 2;
  group.add(card, glow);

  // The flames rise toward the screen's top even when the card is tapped.
  const flameH = h * 0.45;
  const bw = Math.abs(w * Math.cos(a)) + Math.abs(h * Math.sin(a)) + 32;
  const bh = Math.abs(w * Math.sin(a)) + Math.abs(h * Math.cos(a)) + 32 + flameH;
  const fire = new Mesh(new PlaneGeometry(bw, bh), new ShaderMaterial({
    vertexShader: fireVert, fragmentShader: fireFrag, ...ADDITIVE,
    uniforms: {
      ...key, uFront: uniforms.uFront, uTime: clock,
      uCenter: { value: new Vector2(pose.x, -pose.y) }, uAngle: { value: a },
      uBand: { value: 0.16 }, uFlameH: { value: flameH }, uGain: { value: 1.14 },
    },
  }));
  fire.name = "card-burn-flame";
  fire.position.set(pose.x, -pose.y + flameH / 2, 4);
  fire.renderOrder = 5;
  const embers: Particle[] = [], smoke: Particle[] = [];
  const up = new Vector2(-Math.sin(a), Math.cos(a));
  for (let i = 0; i < count(90, PARTICLE_SHARE[tier]); i++) {
    const x = rand(0, w), y = rand(0, h), ox = x - ix, oy = y - iy;
    const d = Math.hypot(ox, oy) || 1, out = rand(15, 60);
    embers.push({ pos: [x, -y, 2], vel: [ox / d * out + rand(-30, 30), -oy / d * out + rand(-30, 30), rand(40, 170)], spawn: rand(0, 0.05), life: rand(0.4, 1.1), drag: 1.6, s0: rand(1.6, 3.4), s1: rand(0.8, 1.8), heat: rand(0.8, 1) });
    if (i % 2 === 0) smoke.push({ pos: [x, -y, 4], vel: [up.x * rand(20, 50) + rand(-15, 15), up.y * rand(20, 50) + rand(-15, 15), rand(20, 60)], spawn: rand(0.1, 0.35), life: rand(0.9, 1.5), drag: 1.5, s0: rand(14, 22), s1: rand(55, 90) });
  }
  const extras = [
    particleLayer(smoke, "SMOKE", clock, { accZ: 30, gain: 0.34, order: 3, key }),
    particleLayer(embers, "EMBER", clock, { accZ: 140, gain: 1.31, cool: 1.3, order: 5, key }),
  ];
  for (const mesh of extras) { mesh.position.set(-w / 2, h / 2, 0); group.add(mesh); }
  for (const mesh of [card, glow, fire, ...extras]) mesh.frustumCulled = false;
  return { group, fire, clock, uniforms };
}

export const cardBurnKind: SceneEffectKind = {
  warmUp(host) {
    const { group, fire } = burnParts({
      pose: { x: 2, y: 2, w: 4, h: 4, angleDeg: 0 }, surface: host.placeholderTexture,
      radius: 0, impact: { u: 0.5, v: 0.5 }, amount: 3, tier: "reduced", pace: 1,
      startMs: null, onDone() {},
    });
    return [group, fire];
  },
};

class CardBurn implements SceneEffect {
  private readonly parts;
  private startMs: number | null;

  constructor(private readonly host: EffectHost, private readonly params: CardBurnParams) {
    this.parts = burnParts(params);
    this.startMs = params.startMs;
    host.scene.add(this.parts.group, this.parts.fire);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = Math.max(0, (nowMs - this.startMs) / 1000 / this.params.pace);
    this.parts.clock.value = t;
    this.parts.uniforms.uFront.value = -0.08 + t / BURN_S * BURN_SPAN;
    this.parts.uniforms.uScorchGlow.value = 0.2 * Math.exp(-t / 0.2);
    return t < BURN_S + BURN_TAIL_S;
  }

  dispose(silent: boolean) {
    const { group, fire } = this.parts;
    this.host.scene.remove(group, fire);
    const geometries = new Set<BufferGeometry>();
    group.traverse((object) => {
      if (!(object instanceof Mesh)) return;
      geometries.add(object.geometry);
      (object.material as ShaderMaterial).dispose();
    });
    for (const geometry of geometries) geometry.dispose();
    fire.geometry.dispose();
    fire.material.dispose();
    this.params.surface.dispose();
    if (!silent) this.params.onDone();
  }
}

export function createCardBurn(host: EffectHost, params: CardBurnParams): SceneEffect {
  return new CardBurn(host, params);
}
