// ─── Water ───
// The water the card VFX draw with: a pressurised jet, its splash, and the
// tints water takes on the table. Water is not light: it draws in ordinary
// alpha, clear through its body, darker where it bends the light, and bright
// only where it catches the light above the table's top left.

import { BufferGeometry, Float32BufferAttribute, ShaderMaterial, Vector3 } from "three";

import { VALUE_NOISE_GLSL } from "./glslChunks.ts";
import { count, NORMAL, type Particle, rand, type Vec2, type Vec3 } from "./vfxParticles.ts";

/** The colour water shows over the board, and the paler cast of its spray. */
export const WATER: Vec3 = [0.5, 0.74, 0.88];
export const MIST: Vec3 = [0.8, 0.88, 0.93];
/** Where the light above the table comes from, on screen (world y up). */
const LIGHT_2D: Vec2 = [-0.55, 0.83];

const jetVert = /* glsl */ `
  attribute float aAcross, aAlong; varying float vAcross, vAlong;
  void main() { vAcross = aAcross; vAlong = aAlong; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`;

// A column of water seen from above: clear through its middle, darker toward
// its edges, a highlight along the side facing the light, and streaks of
// white water racing along it. Its edges churn, and it rounds off at its head.
const jetFrag = /* glsl */ `
  uniform float uHead, uTail, uTime, uLength, uHeadPx, uLitSide, uGain; uniform vec3 uTint;
  varying float vAcross, vAlong;
  ${VALUE_NOISE_GLSL}
  void main() {
    if (vAlong > uHead || vAlong < uTail) discard;
    float taper = sqrt(clamp((uHead - vAlong) * uLength / uHeadPx, 0.0, 1.0));
    float churn = 0.78 + 0.32 * vnoise(vec2(vAlong * uLength * 0.07 - uTime * 22.0, sign(vAcross) * 5.0));
    float x = abs(vAcross) / max(taper * churn, 0.001);
    if (x >= 1.0) discard;
    float edge = smoothstep(0.55, 1.0, x);
    float flow = vnoise(vec2(vAlong * uLength * 0.05 - uTime * 16.0, vAcross * 3.0));
    float streak = smoothstep(0.5, 0.8, flow) * (1.0 - 0.6 * x);
    float glint = exp(-pow((vAcross - uLitSide * 0.4) / 0.18, 2.0));
    vec3 col = mix(mix(uTint, uTint * 0.7, edge), vec3(1.0), clamp(glint * 0.85 + streak * 0.7, 0.0, 1.0));
    float body = 1.0 - smoothstep(0.85, 1.0, x);
    gl_FragColor = vec4(col, body * (0.3 + 0.3 * edge + 0.55 * glint + 0.45 * streak) * uGain);
  }`;

/** A jet's path: a quadratic bow from `S` through control `C` to `T`. */
export interface JetPath {
  S: Vec2;
  C: Vec2;
  T: Vec2;
}

const bezier = (a: number, b: number, c: number, u: number) => (1 - u) * (1 - u) * a + 2 * (1 - u) * u * b + u * u * c;

/** The point `u` of the way along `path`. */
export function jetPoint({ S, C, T }: JetPath, u: number): Vec2 {
  return [bezier(S[0], C[0], T[0], u), bezier(S[1], C[1], T[1], u)];
}

/** The length of `path`, in px. */
export function jetLength(path: JetPath): number {
  let length = 0;
  let previous = path.S;
  for (let i = 1; i <= 16; i++) {
    const point = jetPoint(path, i / 16);
    length += Math.hypot(point[0] - previous[0], point[1] - previous[1]);
    previous = point;
  }
  return length;
}

const JET_SEGMENTS = 48;

/** A strip `halfWidth` either side of `path`; `aAlong` runs 0 → 1 from its
 *  source, so the material draws only the length between its tail and head. */
