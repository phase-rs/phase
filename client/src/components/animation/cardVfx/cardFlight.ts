// ─── Card flight ───
// A card lifted off one surface and set down on another, drawn on the shared
// overlay. The look and motion port the approved Play lab (`fly()`): a bowed
// path, a height curve per profile, tilt toward the viewer and lean into the
// direction of travel, an optional back-to-front flip, and a drop shadow at
// the `full` tier. The target is re-measured every frame; the flight lands
// only on the object's own node, once that node has stopped moving.

import {
  DataTexture,
  DoubleSide,
  MathUtils,
  Mesh,
  PlaneGeometry,
  Quaternion,
  ShaderMaterial,
  type Texture,
  Vector2,
  Vector3,
} from "three";

import type { ObjectId } from "../../../adapter/types.ts";
import type { VfxQuality } from "../../../animation/types.ts";
import type { Aim, CardPose } from "./cardAnchors.ts";
import type { CardFlightRoute, FlightDestination, RippleLook } from "./cardFlightSpecs.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { ROUNDED_BOX_GLSL } from "./glslChunks.ts";

/** The tiers that mount the overlay; `minimal` never does. */
export type CardVfxTier = Exclude<VfxQuality, "minimal">;

/** Which way the card turns over in flight. */
export type FlightFlip = "none" | "toFront" | "toBack";

export type FlightRelease = "land" | "abandon";

export const CAST_FLIGHT_MS = 440;
export const RESOLVE_FLIGHT_MS = 420;
export const DRAW_FLIGHT_MS = 340;
export const SETTLE_MS = 150;
/** A rebased leg lasts at least this fraction of the route's flight time. */
export const REBASE_MIN_FRACTION = 0.6;
/** A hold hovers above the source by this fraction of the card's height. */
export const HOLD_RISE_FRACTION = 0.5;
/** An own node that moved less than this since the previous frame is stationary. */
export const LAND_STATIONARY_PX = 0.5;
export const LAND_STATIONARY_WAIT_MAX_MS = 300;
/** An own node whose accumulated opacity changed less than this since the
 *  previous frame is stationary in opacity; a fall past it during a reveal
 *  means the node is fading out. */
export const LAND_STATIONARY_OPACITY = 0.002;
/** After release, the card stays drawn on its node for at most this long while
 *  the node's face image loads. Not scaled by pace: image fetch and decode
 *  latency does not change with animation speed. */
export const LAND_REVEAL_WAIT_MAX_MS = 400;
export const CARD_FLIGHT_MAX_AWAIT_MS = 4000;
export const ABANDON_FADE_MS = 150;
export const LANDING_CROSSFADE_MS = 150;
/** Frames after the engine commit on which a missing own node abandons. */
export const ABSENT_FRAMES_AFTER_COMMIT = 2;
/** Card width over height (63 × 88 mm). */
export const CARD_ASPECT = 63 / 88;
/** A target within this of the card aspect is a full-card slot. */
export const FULL_CARD_ASPECT_TOLERANCE = 0.05;

const PEAK_PX = 170;
const PANEL_PEAK_FRACTION = 0.65;
const TILT_RAD = MathUtils.degToRad(14);
const LEAN_RAD = MathUtils.degToRad(9);
const BOW_FRACTION = 0.16;
const BOW_MAX_PX = 90;
const RADIUS_FRACTION = 0.046;
const SETTLE_SQUASH = 0.028;
const LAND_XY_AT = 0.72;
const LAND_PEAK_AT = 0.36;
const FLIP_FROM = 0.12;
const FLIP_TO = 0.72;

/** `panel`: lift and set down on a UI surface (stack, a pile, a hand). `land`:
 *  glide until over the slot, then drop onto the board and settle. */
export type FlightCurve = "panel" | "land";

export interface FlightProfile {
  curve: FlightCurve;
  durationMs: number;
}

