// ─── Landing dust ───
// A ring of dust kicked out from a card's outline as it lands on the
// battlefield, ported from the approved Play lab (`landingFx`). Tinted toward
// the card's engine-provided colours; a hidden or colourless card gets plain
// dust. `full` tier only.

import {
  BufferGeometry,
  Color,
  Float32BufferAttribute,
  MathUtils,
  Points,
  Quaternion,
  ShaderMaterial,
  Vector3,
} from "three";

import type { ManaColor } from "../../../adapter/types.ts";
import type { CardPose } from "./cardAnchors.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";

const DUST_PARTICLES = 110;
/** The longest particle life, in seconds before pace. */
const DUST_MAX_LIFE_S = 0.7;
/** How far the dust colour moves from plain dust toward the card's colour. */
const DUST_TINT = 0.3;
const NEUTRAL_DUST = new Color(0.42, 0.39, 0.34);
const MANA_TINTS: Record<ManaColor, Color> = {
  White: new Color(0xf0e6c8),
  Blue: new Color(0x3a7bd5),
  Black: new Color(0x4a3f55),
  Red: new Color(0xd8442e),
  Green: new Color(0x3f8f4a),
};
const Z_AXIS = new Vector3(0, 0, 1);

const dustVert = /* glsl */ `
  attribute vec2 aVel;
  attribute float aLife;
  attribute float aSize;
  uniform float uTime;
  uniform float uDpr;
  varying float vA;
  void main() {
    float life = clamp(uTime / aLife, 0.0, 1.0);
    // Drag: fast out of the card's footprint, then drifting to a stop.
    vec2 off = aVel * (1.0 - exp(-7.0 * uTime)) / 7.0;
    vA = 1.0 - life;
    gl_PointSize = aSize * uDpr;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position.xy + off, position.z, 1.0);
  }
`;

const dustFrag = /* glsl */ `
  uniform vec3 uColor;
  varying float vA;
  void main() {
    vec2 q = gl_PointCoord - 0.5;
    if (dot(q, q) > 0.25) discard;
    gl_FragColor = vec4(uColor, vA * 0.75);
    #include <colorspace_fragment>
  }
`;

/** Plain dust moved toward the average of the card's colours. */
export function dustColor(colors: readonly ManaColor[] | null): Color {
  const color = NEUTRAL_DUST.clone();
  if (!colors?.length) return color;
  const tint = new Color(0, 0, 0);
  for (const mana of colors) tint.add(MANA_TINTS[mana]);
  tint.multiplyScalar(1 / colors.length);
  return color.lerp(tint, DUST_TINT);
}

function createDustPoints(
  geometry: BufferGeometry,
  color: Color,
  pixelRatio: number,
): Points<BufferGeometry, ShaderMaterial> {
  const points = new Points(
    geometry,
    new ShaderMaterial({
      vertexShader: dustVert,
      fragmentShader: dustFrag,
      transparent: true,
      depthWrite: false,
      uniforms: {
        uTime: { value: 0 },
        uDpr: { value: pixelRatio },
        uColor: { value: color },
      },
    }),
  );
  points.frustumCulled = false;
  points.renderOrder = 1;
  return points;
}

function dustGeometry(positions: number[], velocities: number[], lives: number[], sizes: number[]) {
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new Float32BufferAttribute(positions, 3));
  geometry.setAttribute("aVel", new Float32BufferAttribute(velocities, 2));
  geometry.setAttribute("aLife", new Float32BufferAttribute(lives, 1));
  geometry.setAttribute("aSize", new Float32BufferAttribute(sizes, 1));
  return geometry;
}

export const landingDustKind: SceneEffectKind = {
  // One dead particle with the dust material, so init compiles its program.
  warmUp() {
    const points = createDustPoints(dustGeometry([0, 0, 1], [0, 0], [1], [0]), NEUTRAL_DUST.clone(), 1);
    points.name = "landing-dust-warmup";
    return [points];
  },
};

class LandingDust implements SceneEffect {
  private readonly points: Points<BufferGeometry, ShaderMaterial>;
  private startMs: number | null = null;

  constructor(
    private readonly host: EffectHost,
    pose: CardPose,
    colors: readonly ManaColor[] | null,
    private readonly pace: number,
    pixelRatio: number,
  ) {
    const positions: number[] = [];
    const velocities: number[] = [];
    const lives: number[] = [];
    const sizes: number[] = [];
    const { w, h } = pose;
    const turn = new Quaternion().setFromAxisAngle(Z_AXIS, -MathUtils.degToRad(pose.angleDeg));
    const v = new Vector3();
    const perimeter = 2 * (w + h);
    for (let i = 0; i < DUST_PARTICLES; i++) {
      // Sample the card outline; push outward along its normal with some spread.
      let s = Math.random() * perimeter;
      let lx: number, ly: number, nx: number, ny: number;
      if (s < w) {
        [lx, ly, nx, ny] = [s - w / 2, h / 2, 0, 1];
      } else if ((s -= w) < h) {
        [lx, ly, nx, ny] = [w / 2, h / 2 - s, 1, 0];
      } else if ((s -= h) < w) {
        [lx, ly, nx, ny] = [w / 2 - s, -h / 2, 0, -1];
      } else {
        s -= w;
        [lx, ly, nx, ny] = [-w / 2, -h / 2 + s, -1, 0];
      }
      const speed = 140 + Math.random() * 260;
      const spread = (Math.random() - 0.5) * 0.9;
      v.set(lx, ly, 0).applyQuaternion(turn);
      positions.push(pose.x + v.x, -pose.y + v.y, 1);
      v.set((nx + ny * spread) * speed, (ny - nx * spread) * speed, 0).applyQuaternion(turn);
      velocities.push(v.x, v.y);
      lives.push(0.3 + Math.random() * (DUST_MAX_LIFE_S - 0.3));
      sizes.push(1.2 + Math.random() * 1.8);
    }
    this.points = createDustPoints(
      dustGeometry(positions, velocities, lives, sizes),
      dustColor(colors),
      pixelRatio,
    );
    this.points.name = "landing-dust";
    host.scene.add(this.points);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = (nowMs - this.startMs) / 1000 / this.pace;
    this.points.material.uniforms.uTime.value = t;
    return t < DUST_MAX_LIFE_S;
  }

  dispose() {
    this.host.scene.remove(this.points);
    this.points.geometry.dispose();
    this.points.material.dispose();
  }
}

export function createLandingDust(
  host: EffectHost,
  pose: CardPose,
  colors: readonly ManaColor[] | null,
  pace: number,
  pixelRatio: number,
): SceneEffect {
  return new LandingDust(host, pose, colors, pace, pixelRatio);
}
