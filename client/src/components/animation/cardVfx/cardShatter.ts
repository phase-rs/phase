// ─── Card shatter ───
// A destroyed permanent cracks in slow motion from an impact point, then
// breaks into Voronoi shards that burst toward the viewer and fall back onto
// the board, ported from the approved Destroy lab (`destroy()`). The shards are
// cut from a texture of the permanent's own surface, so the first frame matches
// the board card it replaces. `full` adds shard shadows and paper flakes.

import {
  BufferGeometry,
  DoubleSide,
  Float32BufferAttribute,
  Group,
  MathUtils,
  Mesh,
  Points,
  ShaderMaterial,
  type Texture,
  Vector2,
  Vector3,
} from "three";

import type { CardPose } from "./cardAnchors.ts";
import type { CardVfxTier } from "./cardFlight.ts";
import type { EffectHost, SceneEffect, SceneEffectKind } from "./cardVfxScene.ts";
import { CORNER_MASK_GLSL, VALUE_NOISE_GLSL } from "./glslChunks.ts";

/** The slow-motion crack, in seconds before pace. */
export const SHATTER_CRACK_S = 0.46;
/** Cracks finish this far into the crack phase; the break follows. */
const CRACK_SPREAD_FRACTION = 0.7;
/** How fast the shards drift while the cracks settle, as a fraction of real time. */
const CRACK_WARP = 0.08;
/** The shards' fall, in seconds before pace. */
export const SHATTER_FALL_S = 0.76;
/** The last flakes outlive the shards by this long, in seconds before pace. */
const SHATTER_TAIL_S = 0.05;
export const SHATTER_SHARDS: Record<CardVfxTier, number> = { full: 38, reduced: 26 };
const FORCE = 320;
const GRAVITY = 2600;
const RIM = 0.55;
const TILT = 0.045;
const THICK = 1.6;
const SHADOW_DIR = new Vector2(0.32, -0.42);
const SHADOW_STRENGTH = 0.3;
const EPS = 0.01;

type Point = [number, number];
type Polygon = Point[];

// ---------- Fracture ----------

