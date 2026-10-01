// ─── Exile dissolve ───
// An exiled permanent lifts off the board and pales, then a front crosses it
// and releases it as flakes of its own colours, ported from the approved Exile
// lab (`exile()`). A card exiled out of the game drifts away up the screen; one
// held by a permanent (linked exile) flies flake by flake to its ghost under
// that permanent, tucking under the holder where the holder covers it.

import {
  BufferGeometry,
  Float32BufferAttribute,
  MathUtils,
  Mesh,
  PlaneGeometry,
  Points,
  ShaderMaterial,
  type Texture,
  Vector2,
  Vector3,
  Vector4,
} from "three";

import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { ROUNDED_BOX_GLSL, VALUE_NOISE_GLSL } from "./glslChunks.ts";

/** The lift off the board, in seconds before pace. */
export const DISSOLVE_LIFT_S = 0.22;
/** The front crossing the card, in seconds before pace. */
export const DISSOLVE_CROSS_S = 0.56;
/** How long a flake travels, in seconds before pace. */
export const DISSOLVE_TRAVEL_S = 0.62;
export const DISSOLVE_FLAKES: Record<CardVfxTier, number> = { full: 4200, reduced: 2000 };
const LIFT_PX = 14;
const PALE = 0.4;
const EDGE = 0.6;
/** A held card's ghost shows once most of its flakes have arrived. */
const GHOST_ARRIVAL = 0.85;
const OFFSCREEN = new Vector4(-1e4, 0, 0, 0);

const smooth = (a: number, b: number, x: number) => {
  const t = Math.min(Math.max((x - a) / (b - a), 0), 1);
  return t * t * (3 - 2 * t);
};

// The dissolve order lives in one GLSL function, used by both the card (which
// pixels are gone) and the flakes (when each one is released), so they agree.
const dissolveChunk = /* glsl */ `
  uniform vec2 uFirst; uniform float uAspect, uSacrifice;
  ${VALUE_NOISE_GLSL}
  float fbm(vec2 p) { return vnoise(p) * 0.55 + vnoise(p * 2.1 + 7.3) * 0.3 + vnoise(p * 4.3 + 1.7) * 0.15; }
  // 0 = released first, 1 = released last.
  float dissolveKey(vec2 uv) {
    vec2 a = vec2(uAspect, 1.0);
    if (uSacrifice > 0.5) return clamp(1.0 - length((uv - 0.5) * a) / length(a * 0.5) + (fbm(uv * a * 9.0) - 0.5) * 0.22, 0.0, 1.0);
    float along = 0.5 - dot(uv - 0.5, uFirst) / (abs(uFirst.x) + abs(uFirst.y));
    return clamp(along * 0.72 + fbm(uv * a * 6.0) * 0.28, 0.0, 1.0);
  }`;

