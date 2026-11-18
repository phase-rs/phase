// ─── Dexterity flip: geometry and flight path ───
// PURE module: no WebGL context, no DOM. Like coinGeometry.ts it depends only
// on `three` math/geometry classes, so the tile's shape, its resting
// orientation and the whole flight path are unit-testable under happy-dom.
//
// A dexterity flip drops a permanent's art tile from above, tumbling end over
// end about a horizontal axis, onto a ground plane (XZ, +Y up). The engine owns
// the result — how many complete turns it made and which side landed up — and
// this module only turns that result into a path that ends exactly there.

import { BufferGeometry, ExtrudeGeometry, Float32BufferAttribute, Shape, Vector3 } from "three";

/** Which side of the tile is up when it comes to rest. */
export type DexterityLanding = "faceUp" | "faceDown";

/** The result the animation lands on, as reported by the engine. */
export interface DexterityFlipResult {
  landing: DexterityLanding;
  /** Complete turns the tile made in the air (a whole number ≥ 0). */
  fullTurns: number;
}

/** The tumble axis: horizontal (X), so the tile turns end over end toward the camera. */
export const TUMBLE_AXIS: Vector3 = new Vector3(1, 0, 0);

/** Rotation about TUMBLE_AXIS that lays the tile flat on the ground with the
 *  given side up. The tile is built in the XY plane with its front facing +Z:
 *  -90° turns +Z to +Y (front up), +90° turns -Z to +Y (back up). */
export const LANDING_REST_ANGLE = {
  faceUp: -Math.PI / 2,
  faceDown: Math.PI / 2,
} as const satisfies Record<DexterityLanding, number>;

/** Tile thickness, in tile heights. */
export const TILE_DEPTH = 0.035;
/** Corner radius, in tile heights. */
const TILE_RADIUS = 0.07;
const CORNER_SEGMENTS = 6;

/** Material groups of the tile geometry, in order. */
export const TILE_GROUP = { side: 0, front: 1, back: 2 } as const;

/** Release height above the ground, in tile heights. */
export const RELEASE_HEIGHT = 2.6;

/** Phase lengths at speed 1, in ms. */
export const FLIGHT_MS = {
  /** Held at the release height before letting go. */
  hold: 260,
  /** Free fall from release to first contact. */
  fall: 900,
  /** Bounce and wobble after contact. */
  bounce: 420,
  /** Accent glow rising once the tile is at rest. */
  reveal: 380,
} as const;

/** Total length at speed 1; the tile is at rest from `FLIGHT_MS.hold + fall + bounce`. */
export const FLIGHT_TOTAL_MS = FLIGHT_MS.hold + FLIGHT_MS.fall + FLIGHT_MS.bounce + FLIGHT_MS.reveal;

const BOUNCE_HEIGHT = 0.12;
const WOBBLE_RADIANS = (7 * Math.PI) / 180;
/** A tile that makes no complete turn still flutters a little on the way down. */
const FLUTTER_RADIANS = (18 * Math.PI) / 180;

/**
 * Signed rotation about TUMBLE_AXIS from release to rest. The tile enters face
 * up, so each complete turn adds 2π and landing face down adds the last π.
 * Negative, so the leading edge tips toward the camera as it turns.
 */
export function tumbleRotation({ landing, fullTurns }: DexterityFlipResult): number {
  if (!Number.isInteger(fullTurns) || fullTurns < 0) {
    throw new RangeError(`fullTurns must be a whole number ≥ 0, got ${fullTurns}`);
  }
  const halfTurns = fullTurns * 2 + (landing === "faceDown" ? 1 : 0);
  return -halfTurns * Math.PI;
}

/** One frame of the flight. */
export interface FlightSample {
  /** Height of the tile's centre above the ground, in tile heights. */
  height: number;
  /** Rotation about TUMBLE_AXIS. */
  angle: number;
  /** 0 → 1 across the fall: drives the ground shadow. */
  fall: number;
  /** 0 → 1 across the bounce, 0 before contact: drives the dust ring. */
  impact: number;
  /** 0 → 1 across the reveal: drives the accent glow. */
  reveal: number;
  /** True once the tile is at rest on the ground. */
  settled: boolean;
}

const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/**
 * The tile's state `elapsedMs` after the animation starts. Gravity drops it from
 * rest at the release height, it spins at a constant rate so it touches down at
 * exactly its rest angle, then it bounces once and wobbles in place.
 */
export function sampleFlight(elapsedMs: number, result: DexterityFlipResult, speedMultiplier = 1): FlightSample {
  const scale = speedMultiplier > 0 ? speedMultiplier : 1;
  const hold = FLIGHT_MS.hold * scale;
  const fall = FLIGHT_MS.fall * scale;
  const bounce = FLIGHT_MS.bounce * scale;
  const reveal = FLIGHT_MS.reveal * scale;

  const start = LANDING_REST_ANGLE.faceUp;
  const rotation = tumbleRotation(result);
  const rest = start + rotation;

  if (elapsedMs < hold) {
    return { height: RELEASE_HEIGHT, angle: start, fall: 0, impact: 0, reveal: 0, settled: false };
  }

  const fallProgress = clamp01((elapsedMs - hold) / fall);
  if (fallProgress < 1) {
    // Constant-acceleration drop from rest, constant-rate spin.
    const height = RELEASE_HEIGHT * (1 - fallProgress * fallProgress);
    const flutter = rotation === 0 ? FLUTTER_RADIANS * Math.sin(fallProgress * Math.PI * 2) * (1 - fallProgress) : 0;
    const angle = start + rotation * fallProgress + flutter;
    return { height, angle, fall: fallProgress, impact: 0, reveal: 0, settled: false };
  }

  const bounceProgress = clamp01((elapsedMs - hold - fall) / bounce);
  const decay = 1 - bounceProgress;
  const height = BOUNCE_HEIGHT * Math.sin(bounceProgress * Math.PI) * decay;
  // The wobble keeps the landed side up: it never approaches a quarter turn.
  const angle = rest + WOBBLE_RADIANS * Math.sin(bounceProgress * Math.PI * 3) * decay;
  const revealProgress = clamp01((elapsedMs - hold - fall - bounce) / reveal);
  return {
    height,
    angle,
    fall: 1,
    impact: bounceProgress,
    reveal: revealProgress,
    settled: bounceProgress >= 1,
  };
}