/** How a flight moves, by the zone it lands in. */
export const FLIGHT_PROFILES: Record<FlightDestination, FlightProfile> = {
  Stack: { curve: "panel", durationMs: CAST_FLIGHT_MS },
  Battlefield: { curve: "land", durationMs: RESOLVE_FLIGHT_MS },
  Graveyard: { curve: "panel", durationMs: RESOLVE_FLIGHT_MS },
  Hand: { curve: "panel", durationMs: DRAW_FLIGHT_MS },
  Library: { curve: "panel", durationMs: RESOLVE_FLIGHT_MS },
  Exile: { curve: "panel", durationMs: RESOLVE_FLIGHT_MS },
};

/** A card pose in flight: height above the table, how far the tilt and lean
 *  are applied, and the turn about the card's vertical axis (0 = front up). */
export interface FlightState extends CardPose {
  z: number;
  bump: number;
  flip: number;
}

export interface FlightFrame extends FlightState {
  quaternion: Quaternion;
  /** The in-plane rotation alone, which the shadow follows. */
  planar: Quaternion;
}

const X_AXIS = new Vector3(1, 0, 0);
const Y_AXIS = new Vector3(0, 1, 0);
const Z_AXIS = new Vector3(0, 0, 1);

const clamp01 = (x: number) => Math.min(Math.max(x, 0), 1);
const smooth = (a: number, b: number, x: number) => {
  const t = clamp01((x - a) / (b - a));
  return t * t * (3 - 2 * t);
};
const easeInOutCubic = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);
const easeOutCubic = (t: number) => 1 - (1 - t) ** 3;
const easeOutQuad = (t: number) => 1 - (1 - t) ** 2;
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;

function flipAngle(flip: FlightFlip, end: boolean): number {
  switch (flip) {
    case "none":
      return 0;
    case "toFront":
      return end ? 0 : Math.PI;
    case "toBack":
      return end ? Math.PI : 0;
  }
}

/** The largest card-aspect box inside `pose`, centred (contain fit). */
export function fitCardAspect(pose: CardPose): CardPose {
  return pose.w / pose.h > CARD_ASPECT
    ? { ...pose, w: pose.h * CARD_ASPECT }
    : { ...pose, h: pose.w / CARD_ASPECT };
}

/** A card lying on `pose`, turned as `flip` starts. */
export function restingState(pose: CardPose, flip: FlightFlip): FlightState {
  return { ...fitCardAspect(pose), z: 0, bump: 0, flip: flipAngle(flip, false) };
}

/** The pose `t` of the way along a leg from `from` to `to`. Pure, so every
 *  frame is re-derived from the live target: a moving target bends the path
 *  rather than restarting it. */
export function flightPose(
  curve: FlightCurve,
  t: number,
  from: FlightState,
  to: CardPose,
  targetFlip: number,
): FlightFrame {
  // The path bows toward the top of the screen, like a card lifted and tossed.
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const dist = Math.hypot(dx, dy) || 1;
  let px = -dy / dist;
  let py = dx / dist;
  if (py > 0) {
    px = -px;
    py = -py;
  }
  const bow = Math.min(dist * BOW_FRACTION, BOW_MAX_PX);
  const cx = (from.x + to.x) / 2 + px * bow;
  const cy = (from.y + to.y) / 2 + py * bow;
  const bez = (e: number): [number, number] => {
    const u = 1 - e;
    return [u * u * from.x + 2 * u * e * cx + e * e * to.x, u * u * from.y + 2 * u * e * cy + e * e * to.y];
  };
  const peak = PEAK_PX * (curve === "land" ? 1 : PANEL_PEAK_FRACTION) * Math.min(1, 0.35 + dist / 500);

  let exy: number;
  let lift: number;
  switch (curve) {
    case "land":
      // Reach the air above the slot, then fall onto it.
      exy = easeOutCubic(Math.min(t / LAND_XY_AT, 1));
      lift = t < LAND_PEAK_AT
        ? peak * easeOutQuad(t / LAND_PEAK_AT)
        : peak * (1 - ((t - LAND_PEAK_AT) / (1 - LAND_PEAK_AT)) ** 2);
      break;
    case "panel":
      exy = easeInOutCubic(t);
      lift = peak * Math.sin(Math.PI * t);
      break;
  }

  const [x, y] = bez(exy);
  const [x2, y2] = bez(Math.min(exy + 0.01, 1));
  // A leg that starts in the air (a rebase or handoff) carries its height,
  // tilt and turn in and blends them out, so the switch has no jump.
  const z = lift + from.z * (1 - exy);
  const bump = from.bump * (1 - t) + (1 - from.bump) * Math.sin(Math.PI * t);
  const flip = lerp(from.flip, targetFlip, smooth(FLIP_FROM, FLIP_TO, t));
  const angleDeg = lerp(from.angleDeg, to.angleDeg, exy);

  const planar = new Quaternion().setFromAxisAngle(Z_AXIS, -MathUtils.degToRad(angleDeg));
  const tilt = new Quaternion().setFromAxisAngle(X_AXIS, TILT_RAD * bump);
  const turn = new Quaternion().setFromAxisAngle(Y_AXIS, flip);
  // Lean the leading edge down along the direction of travel (world is y-up).
  const vx = x2 - x;
  const vy = -(y2 - y);
  const vl = Math.hypot(vx, vy);
  const leanAxis = vl > 1e-4 ? new Vector3(-vy / vl, vx / vl, 0) : Y_AXIS;
  const quaternion = new Quaternion()
    .setFromAxisAngle(leanAxis, LEAN_RAD * bump)
    .multiply(planar)
    .multiply(tilt)
    .multiply(turn);

  return {
    x,
    y,
    z,
    w: lerp(from.w, to.w, exy),
    h: lerp(from.h, to.h, exy),
    angleDeg,
    bump,
    flip,
    quaternion,
    planar,
  };
}