const plainVert = /* glsl */ `
  varying vec2 vUv;
  void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

const cardFrag = /* glsl */ `
  uniform sampler2D uMap; uniform vec2 uSize; uniform float uRadius, uProg, uPale, uEdge;
  varying vec2 vUv;
  ${dissolveChunk}
  ${ROUNDED_BOX_GLSL}
  void main() {
    vec2 p = (vUv - 0.5) * uSize;
    float corner = clamp(0.5 - roundedBox(p, uSize * 0.5, uRadius), 0.0, 1.0);
    float gap = dissolveKey(vUv) - uProg;
    if (corner <= 0.0 || gap < 0.0) discard;
    vec3 col = texture2D(uMap, vUv).rgb;
    if (uSacrifice > 0.5) {
      col *= 1.0 - uPale * 1.8;
      float edge = (1.0 - smoothstep(0.0, 0.045, gap)) * step(0.0001, uProg);
      col = mix(col, vec3(0.7, 0.12, 0.32), edge);
      gl_FragColor = vec4(col, corner);
      #include <colorspace_fragment>
      return;
    }
    vec3 pale = vec3(0.78, 0.8, 0.84);
    float grey = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = mix(col, mix(vec3(grey), pale, 0.5), uPale);
    // Ahead of the front the card thins toward pale; the front itself is a hairline.
    col = mix(col, pale, (1.0 - smoothstep(0.0, 0.07, gap)) * 0.6 * step(0.0001, uProg));
    col = mix(col, vec3(0.92, 0.94, 0.97), (1.0 - smoothstep(0.0, 0.012, gap)) * uEdge * step(0.0001, uProg));
    gl_FragColor = vec4(col, corner);
    #include <colorspace_fragment>
  }`;

const shadowFrag = /* glsl */ `
  uniform vec2 uSize; uniform float uRadius, uBlur, uAlpha;
  varying vec2 vUv;
  ${ROUNDED_BOX_GLSL}
  void main() {
    vec2 p = (vUv - 0.5) * (uSize + 2.0 * uBlur);
    gl_FragColor = vec4(0.0, 0.0, 0.0, uAlpha * (1.0 - smoothstep(-uBlur, uBlur, roundedBox(p, uSize * 0.5, uRadius))));
  }`;

const vortexFrag = /* glsl */ `
  uniform float uTime, uAlpha;
  varying vec2 vUv;
  ${VALUE_NOISE_GLSL}
  void main() {
    vec2 p = (vUv - 0.5) * 2.0;
    float r = length(p);
    if (r > 1.0) discard;
    float a = atan(p.y, p.x);
    float spiral = pow(0.5 + 0.5 * sin(a * 3.0 + r * 22.0 + uTime * 10.0), 5.0);
    float rim = exp(-pow((r - 0.72) * 18.0, 2.0));
    float curl = spiral * smoothstep(0.2, 0.6, r) * (1.0 - smoothstep(0.72, 1.0, r));
    vec3 col = vec3(0.035, 0.006, 0.02) + vec3(0.5, 0.045, 0.13) * (rim + curl * 0.7);
    gl_FragColor = vec4(col, (1.0 - smoothstep(0.8, 1.0, r)) * uAlpha);
    #include <colorspace_fragment>
  }`;

function createVortexMaterial() {
  return new ShaderMaterial({
    vertexShader: plainVert,
    fragmentShader: vortexFrag,
    transparent: true,
    depthWrite: false,
    depthTest: false,
    uniforms: { uTime: { value: 0 }, uAlpha: { value: 0 } },
  });
}

// Flakes: one point per sample of the card. Each is released when the front
// passes it, takes the card's colour at its spot, then drifts away or flies to
// the same spot on the ghost card.
const flakeVert = /* glsl */ `
  attribute vec2 aUv; attribute vec3 aRand;
  uniform sampler2D uMap; uniform vec2 uSize; uniform float uT, uCross, uTravel, uLift, uDpr, uHeld, uAngle, uGhostScale, uGhostAngle, uHolderAngle;
  uniform vec3 uCardPos; uniform vec3 uGhostPos; uniform vec4 uHolder;
  varying vec3 vCol; varying float vA;
  ${dissolveChunk}
  // Counter-clockwise by a (world y up), as a mesh's rotation.z turns.
  vec2 turn(vec2 v, float a) { float c = cos(a), s = sin(a); return vec2(c * v.x - s * v.y, s * v.x + c * v.y); }
  void main() {
    float birth = (dissolveKey(aUv) + 0.03) / 1.06 * uCross;
    float age = uT - birth;
    float life = uTravel * (0.7 + 0.6 * aRand.z);
    float k = clamp(age / life, 0.0, 1.0);
    vec2 offset = (aUv - 0.5) * uSize;
    vec3 start = uCardPos + vec3(turn(offset, uAngle), uLift);
    vec3 pos;
    float alpha;
    if (uHeld > 0.5) {
      // Arc to the same spot on the ghost card, lifting over the board on the way.
      vec3 end = uGhostPos + vec3(turn(offset * uGhostScale, uGhostAngle), 0.0);
      float e = k * k * (3.0 - 2.0 * k);
      vec3 mid = mix(start, end, 0.5) + vec3((aRand.x - 0.5) * 28.0, (aRand.y - 0.5) * 20.0, 80.0 + aRand.z * 50.0);
      pos = mix(mix(start, mid, e), mix(mid, end, e), e);
      alpha = 1.0;
      // Flakes landing where the holder card covers the ghost tuck under it.
      vec2 d = abs(turn(pos.xy - uHolder.xy, -uHolderAngle)) - uHolder.zw;
      float inside = 1.0 - smoothstep(-3.0, 1.0, max(d.x, d.y));
      alpha *= 1.0 - inside * smoothstep(0.55, 0.9, k);
      alpha *= 1.0 - smoothstep(0.9, 1.0, k) * 0.9;
    } else if (uSacrifice > 0.5) {
      // The card's released fragments spiral inward and disappear into the table.
      float angle = k * 4.0;
      vec2 curled = turn(turn(offset, uAngle), angle) * pow(1.0 - k, 1.8);
      pos = uCardPos + vec3(curled, uLift * (1.0 - k) - k * 24.0);
      alpha = 1.0 - smoothstep(0.5, 1.0, k);
    } else {
      // Drift up the screen and toward the viewer, with a slow sideways sway.
      float a = age;
      pos = start + vec3(sin(a * 4.0 + aRand.x * 6.28) * 7.0 * a + (aRand.x - 0.5) * 110.0 * a,
                         (55.0 + aRand.y * 95.0) * a,
                         (90.0 + aRand.z * 180.0) * a);
      alpha = 0.85 * (1.0 - smoothstep(0.25, 1.0, k));
    }
    vec3 col = texture2D(uMap, aUv).rgb;
    vCol = mix(col, vec3(0.86, 0.88, 0.92), uHeld > 0.5 ? 0.15 : smoothstep(0.2, 0.9, k) * 0.5);
    if (uSacrifice > 0.5) vCol = mix(col * 0.4, vec3(0.9, 0.18, 0.36), smoothstep(0.0, 0.7, k));
    vA = age > 0.0 && k < 1.0 ? alpha : 0.0;
    gl_PointSize = vA > 0.0 ? (1.1 + aRand.y * 1.2) * uDpr * (uHeld > 0.5 ? 1.0 : 1.0 - 0.5 * k) : 0.0;
    gl_Position = projectionMatrix * viewMatrix * vec4(pos, 1.0);
  }`;

const flakeFrag = /* glsl */ `
  varying vec3 vCol; varying float vA;
  void main() {
    vec2 q = abs(gl_PointCoord - 0.5);
    if (q.x + q.y > 0.62) discard;
    gl_FragColor = vec4(vCol, vA);
    #include <colorspace_fragment>
  }`;

/** Where a held card's flakes go this frame: its ghost once the ghost exists,
 *  and the holder, which the flakes tuck under. `null` poses are not on screen. */
export interface LinkAim {
  ghost: CardPose | null;
  holder: CardPose | null;
}

export interface ExileDissolveParams {
  look: "exile" | "sacrifice";
  /** The permanent's pose on the board: its layout size and rotation. */
  pose: CardPose;
  /** The permanent's surface, drawn at its layout size. */
  surface: Texture;
  /** The corner radius of the surface, in CSS px. */
  radius: number;
  /** For a card held in exile by a permanent, where its flakes go, measured
   *  each frame; `null` for a card exiled out of the game. */
  link: ((origin: DOMRectReadOnly) => LinkAim) | null;
  tier: CardVfxTier;
  pace: number;
  pixelRatio: number;
  /** Runs once, when the card may show where it went: a held card's ghost as
   *  its flakes arrive, a card out of the game when its last flake is gone.
   *  A silent dispose does not run it. */
  onArrive(): void;
}

function flakeGeometry(count: number, aspect: number): BufferGeometry {
  // Flake samples on a jittered grid over the card.
  const cols = Math.max(1, Math.round(Math.sqrt(count * aspect)));
  const rows = Math.max(1, Math.round(count / cols));
  const uvs: number[] = [];
  const random: number[] = [];
  for (let j = 0; j < rows; j++) {
    for (let i = 0; i < cols; i++) {
      uvs.push((i + Math.random()) / cols, (j + Math.random()) / rows);
      random.push(Math.random(), Math.random(), Math.random());
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new Float32BufferAttribute(new Float32Array((uvs.length / 2) * 3), 3));
  geometry.setAttribute("aUv", new Float32BufferAttribute(uvs, 2));
  geometry.setAttribute("aRand", new Float32BufferAttribute(random, 3));
  return geometry;
}

function dissolveUniforms(w: number, h: number, first: Vector2) {
  return { uSacrifice: { value: 0 }, uFirst: { value: first }, uAspect: { value: w / h }, uSize: { value: new Vector2(w, h) } };
}

function createCardMaterial(surface: Texture, w: number, h: number, radius: number, first: Vector2) {
  return new ShaderMaterial({
    vertexShader: plainVert,
    fragmentShader: cardFrag,
    transparent: true,
    uniforms: {
      ...dissolveUniforms(w, h, first),
      uMap: { value: surface },
      uRadius: { value: radius },
      uProg: { value: 0 },
      uPale: { value: 0 },
      uEdge: { value: EDGE },
    },
  });
}

function createShadowMaterial(w: number, h: number, radius: number) {
  return new ShaderMaterial({
    vertexShader: plainVert,
    fragmentShader: shadowFrag,
    transparent: true,
    depthWrite: false,
    depthTest: false,
    uniforms: {
      uSize: { value: new Vector2(w, h) },
      uRadius: { value: radius },
      uBlur: { value: 2 },
      uAlpha: { value: 0 },
    },
  });
}

function createFlakeMaterial(surface: Texture, w: number, h: number, first: Vector2, pixelRatio: number) {
  return new ShaderMaterial({
    vertexShader: flakeVert,
    fragmentShader: flakeFrag,
    transparent: true,
    depthWrite: false,
    uniforms: {
      ...dissolveUniforms(w, h, first),
      uMap: { value: surface },
      uT: { value: -1 },
      uCross: { value: DISSOLVE_CROSS_S },
      uTravel: { value: DISSOLVE_TRAVEL_S },
      uLift: { value: 0 },
      uDpr: { value: pixelRatio },
      uHeld: { value: 0 },
      uAngle: { value: 0 },
      uGhostScale: { value: 1 },
      uGhostAngle: { value: 0 },
      uHolderAngle: { value: 0 },
      uCardPos: { value: new Vector3() },
      uGhostPos: { value: new Vector3() },
      uHolder: { value: OFFSCREEN.clone() },
    },
  });
}

export const exileDissolveKind: SceneEffectKind = {
  // The card, its shadow and one flake, so init compiles each program.
  warmUp(host: EffectHost) {
    const first = new Vector2(0, -1);
    const card = new Mesh(new PlaneGeometry(1, 1), createCardMaterial(host.placeholderTexture, 4, 4, 0, first));
    card.name = "exile-dissolve-warmup";
    const shadow = new Mesh(card.geometry, createShadowMaterial(4, 4, 0));
    shadow.name = "exile-dissolve-shadow-warmup";
    const flakes = new Points(flakeGeometry(1, 1), createFlakeMaterial(host.placeholderTexture, 4, 4, first, 1));
    flakes.name = "exile-dissolve-flakes-warmup";
    const vortex = new Mesh(card.geometry, createVortexMaterial());
    vortex.name = "sacrifice-vortex-warmup";
    return [card, shadow, flakes, vortex];
  },
};

class ExileDissolve implements SceneEffect {
  private readonly plane = new PlaneGeometry(1, 1);
  private readonly card: Mesh<PlaneGeometry, ShaderMaterial>;
  private readonly shadow: Mesh<PlaneGeometry, ShaderMaterial> | null = null;
  private readonly flakes: Points<BufferGeometry, ShaderMaterial>;
  private readonly vortex: Mesh<PlaneGeometry, ShaderMaterial> | null = null;
  private startMs: number | null = null;
  private arrived = false;

  constructor(
    private readonly host: EffectHost,
    private readonly params: ExileDissolveParams,
  ) {
    const { pose, surface, radius, tier, pixelRatio, link } = params;
    const { w, h } = pose;
    // The first-released side, in uv space (y up): the bottom edge, or the
    // side facing the holder for a held card.
    const first = new Vector2(0, -1);
    const holder = link?.(host.canvasOrigin()).holder;
    if (holder && (holder.x !== pose.x || holder.y !== pose.y)) {
      first.set(holder.x - pose.x, -(holder.y - pose.y)).normalize();
    }
    this.card = new Mesh(this.plane, createCardMaterial(surface, w, h, radius, first));
    this.card.name = "exile-dissolve";
    this.card.scale.set(w, h, 1);
    this.card.position.set(pose.x, -pose.y, 0);
    // The pose angle turns clockwise on screen; world space is y up.
    this.card.rotation.z = -MathUtils.degToRad(pose.angleDeg);
    this.card.frustumCulled = false;
    this.card.renderOrder = 2;
    this.card.material.uniforms.uSacrifice.value = params.look === "sacrifice" ? 1 : 0;
    host.scene.add(this.card);
    if (params.look === "sacrifice") {
      this.vortex = new Mesh(this.plane, createVortexMaterial());
      this.vortex.name = "sacrifice-vortex";
      this.vortex.position.set(pose.x, -pose.y, 0);
      this.vortex.renderOrder = 1;
      this.vortex.frustumCulled = false;
      host.scene.add(this.vortex);
    }
    if (tier === "full") {
      this.shadow = new Mesh(this.plane, createShadowMaterial(w, h, radius));
      this.shadow.name = "exile-dissolve-shadow";
      this.shadow.rotation.z = this.card.rotation.z;
      this.shadow.frustumCulled = false;
      this.shadow.renderOrder = 0;
      host.scene.add(this.shadow);
    }
    this.flakes = new Points(flakeGeometry(DISSOLVE_FLAKES[tier], w / h), createFlakeMaterial(surface, w, h, first, pixelRatio));
    this.flakes.name = "exile-dissolve-flakes";
    const uniforms = this.flakes.material.uniforms;
    uniforms.uCardPos.value.set(pose.x, -pose.y, 0);
    uniforms.uGhostPos.value.set(pose.x, -pose.y, 0);
    uniforms.uAngle.value = this.card.rotation.z;
    uniforms.uGhostAngle.value = this.card.rotation.z;
    uniforms.uHeld.value = link ? 1 : 0;
    uniforms.uSacrifice.value = params.look === "sacrifice" ? 1 : 0;
    this.flakes.frustumCulled = false;
    this.flakes.renderOrder = 3;
    host.scene.add(this.flakes);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const { pose, pace, link } = this.params;
    const t = (nowMs - this.startMs) / 1000 / pace;
    const lifted = smooth(0, 1, t / DISSOLVE_LIFT_S);
    const sacrifice = this.params.look === "sacrifice";
    const z = LIFT_PX * lifted * (sacrifice ? -0.5 : 1);
    const td = t - DISSOLVE_LIFT_S;
    this.card.position.z = z;
    const card = this.card.material.uniforms;
    card.uPale.value = PALE * lifted;
    card.uProg.value = td <= 0 ? 0 : -0.03 + 1.06 * (td / DISSOLVE_CROSS_S);
    const flakes = this.flakes.material.uniforms;
    flakes.uT.value = td;
    flakes.uLift.value = z;
    if (this.vortex) {
      const envelope = smooth(0, 0.16, t) * (1 - smooth(0.85, 1.55, t));
      this.vortex.scale.set(pose.w * 1.4 * envelope, pose.h * 0.9 * envelope, 1);
      this.vortex.material.uniforms.uTime.value = t;
      this.vortex.material.uniforms.uAlpha.value = envelope;
    }
    if (link) this.aimLink(link(this.host.canvasOrigin()));
    if (this.shadow) {
      const blur = 2 + z * 0.35;
      this.shadow.position.set(pose.x + z * 0.3, -pose.y - z * 0.42, 0);
      this.shadow.scale.set(pose.w + blur * 2, pose.h + blur * 2, 1);
      const shadow = this.shadow.material.uniforms;
      shadow.uBlur.value = blur;
      // Fades with the card as the front eats it.
      shadow.uAlpha.value = 0.45 * smooth(1, 10, z) * (1 - smooth(0.1, 0.8, td / DISSOLVE_CROSS_S));
    }
    if (link && !this.arrived && td > DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * GHOST_ARRIVAL) this.arrive();
    return td < DISSOLVE_CROSS_S + DISSOLVE_TRAVEL_S * 1.3;
  }

  // Flakes already in the air bend toward the ghost when it appears.
  private aimLink({ ghost, holder }: LinkAim) {
    const flakes = this.flakes.material.uniforms;
    const target = ghost ?? holder;
    if (target) {
      flakes.uGhostPos.value.set(target.x, -target.y, 0);
      flakes.uGhostAngle.value = -MathUtils.degToRad(target.angleDeg);
    }
    if (ghost) flakes.uGhostScale.value = ghost.w / this.params.pose.w;
    if (holder) {
      flakes.uHolder.value.set(holder.x, -holder.y, holder.w / 2, holder.h / 2);
      flakes.uHolderAngle.value = -MathUtils.degToRad(holder.angleDeg);
    } else {
      flakes.uHolder.value.copy(OFFSCREEN);
    }
  }

  private arrive() {
    this.arrived = true;
    this.params.onArrive();
  }

  dispose(silent: boolean) {
    this.host.scene.remove(this.card, this.flakes);
    if (this.vortex) {
      this.host.scene.remove(this.vortex);
      this.vortex.material.dispose();
    }
    if (this.shadow) {
      this.host.scene.remove(this.shadow);
      this.shadow.material.dispose();
    }
    this.plane.dispose();
    this.card.material.dispose();
    this.flakes.geometry.dispose();
    this.flakes.material.dispose();
    this.params.surface.dispose();
    if (!silent && !this.arrived) this.arrive();
  }
}

export function createExileDissolve(host: EffectHost, params: ExileDissolveParams): SceneEffect {
  return new ExileDissolve(host, params);
}