export function jetGeometry(path: JetPath, halfWidth: number): BufferGeometry {
  const pos: number[] = [];
  const across: number[] = [];
  const along: number[] = [];
  const points = Array.from({ length: JET_SEGMENTS + 1 }, (_, i) => jetPoint(path, i / JET_SEGMENTS));
  const sides = points.map((p, i) => {
    const p0 = points[Math.max(i - 1, 0)];
    const p1 = points[Math.min(i + 1, JET_SEGMENTS)];
    const tl = Math.hypot(p1[0] - p0[0], p1[1] - p0[1]) || 1;
    const nx = -(p1[1] - p0[1]) / tl;
    const ny = (p1[0] - p0[0]) / tl;
    return { l: [p[0] + nx * halfWidth, p[1] + ny * halfWidth], r: [p[0] - nx * halfWidth, p[1] - ny * halfWidth] };
  });
  for (let i = 0; i < JET_SEGMENTS; i++) {
    const a = i / JET_SEGMENTS;
    const b = (i + 1) / JET_SEGMENTS;
    const A = sides[i];
    const B = sides[i + 1];
    const quad: [number[], number, number][] = [
      [A.l, 1, a], [A.r, -1, a], [B.l, 1, b],
      [A.r, -1, a], [B.r, -1, b], [B.l, 1, b],
    ];
    for (const [p, c, s] of quad) {
      pos.push(p[0], p[1], 12);
      across.push(c);
      along.push(s);
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new Float32BufferAttribute(pos, 3));
  geometry.setAttribute("aAcross", new Float32BufferAttribute(across, 1));
  geometry.setAttribute("aAlong", new Float32BufferAttribute(along, 1));
  return geometry;
}

/** The jet's material, for a path of `length` px whose left side (`aAcross`
 *  = 1) faces the light when `litSide` is 1, and faces away at −1. */
export function jetMaterial(clock: { value: number }, length: number, headPx: number, litSide: number): ShaderMaterial {
  return new ShaderMaterial({
    vertexShader: jetVert,
    fragmentShader: jetFrag,
    ...NORMAL,
    uniforms: {
      uHead: { value: 0 },
      uTail: { value: 0 },
      uTime: clock,
      uLength: { value: length },
      uHeadPx: { value: headPx },
      uLitSide: { value: litSide },
      uGain: { value: 1 },
      uTint: { value: new Vector3(...WATER) },
    },
  });
}

/** Which side of a path heading along `dir` faces the light: 1 its left, −1 its right. */
export function litSide(dir: Vec2): number {
  return -dir[1] * LIGHT_2D[0] + dir[0] * LIGHT_2D[1] >= 0 ? 1 : -1;
}

/** Water thrown up where a jet or a wave strikes, at `atS`: drops flung out,
 *  most of them on along `dir` when it has one, and a haze of mist. */
export function splash(
  T: Vec2,
  atS: number,
  dir: Vec2 | null,
  scale: number,
  share: number,
  into: { drops: Particle[]; mist: Particle[] },
) {
  const heading = (): Vec2 => {
    const a = rand(0, Math.PI * 2);
    if (!dir) return [Math.cos(a), Math.sin(a)];
    // Back along the jet and out to either side, as water off a struck surface.
    const x = -dir[0] * 0.4 + Math.cos(a);
    const y = -dir[1] * 0.4 + Math.sin(a);
    const l = Math.hypot(x, y) || 1;
    return [x / l, y / l];
  };
  for (let i = 0; i < count(70 * scale, share); i++) {
    const [hx, hy] = heading();
    const speed = rand(120, 560) * scale;
    into.drops.push({ pos: [T[0], T[1], 6], vel: [hx * speed, hy * speed, rand(80, 360)], spawn: atS + rand(0, 0.06), life: rand(0.4, 0.8), drag: rand(3, 5), s0: rand(2.5, 5) * scale, s1: rand(3, 6) * scale });
  }
  for (let i = 0; i < count(22, share); i++) {
    const [hx, hy] = heading();
    const speed = rand(60, 200) * scale;
    into.mist.push({ pos: [T[0], T[1], 8], vel: [hx * speed, hy * speed, rand(10, 50)], spawn: atS + rand(0, 0.08), life: rand(0.6, 1.1), drag: 3, s0: rand(14, 22) * scale, s1: rand(50, 90) * scale });
  }
}