// One card: a unit plane scaled to the card, front and back textures, lit
// relative to rest so a flat card matches its DOM twin exactly. The plane is
// flat (scale z = 1), so the model matrix maps its normal exactly.
const cardVert = /* glsl */ `
  varying vec2 vUv; varying vec3 vN;
  void main() {
    vUv = uv;
    vN = normalize(mat3(modelMatrix) * normal);
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  }`;
// A washed card (a countered spell) runs pale toward `uWashTint`, its ink
// rippling while the wash runs through it.
const cardFrag = /* glsl */ `
  uniform sampler2D uFront, uBack; uniform vec2 uSize; uniform float uRadius, uAlpha, uWash, uWashTime;
  uniform vec3 uWashTint;
  varying vec2 vUv; varying vec3 vN;
  ${ROUNDED_BOX_GLSL}
  void main() {
    vec2 p = (vUv - 0.5) * uSize;
    float edge = clamp(0.5 - roundedBox(p, uSize * 0.5, uRadius), 0.0, 1.0);
    if (edge <= 0.0) discard;
    vec2 c = vUv - 0.5;
    float d = length(c);
    vec2 uv = vUv + c / max(d, 0.001) * sin(d * 70.0 - uWashTime * 16.0) * 0.004 * uWash * (1.0 - uWash) * 4.0;
    vec3 n = gl_FrontFacing ? vN : -vN;
    vec3 col = gl_FrontFacing ? texture2D(uFront, uv).rgb : texture2D(uBack, vec2(1.0 - uv.x, uv.y)).rgb;
    float luma = dot(col, vec3(0.299, 0.587, 0.114));
    col = mix(col, mix(vec3(luma), uWashTint, 0.55) * 0.5 + uWashTint * 0.45, uWash * 0.8);
    vec3 L = normalize(vec3(-0.35, 0.55, 0.76));
    vec3 H = normalize(L + vec3(0.0, 0.0, 1.0));
    float shade = 1.0 + 0.55 * (dot(n, L) - L.z);
    float spec = max(pow(max(dot(n, H), 0.0), 40.0) - pow(H.z, 40.0), 0.0) * 0.45;
    gl_FragColor = vec4(col * clamp(shade, 0.35, 1.4) + spec, edge * uAlpha);
    #include <colorspace_fragment>
  }`;
const shadowFrag = /* glsl */ `
  uniform vec2 uSize; uniform float uRadius, uBlur, uAlpha;
  varying vec2 vUv;
  ${ROUNDED_BOX_GLSL}
  void main() {
    vec2 p = (vUv - 0.5) * (uSize + 2.0 * uBlur);
    float d = roundedBox(p, uSize * 0.5, uRadius);
    gl_FragColor = vec4(0.0, 0.0, 0.0, uAlpha * (1.0 - smoothstep(-uBlur, uBlur, d)));
  }`;