function clip(poly: Polygon, px: number, py: number, nx: number, ny: number): Polygon {
  const out: Polygon = [];
  for (let i = 0; i < poly.length; i++) {
    const a = poly[i];
    const b = poly[(i + 1) % poly.length];
    const da = (a[0] - px) * nx + (a[1] - py) * ny;
    const db = (b[0] - px) * nx + (b[1] - py) * ny;
    if (da <= 0) out.push(a);
    if (da <= 0 !== db <= 0) {
      const t = da / (da - db);
      out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
  }
  return out;
}

/** Rings of sites around the impact make cracks radiate from the hit and
 *  splinter small near it; the remaining sites are scattered for large slabs. */
function fractureSites(w: number, h: number, ix: number, iy: number, n: number): Point[] {
  const sites: Point[] = [];
  const inside = (x: number, y: number) => x > 0.5 && x < w - 0.5 && y > 0.5 && y < h - 0.5;
  const span = Math.min(w, h);
  let budget = Math.round(n * 0.7);
  for (const [rf, k] of [[0.07, 6], [0.24, 7], [0.46, 8]]) {
    const off = Math.random() * Math.PI * 2;
    for (let j = 0; j < k && budget > 0; j++) {
      const a = off + (j + (Math.random() - 0.5) * 0.5) * ((Math.PI * 2) / k);
      const r = span * rf * (0.85 + Math.random() * 0.3);
      const x = ix + Math.cos(a) * r;
      const y = iy + Math.sin(a) * r;
      if (inside(x, y)) {
        sites.push([x, y]);
        budget--;
      }
    }
  }
  while (sites.length < n) sites.push([Math.random() * w, Math.random() * h]);
  return sites;
}

function voronoi(w: number, h: number, sites: readonly Point[]): Polygon[] {
  const cells: Polygon[] = [];
  sites.forEach(([sx, sy], i) => {
    let poly: Polygon = [[0, 0], [w, 0], [w, h], [0, h]];
    sites.forEach(([tx, ty], j) => {
      if (i !== j && poly.length) poly = clip(poly, (sx + tx) / 2, (sy + ty) / 2, tx - sx, ty - sy);
    });
    if (poly.length >= 3) cells.push(poly);
  });
  return cells;
}

function onBorder(a: Point, b: Point, w: number, h: number): boolean {
  const same = (i: 0 | 1, v: number) => Math.abs(a[i] - v) < EPS && Math.abs(b[i] - v) < EPS;
  return same(0, 0) || same(0, w) || same(1, 0) || same(1, h);
}

function containsPoint(poly: Polygon, x: number, y: number): boolean {
  let sign = 0;
  for (let i = 0; i < poly.length; i++) {
    const a = poly[i];
    const b = poly[(i + 1) % poly.length];
    const c = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
    if (c === 0) continue;
    if (sign === 0) sign = Math.sign(c);
    else if (Math.sign(c) !== sign) return false;
  }
  return true;
}

/** 1 for a crack running straight out from the impact, 0 for one circling it. */
function radialness(a: Point, b: Point, ix: number, iy: number): number {
  const ex = b[0] - a[0];
  const ey = b[1] - a[1];
  const mx = (a[0] + b[0]) / 2 - ix;
  const my = (a[1] + b[1]) / 2 - iy;
  return Math.abs(ex * mx + ey * my) / (Math.hypot(ex, ey) * Math.hypot(mx, my) || 1);
}

const pointKey = (p: Point) => `${Math.round(p[0] * 20)},${Math.round(p[1] * 20)}`;

/** Both shards that share an edge walk it in opposite directions; this gives
 *  each a sign so the shader can place one wandering centreline both agree on. */
function edgeSide(a: Point, b: Point): number {
  const q = (v: number) => Math.round(v * 20);
  return q(a[0]) < q(b[0]) || (q(a[0]) === q(b[0]) && q(a[1]) < q(b[1])) ? 1 : -1;
}

/** A stable per-edge random number: both shards sharing an edge get the same value. */
function edgeHash(a: Point, b: Point): number {
  const key = [pointKey(a), pointKey(b)].sort().join("|");
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  return (h >>> 0) / 4294967296;
}

interface CrackNode {
  p: Point;
  adj: [CrackNode, number][];
  dist: number;
  done: boolean;
}

/** Crack arrival, 0–1: the shortest path from the impact along the fracture
 *  graph, so each crack grows along its own line instead of a circular wipe. */
function crackArrival(cells: readonly Polygon[], w: number, h: number, ix: number, iy: number) {
  const nodes = new Map<string, CrackNode>();
  const node = (p: Point) => {
    const key = pointKey(p);
    let found = nodes.get(key);
    if (!found) {
      found = { p, adj: [], dist: Infinity, done: false };
      nodes.set(key, found);
    }
    return found;
  };
  for (const poly of cells) {
    poly.forEach((a, k) => {
      const b = poly[(k + 1) % poly.length];
      const na = node(a);
      const nb = node(b);
      if (onBorder(a, b, w, h) || na === nb) return;
      // Radial cracks shoot out first; cracks circling the impact join them later.
      const len =
        Math.hypot(b[0] - a[0], b[1] - a[1]) * (1 + Math.random() * 0.35) * (1 + 1.6 * (1 - radialness(a, b, ix, iy)));
      na.adj.push([nb, len]);
      nb.adj.push([na, len]);
    });
  }
  const start = cells.find((poly) => containsPoint(poly, ix, iy)) ?? cells[0];
  for (const p of start) {
    const n = node(p);
    n.dist = Math.min(n.dist, Math.hypot(p[0] - ix, p[1] - iy));
  }
  const all = [...nodes.values()];
  for (;;) {
    let best: CrackNode | null = null;
    for (const n of all) if (!n.done && n.dist < Infinity && (!best || n.dist < best.dist)) best = n;
    if (!best) break;
    best.done = true;
    for (const [m, len] of best.adj) m.dist = Math.min(m.dist, best.dist + len);
  }
  let max = 0;
  for (const n of all) {
    if (n.dist === Infinity) n.dist = Math.hypot(n.p[0] - ix, n.p[1] - iy) * 1.3;
    max = Math.max(max, n.dist);
  }
  return (p: Point) => node(p).dist / (max || 1);
}

const SHARD_ATTRIBUTES = {
  position: 3, normal: 3, aCard: 2, aFace: 1, aEdge: 1, aArrive: 1, aShardArrive: 1, aSide: 1,
  aRadial: 1, aCenter: 2, aVel: 3, aAxis: 3, aSpin: 1, aDelay: 1,
} as const;

type ShardAttribute = keyof typeof SHARD_ATTRIBUTES;
/** A fracture edge in card px with its crack arrival, where flakes spawn. */
type FractureEdge = [number, number, number, number, number];

/** One merged, non-indexed geometry holding every shard of a `w`×`h` card
 *  (card px, y down) broken at (`ix`, `iy`): each shard's top, bottom and rim. */
export function buildShards(w: number, h: number, ix: number, iy: number, n: number) {
  const cells = voronoi(w, h, fractureSites(w, h, ix, iy, n));
  const arrive = crackArrival(cells, w, h, ix, iy);
  const data = Object.fromEntries(Object.keys(SHARD_ATTRIBUTES).map((name) => [name, [] as number[]])) as Record<
    ShardAttribute,
    number[]
  >;
  const edges: FractureEdge[] = [];
  const maxD = Math.hypot(Math.max(ix, w - ix), Math.max(iy, h - iy));
  const axis = new Vector3();
  for (const poly of cells) {
    const cx = poly.reduce((sum, p) => sum + p[0], 0) / poly.length;
    const cy = poly.reduce((sum, p) => sum + p[1], 0) / poly.length;
    const dx = cx - ix;
    const dy = cy - iy;
    const d = Math.hypot(dx, dy) || 1;
    const near = 1 - d / maxD;
    const shardArrive = Math.min(...poly.map(arrive));
    // Card space is y-down; velocities are stored y-up. Shards burst toward the
    // viewer and land back on the board.
    const out = FORCE * (0.22 + near * 0.45) * (0.7 + Math.random() * 0.6);
    const vel = [(dx / d) * out, -(dy / d) * out, FORCE * (1.5 + near * 0.9) * (0.75 + Math.random() * 0.5)];
    axis.set(Math.random() - 0.5, Math.random() - 0.5, (Math.random() < 0.5 ? -1 : 1) * (0.5 + Math.random() * 0.8)).normalize();
    const spin = (2 + Math.random() * 5) * (Math.random() < 0.5 ? -1 : 1) * (0.6 + near * 0.6);
    let side = 1;
    let radial = 1;
    const push = (x: number, y: number, z: number, normal: readonly number[], face: number, edge: number, arrival: number) => {
      data.position.push(x - cx, -(y - cy), z);
      data.normal.push(...normal);
      data.aCard.push(x, y);
      data.aFace.push(face);
      data.aEdge.push(edge);
      data.aArrive.push(arrival);
      data.aShardArrive.push(shardArrive);
      data.aSide.push(side);
      data.aRadial.push(radial);
      data.aCenter.push(cx, cy);
      data.aVel.push(...vel);
      data.aAxis.push(axis.x, axis.y, axis.z);
      data.aSpin.push(spin);
      data.aDelay.push(shardArrive * 0.05);
    };
    poly.forEach((a, k) => {
      const b = poly[(k + 1) % poly.length];
      const ex = b[0] - a[0];
      const ey = b[1] - a[1];
      // The card's own outline is not a crack: no line there.
      const border = onBorder(a, b, w, h);
      // Centroid→edge distance; its barycentric weight times this is the px distance to edge ab.
      const toEdge = border ? 1e4 : Math.abs((cx - a[0]) * ey - (cy - a[1]) * ex) / (Math.hypot(ex, ey) || 1);
      side = edgeSide(a, b);
      radial = radialness(a, b, ix, iy);
      // Only part of the fracture cracks visibly before the break: most edges near
      // the hit and the long radial runs. The rest give way at the shatter.
      const md = Math.hypot((a[0] + b[0]) / 2 - ix, (a[1] + b[1]) / 2 - iy) / maxD;
      const shows = edgeHash(a, b) < Math.min(Math.max(1.2 - md * 1.5 + radial * 0.45, 0.08), 1);
      const ta = shows ? arrive(a) : 2;
      const tb = shows ? arrive(b) : 2;
      const tc = Math.max(ta, tb);
      const top = THICK / 2;
      push(cx, cy, top, [0, 0, 1], 0, toEdge, tc);
      push(b[0], b[1], top, [0, 0, 1], 0, 0, tb);
      push(a[0], a[1], top, [0, 0, 1], 0, 0, ta);
      push(cx, cy, -top, [0, 0, -1], 1, toEdge, tc);
      push(a[0], a[1], -top, [0, 0, -1], 1, 0, ta);
      push(b[0], b[1], -top, [0, 0, -1], 1, 0, tb);
      const len = Math.hypot(ex, ey) || 1;
      const rim = [ey / len, ex / len, 0];
      push(a[0], a[1], top, rim, 2, 0, 0);
      push(b[0], b[1], top, rim, 2, 0, 0);
      push(b[0], b[1], -top, rim, 2, 0, 0);
      push(a[0], a[1], top, rim, 2, 0, 0);
      push(b[0], b[1], -top, rim, 2, 0, 0);
      push(a[0], a[1], -top, rim, 2, 0, 0);
      if (!border) edges.push([a[0], a[1], b[0], b[1], Math.min(arrive(a), arrive(b))]);
    });
  }
  const geometry = new BufferGeometry();
  for (const [name, size] of Object.entries(SHARD_ATTRIBUTES)) {
    geometry.setAttribute(name, new Float32BufferAttribute(data[name as ShardAttribute], size));
  }
  return { geometry, edges };
}

// ---------- Shaders ----------

// Shared by the shard pass and its shadow pass so both move identically.
const shardMotion = /* glsl */ `
  attribute vec2 aCard; attribute float aFace; attribute float aEdge; attribute float aArrive; attribute float aShardArrive;
  attribute float aSide; attribute float aRadial; attribute vec2 aCenter; attribute vec3 aVel; attribute vec3 aAxis; attribute float aSpin; attribute float aDelay;
  uniform float uTime, uFall, uGravity, uProg, uTilt;
  uniform vec2 uImpact;
  vec3 rot(vec3 v, vec3 k, float a) { float c = cos(a), s = sin(a); return v * c + cross(k, v) * s + k * dot(k, v) * (1.0 - c); }
  vec3 shardWorld(out vec3 nW, out float life) {
    vec2 d2 = aCenter - uImpact;
    float dl = length(d2);
    vec2 dir = dl > 0.001 ? d2 / dl : vec2(1.0, 0.0);
    vec3 dirW = vec3(dir.x, -dir.y, 0.0);
    // While the crack reaches it, each shard lifts its outer edge a little so light picks out the break.
    float reached = smoothstep(aShardArrive, aShardArrive + 0.12, uProg);
    vec3 tiltAxis = vec3(dirW.y, -dirW.x, 0.0);
    float tilt = uTilt * reached;
    float t = max(uTime - aDelay, 0.0);
    life = clamp(t / uFall, 0.0, 1.0);
    float g = uGravity, z, spinA;
    vec2 xy;
    // Top-down: burst toward the viewer, land on the board, one small hop, slide to rest.
    float vz = aVel.z, tl = 2.0 * vz / g;
    if (t < tl) { z = vz * t - 0.5 * g * t * t; xy = aVel.xy * t; spinA = aSpin * t; }
    else {
      float tau = t - tl, vz1 = vz * 0.22, tl1 = 2.0 * vz1 / g;
      z = tau < tl1 ? vz1 * tau - 0.5 * g * tau * tau : 0.0;
      float slide = (1.0 - exp(-5.0 * tau)) / 5.0;
      xy = aVel.xy * (tl + 0.45 * slide);
      spinA = aSpin * (tl + 0.35 * slide);
    }
    float shrink = 1.0 - 0.25 * smoothstep(0.7, 1.0, life);
    vec3 p = rot(rot(position, tiltAxis, tilt), aAxis, spinA) * shrink;
    vec3 n = rot(rot(normal, tiltAxis, tilt), aAxis, spinA);
    vec3 local = p + vec3(aCenter.x, -aCenter.y, 0.0) + vec3(0.0, 0.0, 1.5 * reached) + vec3(xy, z);
    vec4 w = modelMatrix * vec4(local, 1.0);
    nW = normalize(mat3(modelMatrix) * n);
    return w.xyz;
  }`;

const shardVert = /* glsl */ `
  ${shardMotion}
  varying vec2 vCard; varying float vFace, vEdge, vArrive, vShade, vSpec, vAlpha, vSide, vRadial;
  void main() {
    vec3 nW; float life;
    vSide = aSide; vRadial = aRadial;
    vec3 w = shardWorld(nW, life);
    vec3 L = normalize(vec3(-0.35, 0.55, 0.76));
    vec3 H = normalize(L + vec3(0.0, 0.0, 1.0));
    vec3 rest = vec3(0.0, 0.0, aFace > 0.5 && aFace < 1.5 ? -1.0 : 1.0);
    // Lit relative to rest, so the unbroken card matches the DOM exactly.
    vShade = 1.0 + 0.6 * (dot(nW, L) - dot(rest, L));
    vSpec = aFace < 0.5 ? max(pow(max(dot(nW, H), 0.0), 48.0) - pow(H.z, 48.0), 0.0) * 0.55 : 0.0;
    vAlpha = 1.0 - smoothstep(0.62, 1.0, life);
    vCard = aCard; vFace = aFace; vEdge = aEdge; vArrive = aArrive;
    gl_Position = projectionMatrix * viewMatrix * vec4(w, 1.0);
  }`;

const shardFrag = /* glsl */ `
  uniform sampler2D uMap; uniform vec2 uSize, uImpact; uniform float uRadius, uProg, uRim, uDrain, uCrazeR;
  varying vec2 vCard; varying float vFace, vEdge, vArrive, vShade, vSpec, vAlpha, vSide, vRadial;
  ${CORNER_MASK_GLSL}
  ${VALUE_NOISE_GLSL}
  vec2 hash22(vec2 p) { float h = hash21(p); return vec2(h, hash21(p + h + 19.19)); }
  // Worley F2 - F1: small near cell borders, which draws a fine crack network.
  float cellEdge(vec2 p) {
    vec2 i = floor(p), f = fract(p);
    float d1 = 8.0, d2 = 8.0;
    for (int y = -1; y <= 1; y++) for (int x = -1; x <= 1; x++) {
      vec2 g = vec2(float(x), float(y));
      vec2 r = g + hash22(i + g) - f;
      float d = dot(r, r);
      if (d < d1) { d2 = d1; d1 = d; } else if (d < d2) { d2 = d; }
    }
    return sqrt(d2) - sqrt(d1);
  }
  void main() {
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    vec3 col;
    if (vFace < 0.5) {
      col = texture2D(uMap, vec2(vCard.x / uSize.x, 1.0 - vCard.y / uSize.y)).rgb;
      float grey = dot(col, vec3(0.2126, 0.7152, 0.0722));
      col = mix(col, vec3(grey) * 0.9, uDrain);
      vec3 paper = vec3(0.62, 0.58, 0.52);
      // Crazing: a fine crack network around the hit, spreading quickly.
      float dImp = distance(vCard, uImpact);
      float crazeReach = 1.0 - smoothstep(uProg * uCrazeR * 1.6, uProg * uCrazeR * 1.6 + 3.0, dImp);
      float crazeMask = (1.0 - smoothstep(uCrazeR * 0.25, uCrazeR, dImp)) * crazeReach;
      float ce = cellEdge(vCard / 6.5) * 6.5 * 0.5;
      col = mix(col, vec3(0.004), (1.0 - smoothstep(0.1, 0.4, ce)) * crazeMask * 0.5);
      // Stress-whitened card stock right at the impact.
      float bruise = (1.0 - smoothstep(1.5, 7.0, dImp)) * step(0.001, uProg) * (0.4 + 0.6 * vnoise(vCard * 0.9));
      col = mix(col, paper, bruise * 0.55 * uRim);
      // Main cracks wander around the straight fracture edge; both neighbouring shards
      // read the same noise, so they agree on one centreline.
      float wob = (vnoise(vCard * 0.32) - 0.5) * 2.4 + (vnoise(vCard * 1.4 + 17.0) - 0.5) * 0.5;
      float s = abs(vSide * vEdge - wob);
      float on = step(vArrive, uProg);
      float width = mix(0.7, 0.28, clamp(vArrive, 0.0, 1.0)) * mix(0.55, 1.0, vRadial);
      float line = 1.0 - smoothstep(width * 0.35, width, s);
      // Paper shows in broken patches along the lip, not as a continuous outline.
      float chip = smoothstep(0.4, 0.75, vnoise(vCard * 0.25 + 5.0));
      float lip = (1.0 - smoothstep(width, width + 1.2, s)) * (1.0 - line) * chip;
      col = mix(col, paper, lip * on * uRim);
      col = mix(col, vec3(0.002), line * on * 0.92);
    } else if (vFace < 1.5) {
      col = vec3(0.075, 0.045, 0.024);
    } else {
      col = vec3(0.42, 0.39, 0.34);
    }
    col = col * clamp(vShade, 0.3, 1.5) + vSpec;
    gl_FragColor = vec4(col, vAlpha * corner);
    #include <colorspace_fragment>
  }`;

const shadowVert = /* glsl */ `
  ${shardMotion}
  uniform vec2 uShadowDir;
  varying vec2 vCard; varying float vFace, vAlpha;
  void main() {
    vec3 nW; float life;
    vec3 w = shardWorld(nW, life);
    float height = max(w.z, 0.0);
    w.xy += uShadowDir * height;
    w.z = 0.0;
    // Higher shards cast fainter shadows; the resting card casts none of its own.
    vAlpha = smoothstep(1.0, 8.0, height) * (1.0 - smoothstep(60.0, 420.0, height)) * (1.0 - smoothstep(0.62, 1.0, life));
    vCard = aCard; vFace = aFace;
    gl_Position = projectionMatrix * viewMatrix * vec4(w, 1.0);
  }`;

const shadowFrag = /* glsl */ `
  uniform vec2 uSize; uniform float uRadius, uStrength;
  varying vec2 vCard; varying float vFace, vAlpha;
  ${CORNER_MASK_GLSL}
  void main() {
    if (vFace > 0.5) discard;
    float corner = cornerMask(vCard, uSize, uRadius);
    if (corner <= 0.0) discard;
    gl_FragColor = vec4(0.0, 0.0, 0.0, vAlpha * corner * uStrength);
  }`;

// Paper flakes thrown off along the fracture lines.
const flakeVert = /* glsl */ `
  attribute vec3 aVel; attribute float aLife; attribute float aSize; attribute float aDelay;
  uniform float uTime, uGravity, uDpr;
  varying float vA;
  void main() {
    float t = max(uTime - aDelay, 0.0);
    float life = clamp(t / aLife, 0.0, 1.0);
    vec3 p = position + vec3(aVel.xy * t, 0.0);
    p.z += max(aVel.z * t - 0.5 * uGravity * t * t, 0.0);
    vA = step(0.0001, uTime - aDelay) * (1.0 - life);
    gl_PointSize = aSize * uDpr;
    gl_Position = projectionMatrix * viewMatrix * modelMatrix * vec4(p, 1.0);
  }`;

const flakeFrag = /* glsl */ `
  varying float vA;
  void main() {
    vec2 q = abs(gl_PointCoord - 0.5);
    if (q.x + q.y > 0.6) discard;
    gl_FragColor = vec4(0.42, 0.39, 0.34, vA * 0.85);
    #include <colorspace_fragment>
  }`;

function buildFlakes(edges: readonly FractureEdge[], count: number): BufferGeometry {
  const position: number[] = [];
  const velocity: number[] = [];
  const life: number[] = [];
  const size: number[] = [];
  const delay: number[] = [];
  for (let i = 0; i < count; i++) {
    const [ax, ay, bx, by, arrival] = edges[Math.floor(Math.random() * edges.length)];
    const t = Math.random();
    position.push(ax + (bx - ax) * t, -(ay + (by - ay) * t), 1);
    const a = Math.random() * Math.PI * 2;
    const speed = FORCE * (0.3 + Math.random() * 0.8);
    velocity.push(Math.cos(a) * speed, Math.sin(a) * speed, FORCE * (1 + Math.random() * 1.2));
    life.push(0.3 + Math.random() * 0.4);
    size.push(1.2 + Math.random() * 1.6);
    delay.push(arrival * 0.05);
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new Float32BufferAttribute(position, 3));
  geometry.setAttribute("aVel", new Float32BufferAttribute(velocity, 3));
  geometry.setAttribute("aLife", new Float32BufferAttribute(life, 1));
  geometry.setAttribute("aSize", new Float32BufferAttribute(size, 1));
  geometry.setAttribute("aDelay", new Float32BufferAttribute(delay, 1));
  return geometry;
}

// ---------- Effect ----------

export interface CardShatterParams {
  /** The permanent's pose on the board: its layout size and rotation. */
  pose: CardPose;
  /** The permanent's surface, drawn at its layout size. */
  surface: Texture;
  /** The corner radius of the surface, in CSS px. */
  radius: number;
  /** Where the break starts, as fractions of the card's width and height. */
  impact: { u: number; v: number };
  tier: CardVfxTier;
  pace: number;
  pixelRatio: number;
  /** When the break starts, on the frame clock (`performance.now()`); until
   *  then the card lies whole. `null` starts it on its first frame. */
  startMs: number | null;
  /** Runs once the last shard has faded. */
  onDone(): void;
}

function motionUniforms(w: number, h: number, impact: Vector2) {
  return {
    uTime: { value: 0 },
    uFall: { value: SHATTER_FALL_S },
    uGravity: { value: GRAVITY },
    uProg: { value: 0 },
    uTilt: { value: TILT },
    uImpact: { value: impact },
    uSize: { value: new Vector2(w, h) },
  };
}

function createShardMaterial(surface: Texture, w: number, h: number, radius: number, impact: Vector2) {
  return new ShaderMaterial({
    vertexShader: shardVert,
    fragmentShader: shardFrag,
    transparent: true,
    side: DoubleSide,
    uniforms: {
      ...motionUniforms(w, h, impact),
      uMap: { value: surface },
      uRadius: { value: radius },
      uRim: { value: RIM },
      uDrain: { value: 0 },
      uCrazeR: { value: Math.min(w, h) * 0.22 },
    },
  });
}

function createShadowMaterial(uniforms: ShaderMaterial["uniforms"]) {
  return new ShaderMaterial({
    vertexShader: shadowVert,
    fragmentShader: shadowFrag,
    transparent: true,
    depthWrite: false,
    depthTest: false,
    side: DoubleSide,
    // Shares the shard pass's motion uniforms, so the shadow follows its shard.
    uniforms: { ...uniforms, uShadowDir: { value: SHADOW_DIR }, uStrength: { value: SHADOW_STRENGTH } },
  });
}

function createFlakeMaterial(uniforms: ShaderMaterial["uniforms"], pixelRatio: number) {
  return new ShaderMaterial({
    vertexShader: flakeVert,
    fragmentShader: flakeFrag,
    transparent: true,
    depthWrite: false,
    uniforms: { uTime: uniforms.uTime, uGravity: uniforms.uGravity, uDpr: { value: pixelRatio } },
  });
}

export const cardShatterKind: SceneEffectKind = {
  // One shard with every pass's material, so init compiles each program.
  warmUp(host: EffectHost) {
    const { geometry, edges } = buildShards(4, 4, 2, 2, 2);
    const shards = new Mesh(geometry, createShardMaterial(host.placeholderTexture, 4, 4, 0, new Vector2(2, 2)));
    shards.name = "card-shatter-warmup";
    const shadow = new Mesh(geometry, createShadowMaterial(shards.material.uniforms));
    shadow.name = "card-shatter-shadow-warmup";
    const flakes = new Points(buildFlakes(edges, 1), createFlakeMaterial(shards.material.uniforms, 1));
    flakes.name = "card-shatter-flakes-warmup";
    return [shards, shadow, flakes];
  },
};

class CardShatter implements SceneEffect {
  private readonly group = new Group();
  private startMs: number | null;
  private readonly shards: Mesh<BufferGeometry, ShaderMaterial>;
  private readonly extras: (Mesh<BufferGeometry, ShaderMaterial> | Points<BufferGeometry, ShaderMaterial>)[] = [];

  constructor(
    private readonly host: EffectHost,
    private readonly params: CardShatterParams,
  ) {
    this.startMs = params.startMs;
    const { pose, surface, radius, impact, tier, pixelRatio } = params;
    const { w, h } = pose;
    const ix = impact.u * w;
    const iy = impact.v * h;
    const { geometry, edges } = buildShards(w, h, ix, iy, SHATTER_SHARDS[tier]);
    const impactPoint = new Vector2(ix, iy);
    this.shards = new Mesh(geometry, createShardMaterial(surface, w, h, radius, impactPoint));
    this.shards.name = "card-shatter-shards";
    this.shards.position.set(-w / 2, h / 2, 0);
    this.shards.frustumCulled = false;
    this.shards.renderOrder = 1;
    this.group.add(this.shards);
    if (tier === "full") {
      const shadow = new Mesh(geometry, createShadowMaterial(this.shards.material.uniforms));
      shadow.name = "card-shatter-shadow";
      shadow.renderOrder = 0;
      const flakes = new Points(
        buildFlakes(edges, Math.round(SHATTER_SHARDS.full * 1.6)),
        createFlakeMaterial(this.shards.material.uniforms, pixelRatio),
      );
      flakes.name = "card-shatter-flakes";
      flakes.renderOrder = 2;
      for (const extra of edges.length ? [shadow, flakes] : [shadow]) {
        extra.position.copy(this.shards.position);
        extra.frustumCulled = false;
        this.group.add(extra);
        this.extras.push(extra);
      }
    }
    // Canvas-local CSS px, y flipped; the pose angle turns clockwise on screen.
    this.group.position.set(pose.x, -pose.y, 0);
    this.group.rotation.z = -MathUtils.degToRad(pose.angleDeg);
    this.group.name = "card-shatter";
    host.scene.add(this.group);
  }

  update(nowMs: number): boolean {
    this.startMs ??= nowMs;
    const t = Math.max(0, (nowMs - this.startMs) / 1000 / this.params.pace);
    const spreadS = SHATTER_CRACK_S * CRACK_SPREAD_FRACTION;
    // The cracks spread on a near-frozen clock; the break then runs in real time.
    const sim =
      t < spreadS
        ? 0
        : t < SHATTER_CRACK_S
          ? (t - spreadS) * CRACK_WARP
          : (SHATTER_CRACK_S - spreadS) * CRACK_WARP + (t - SHATTER_CRACK_S);
    const uniforms = this.shards.material.uniforms;
    uniforms.uTime.value = sim;
    uniforms.uProg.value = Math.min(t / spreadS, 1) ** 0.8;
    uniforms.uDrain.value = 0.3 * Math.min(t / SHATTER_CRACK_S, 1);
    // A short recoil into the table on the hit.
    this.group.scale.setScalar(1 - 0.018 * Math.sin(Math.min(t / 0.09, 1) * Math.PI));
    return t < SHATTER_CRACK_S + SHATTER_FALL_S + SHATTER_TAIL_S;
  }

  dispose(silent: boolean) {
    this.host.scene.remove(this.group);
    this.shards.geometry.dispose();
    this.shards.material.dispose();
    this.params.surface.dispose();
    for (const extra of this.extras) {
      if (extra.geometry !== this.shards.geometry) extra.geometry.dispose();
      extra.material.dispose();
    }
    if (!silent) this.params.onDone();
  }
}

export function createCardShatter(host: EffectHost, params: CardShatterParams): SceneEffect {
  return new CardShatter(host, params);
}