/** How to show an image of `imageAspect` (width / height) inside a tile of
 *  `tileAspect` without stretching it: cropped from the centre, as CSS
 *  `object-fit: cover` does. Values are a texture's `repeat` and `offset`. */
export function coverCrop(imageAspect: number, tileAspect: number): { repeat: [number, number]; offset: [number, number] } {
  if (!(imageAspect > 0) || !(tileAspect > 0)) return { repeat: [1, 1], offset: [0, 0] };
  if (imageAspect > tileAspect) {
    const repeatX = tileAspect / imageAspect;
    return { repeat: [repeatX, 1], offset: [(1 - repeatX) / 2, 0] };
  }
  const repeatY = imageAspect / tileAspect;
  return { repeat: [1, repeatY], offset: [0, (1 - repeatY) / 2] };
}

function roundedRect(width: number, height: number, radius: number): Shape {
  const x = -width / 2;
  const y = -height / 2;
  const shape = new Shape();
  shape.moveTo(x + radius, y);
  shape.lineTo(x + width - radius, y);
  shape.quadraticCurveTo(x + width, y, x + width, y + radius);
  shape.lineTo(x + width, y + height - radius);
  shape.quadraticCurveTo(x + width, y + height, x + width - radius, y + height);
  shape.lineTo(x + radius, y + height);
  shape.quadraticCurveTo(x, y + height, x, y + height - radius);
  shape.lineTo(x, y + radius);
  shape.quadraticCurveTo(x, y, x + radius, y);
  return shape;
}

/**
 * Build the tile (caller owns disposal): a thin rounded rectangle one unit tall
 * and `aspect` wide, centred on the origin, front facing +Z. Material groups
 * are [side, front, back] (`TILE_GROUP`).
 *
 * Each face's UVs span the full texture, laid so the image reads upright from
 * the camera: the front as built, the back once the tile has turned a half
 * turn about TUMBLE_AXIS (which flips screen-up to the tile's -Y).
 */
export function createTileGeometry(aspect: number): BufferGeometry {
  const width = aspect;
  const height = 1;
  const extruded = new ExtrudeGeometry(roundedRect(width, height, TILE_RADIUS), {
    depth: TILE_DEPTH,
    bevelEnabled: false,
    curveSegments: CORNER_SEGMENTS,
  });
  extruded.translate(0, 0, -TILE_DEPTH / 2);

  // ExtrudeGeometry (non-indexed) puts both lids in one group. Sort its
  // triangles by facing into side / front / back buckets so each face takes its
  // own material.
  const position = extruded.getAttribute("position");
  const normal = extruded.getAttribute("normal");

  const buckets: Record<keyof typeof TILE_GROUP, { position: number[]; normal: number[]; uv: number[] }> = {
    side: { position: [], normal: [], uv: [] },
    front: { position: [], normal: [], uv: [] },
    back: { position: [], normal: [], uv: [] },
  };

  for (let first = 0; first < position.count; first += 3) {
    let facing = 0;
    for (let corner = 0; corner < 3; corner++) facing += normal.getZ(first + corner);
    const kind: keyof typeof TILE_GROUP = facing > 2.9 ? "front" : facing < -2.9 ? "back" : "side";
    const bucket = buckets[kind];
    for (let corner = 0; corner < 3; corner++) {
      const vertex = first + corner;
      const x = position.getX(vertex);
      const y = position.getY(vertex);
      bucket.position.push(x, y, position.getZ(vertex));
      bucket.normal.push(normal.getX(vertex), normal.getY(vertex), normal.getZ(vertex));
      const u = x / width + 0.5;
      const v = y / height + 0.5;
      bucket.uv.push(u, kind === "back" ? 1 - v : v);
    }
  }
  extruded.dispose();

  const geometry = new BufferGeometry();
  const order = ["side", "front", "back"] as const;
  const positions: number[] = [];
  const normals: number[] = [];
  const uvs: number[] = [];
  for (const kind of order) {
    const bucket = buckets[kind];
    geometry.addGroup(positions.length / 3, bucket.position.length / 3, TILE_GROUP[kind]);
    positions.push(...bucket.position);
    normals.push(...bucket.normal);
    uvs.push(...bucket.uv);
  }
  geometry.setAttribute("position", new Float32BufferAttribute(positions, 3));
  geometry.setAttribute("normal", new Float32BufferAttribute(normals, 3));
  geometry.setAttribute("uv", new Float32BufferAttribute(uvs, 2));
  return geometry;
}