const plainVert = /* glsl */ `
  varying vec2 vUv;
  void main() { vUv = uv; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

const UNIT_PLANE = new PlaneGeometry(1, 1);

// The flight and its warm-up build meshes through these same factories, so
// three's program cache keys match and no flight compiles a program.
function createCardMesh(front: Texture, back: Texture, alpha: number): Mesh<PlaneGeometry, ShaderMaterial> {
  const mesh = new Mesh(
    UNIT_PLANE,
    new ShaderMaterial({
      vertexShader: cardVert,
      fragmentShader: cardFrag,
      transparent: true,
      side: DoubleSide,
      uniforms: {
        uFront: { value: front },
        uBack: { value: back },
        uSize: { value: new Vector2(1, 1) },
        uRadius: { value: RADIUS_FRACTION },
        uAlpha: { value: alpha },
        uWash: { value: 0 },
        uWashTime: { value: 0 },
        uWashTint: { value: new Vector3() },
      },
    }),
  );
  mesh.renderOrder = 2;
  mesh.frustumCulled = false;
  return mesh;
}

function createShadowMesh(): Mesh<PlaneGeometry, ShaderMaterial> {
  const mesh = new Mesh(
    UNIT_PLANE,
    new ShaderMaterial({
      vertexShader: plainVert,
      fragmentShader: shadowFrag,
      transparent: true,
      depthWrite: false,
      depthTest: false,
      uniforms: {
        uSize: { value: new Vector2() },
        uRadius: { value: RADIUS_FRACTION },
        uBlur: { value: 2 },
        uAlpha: { value: 0 },
      },
    }),
  );
  mesh.renderOrder = 0;
  mesh.frustumCulled = false;
  return mesh;
}

/** A 1×1 texture a warm-up card binds when no card back could be loaded. */
export function createPlaceholderTexture(): Texture {
  const texture = new DataTexture(new Uint8Array([0, 0, 0, 255]), 1, 1);
  texture.needsUpdate = true;
  return texture;
}

export const cardFlightKind: SceneEffectKind = {
  // Every tier's meshes, textured as a flight textures them. The warm-up card
  // draws fully transparent, so the warm-up frame leaves nothing to see.
  warmUp(host: EffectHost) {
    const texture = host.backTexture ?? host.placeholderTexture;
    const card = createCardMesh(texture, texture, 0);
    card.name = "card-flight-warmup";
    const shadow = createShadowMesh();
    shadow.name = "card-shadow-warmup";
    return [card, shadow];
  },
};

/** The tint a washed card runs toward, by the ripple that washed it. */
const WASH_TINTS: Record<RippleLook, [number, number, number]> = {
  water: [0.72, 0.85, 0.94],
  pale: [0.88, 0.88, 0.86],
};

/** A permanent's surface redrawn as a texture, and where it lies. */
export interface BoardSurface {
  pose: CardPose;
  surface: Texture;
  radius: number;
}

/** A wash running through the card where it rests, before it leaves. */
export interface FlightWash {
  /** When it starts, on the frame clock (`performance.now()`). */
  startMs: number;
  durationMs: number;
  look: RippleLook;
}

export interface CardFlightParams {
  objectId: ObjectId;
  route: CardFlightRoute;
  from: FlightState;
  /** The face texture, owned by the flight; `null` shows the back on both sides. */
  front: Texture | null;
  back: Texture | null;
  flip: FlightFlip;
  pace: number;
  /** How long the card waits, resting on its source, before it leaves. */
  delayMs: number;
  /** A countered spell's wash; `null` for any other flight. */
  wash: FlightWash | null;
  /** The board surface of the permanent it leaves, owned by the flight: the
   *  card rests as that copy until it leaves. `null` rests as the card. */
  rest: BoardSurface | null;
  tier: CardVfxTier;
  aim: (origin: DOMRectReadOnly) => Aim;
  /** Whether the engine has committed the state this flight moves to. */
  committed: () => boolean;
  onRelease: (reason: FlightRelease) => void;
}

export interface CardFlight extends SceneEffect {
  readonly objectId: ObjectId;
  /** The pose last drawn, where a handoff continues from. */
  currentState(): FlightState;
  /** True from the release frame on (revealing or fading), false while the
   *  card is still flying or settling. */
  isReleased(): boolean;
}

type AimKey = HTMLElement | "remembered" | "hold";

const aimKey = (aim: Aim): AimKey =>
  aim.kind === "hold" ? "hold" : aim.el ?? "remembered";

interface Leg {
  start: FlightState;
  startMs: number;
  durationMs: number;
  aimKey: AimKey;
}

type FlightPhase =
  | { kind: "flying" }
  | { kind: "settling"; startMs: number; frame: FlightFrame }
  | { kind: "fading"; startMs: number; durationMs: number; frame: FlightFrame }
  // Released and unveiled, but still drawn opaque on the own node until its
  // face image has settled; `then` says how the card leaves.
  | { kind: "revealing"; sinceMs: number; then: "dispose" | "crossfade"; baseOpacity: number | null };

class CardFlightEffect implements CardFlight {
  readonly objectId: ObjectId;
  private readonly card: Mesh<PlaneGeometry, ShaderMaterial>;
  private rest: Mesh<PlaneGeometry, ShaderMaterial> | null = null;
  private readonly shadow: Mesh<PlaneGeometry, ShaderMaterial> | null;
  private readonly profile: FlightProfile;
  private readonly targetFlip: number;
  private readonly holdPose: CardPose;
  private state: FlightState;
  private phase: FlightPhase = { kind: "flying" };
  private flightStartMs: number | null = null;
  private leg: Leg | null = null;
  private absentAfterCommit = 0;
  private arrivedMs: number | null = null;
  private previousAim: { key: AimKey; pose: CardPose; opacity: number | null } | null = null;

  constructor(
    private readonly host: EffectHost,
    private readonly params: CardFlightParams,
  ) {
    this.objectId = params.objectId;
    this.profile = FLIGHT_PROFILES[params.route.to];
    this.targetFlip = flipAngle(params.flip, true);
    this.state = params.from;
    this.holdPose = { ...params.from, y: params.from.y - params.from.h * HOLD_RISE_FRACTION };
    const back = params.back ?? host.placeholderTexture;
    this.card = createCardMesh(params.front ?? back, back, 1);
    this.card.name = "card-flight";
    if (params.wash) this.card.material.uniforms.uWashTint.value.set(...WASH_TINTS[params.wash.look]);
    host.scene.add(this.card);
    if (params.tier === "full") {
      this.shadow = createShadowMesh();
      this.shadow.name = "card-shadow";
      host.scene.add(this.shadow);
    } else {
      this.shadow = null;
    }
    if (params.rest) this.restOn(params.rest, back);
  }

  // A permanent's tile can be shaped nothing like its card (an art crop), so
  // while it waits it rests as a copy of the tile, and turns into the card as
  // it leaves.
  private restOn({ pose, surface, radius }: BoardSurface, back: Texture) {
    const rest = createCardMesh(surface, back, 1);
    rest.name = "card-flight-rest";
    rest.position.set(pose.x, -pose.y, 0);
    rest.rotation.z = -MathUtils.degToRad(pose.angleDeg);
    rest.scale.set(pose.w, pose.h, 1);
    rest.material.uniforms.uSize.value.set(pose.w, pose.h);
    rest.material.uniforms.uRadius.value = radius;
    this.host.scene.add(rest);
    this.rest = rest;
    this.card.visible = false;
  }

  private leaveRest() {
    const { rest } = this;
    if (!rest) return;
    this.host.scene.remove(rest);
    rest.material.dispose();
    this.params.rest?.surface.dispose();
    this.rest = null;
    this.card.visible = true;
  }

  currentState(): FlightState {
    return this.state;
  }

  isReleased(): boolean {
    switch (this.phase.kind) {
      case "flying":
      case "settling":
        return false;
      case "fading":
      case "revealing":
        return true;
    }
  }

  update(nowMs: number): boolean {
    this.flightStartMs ??= nowMs + this.params.delayMs;
    this.applyWash(nowMs);
    const { pace } = this.params;
    switch (this.phase.kind) {
      case "revealing":
        return this.reveal(nowMs, this.phase);
      case "fading": {
        const alpha = 1 - (nowMs - this.phase.startMs) / this.phase.durationMs;
        if (alpha <= 0) return false;
        this.draw(this.phase.frame, 1, alpha);
        return true;
      }
      case "settling": {
        const s = Math.min((nowMs - this.phase.startMs) / (SETTLE_MS * pace), 1);
        this.draw(this.phase.frame, 1 - SETTLE_SQUASH * Math.sin(Math.PI * s), 1);
        if (s >= 1) this.release("land", { kind: "revealing", sinceMs: nowMs, then: "dispose", baseOpacity: null });
        return true;
      }
      case "flying":
        if (nowMs >= this.flightStartMs) {
          this.leaveRest();
          return this.fly(nowMs, this.flightStartMs);
        }
        // A staggered card rests on its veiled source until its turn to leave.
        if (!this.rest) this.draw(flightPose(this.profile.curve, 0, this.state, this.state, this.state.flip), 1, 1);
        return true;
    }
  }

  private applyWash(nowMs: number) {
    const { wash } = this.params;
    if (!wash) return;
    const uniforms = this.card.material.uniforms;
    uniforms.uWash.value = smooth(0, 1, (nowMs - wash.startMs) / wash.durationMs);
    uniforms.uWashTime.value = nowMs / 1000;
  }

  private fly(nowMs: number, flightStartMs: number): boolean {
    const { pace } = this.params;
    const aim = this.params.aim(this.host.canvasOrigin());
    const key = aimKey(aim);
    const target = aim.kind === "hold" ? this.holdPose : fitCardAspect(aim.pose);
    const flightMs = this.profile.durationMs;
    if (!this.leg) {
      this.leg = { start: this.state, startMs: nowMs, durationMs: flightMs * pace, aimKey: key };
    } else if (key !== this.leg.aimKey) {
      // A new target element: a fresh leg from where the card is now.
      const elapsed = (nowMs - flightStartMs) / pace;
      const legMs = Math.max(REBASE_MIN_FRACTION * flightMs, flightMs - elapsed) * pace;
      this.leg = { start: this.state, startMs: nowMs, durationMs: legMs, aimKey: key };
    }
    const t = clamp01((nowMs - this.leg.startMs) / this.leg.durationMs);
    const frame = flightPose(this.profile.curve, t, this.leg.start, target, this.targetFlip);
    this.state = frame;
    this.draw(frame, 1, 1);

    const previous = this.previousAim;
    this.previousAim =
      aim.kind === "hold" ? null : { key, pose: aim.pose, opacity: aim.kind === "own" ? aim.opacity : null };

    // The queue never waits: once the engine has committed, a destination that
    // still has no own node is abandoned, and so is any flight past its bound.
    const committed = this.params.committed();
    this.absentAfterCommit = committed && aim.kind !== "own" ? this.absentAfterCommit + 1 : 0;
    if (
      this.absentAfterCommit >= ABSENT_FRAMES_AFTER_COMMIT ||
      nowMs - flightStartMs >= CARD_FLIGHT_MAX_AWAIT_MS * pace
    ) {
      this.release("abandon", {
        kind: "fading",
        startMs: nowMs,
        durationMs: ABANDON_FADE_MS * pace,
        frame,
      });
      return true;
    }

    if (t < 1 || aim.kind !== "own") {
      this.arrivedMs = null;
      return true;
    }
    this.arrivedMs ??= nowMs;
    // A provisional previous aim carries no opacity, so it gives no baseline.
    const stationary =
      (previous?.key === key &&
        previous.opacity !== null &&
        poseMoved(previous.pose, aim.pose) < LAND_STATIONARY_PX &&
        Math.abs(previous.opacity - aim.opacity) < LAND_STATIONARY_OPACITY) ||
      nowMs - this.arrivedMs >= LAND_STATIONARY_WAIT_MAX_MS * pace;
    if (stationary) this.land(nowMs, aim.pose, frame);
    return true;
  }

  private land(nowMs: number, slot: CardPose, frame: FlightFrame) {
    const fullCard = Math.abs(slot.w / slot.h - CARD_ASPECT) <= FULL_CARD_ASPECT_TOLERANCE;
    if (!fullCard) {
      // The GL card cannot cover a differently shaped slot, so it releases
      // the slot at once and, once the slot shows its face, fades out over it.
      this.release("land", { kind: "revealing", sinceMs: nowMs, then: "crossfade", baseOpacity: null });
      return;
    }
    switch (this.profile.curve) {
      case "land":
        this.phase = { kind: "settling", startMs: nowMs, frame };
        return;
      case "panel":
        // Released now; the card stays drawn on the slot until it shows its face.
        this.release("land", { kind: "revealing", sinceMs: nowMs, then: "dispose", baseOpacity: null });
        return;
    }
  }

  // The release frame is not evaluated here: its DOM still shows the
  // pre-unveil content, since React applies the store update after the frame.
  private reveal(nowMs: number, phase: Extract<FlightPhase, { kind: "revealing" }>): boolean {
    const aim = this.params.aim(this.host.canvasOrigin());
    if (aim.kind !== "own") return false;
    // At t = 1 every leg term that depends on its start is zero, so the
    // endpoint is the same whatever `from` is; the card sits on the node's pose.
    const frame = flightPose(this.profile.curve, 1, this.state, fitCardAspect(aim.pose), this.targetFlip);
    this.state = frame;
    this.draw(frame, 1, 1);
    // The first revealing frame's opacity is the baseline: the node was
    // stationary in opacity at release, so a later fall means it is fading out.
    const baseOpacity = phase.baseOpacity ?? aim.opacity;
    this.phase = { ...phase, baseOpacity };
    const ended =
      aim.faceImagesSettled ||
      aim.opacity < baseOpacity - LAND_STATIONARY_OPACITY ||
      nowMs - phase.sinceMs >= LAND_REVEAL_WAIT_MAX_MS;
    if (!ended) return true;
    switch (phase.then) {
      case "dispose":
        return false;
      case "crossfade":
        this.phase = {
          kind: "fading",
          startMs: nowMs,
          durationMs: LANDING_CROSSFADE_MS * this.params.pace,
          frame,
        };
        return true;
    }
  }

  private release(reason: FlightRelease, next: FlightPhase) {
    this.phase = next;
    this.params.onRelease(reason);
  }

  private draw(pose: FlightFrame, squash: number, alpha: number) {
    const { card, shadow } = this;
    card.position.set(pose.x, -pose.y, pose.z);
    card.quaternion.copy(pose.quaternion);
    card.scale.set(pose.w * squash, pose.h * squash, 1);
    const uniforms = card.material.uniforms;
    uniforms.uSize.value.set(pose.w, pose.h);
    uniforms.uRadius.value = pose.w * RADIUS_FRACTION;
    uniforms.uAlpha.value = alpha;
    if (!shadow) return;
    const blur = 2 + pose.z * 0.09;
    shadow.position.set(pose.x + pose.z * 0.3, -pose.y - pose.z * 0.42, 0);
    shadow.quaternion.copy(pose.planar);
    shadow.scale.set(pose.w + blur * 2, pose.h + blur * 2, 1);
    const shadowUniforms = shadow.material.uniforms;
    shadowUniforms.uSize.value.set(pose.w, pose.h);
    shadowUniforms.uRadius.value = pose.w * RADIUS_FRACTION;
    shadowUniforms.uBlur.value = blur;
    // A resting card casts none of its own; the DOM card already has one.
    shadowUniforms.uAlpha.value = alpha * 0.5 * smooth(1, 18, pose.z) * (1 - smooth(160, 520, pose.z));
  }

  dispose() {
    this.leaveRest();
    this.host.scene.remove(this.card);
    this.card.material.dispose();
    this.params.front?.dispose();
    if (this.shadow) {
      this.host.scene.remove(this.shadow);
      this.shadow.material.dispose();
    }
  }
}

function poseMoved(a: CardPose, b: CardPose): number {
  return Math.max(Math.hypot(a.x - b.x, a.y - b.y), Math.abs(a.w - b.w), Math.abs(a.h - b.h));
}

export function createCardFlight(host: EffectHost, params: CardFlightParams): CardFlight {
  return new CardFlightEffect(host, params);
}
