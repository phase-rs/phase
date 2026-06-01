// ─── 3D Die Geometry Registry ───
// PURE module: no WebGL context, no DOM. Depends only on `three` math/geometry
// classes so it is unit-testable under happy-dom without a GPU.
//
// Each supported die type maps to a `DieDescriptor` that knows:
//   - how to build a fresh geometry instance,
//   - the outward unit normal of each face (in the geometry's local space),
//   - which number is printed on each face,
//   - the quaternion that brings the face showing a requested value to CAMERA_UP.
//
// Supported v1 dice: d4, d6, d8, d12, d20 (three.js built-in polyhedra).
// d10 / d100 (pentagonal trapezohedron) are OUT OF SCOPE — `dieDescriptor`
// returns a graceful fallback descriptor for any unsupported side count.

import {
  BoxGeometry,
  BufferGeometry,
  DodecahedronGeometry,
  IcosahedronGeometry,
  OctahedronGeometry,
  Quaternion,
  SphereGeometry,
  TetrahedronGeometry,
  Vector3,
} from "three";

/**
 * The canonical "up" a settled die's result face points to after the roll
 * resolves, in the die's own (camera-agnostic) space. We use +Y. `Dice3D` then
 * composes a further view-facing rotation so the result face turns toward its
 * (oblique) camera and the number reads head-on — keeping this module free of
 * any camera knowledge. All `orientationFor` results rotate the labelled face's
 * normal onto this vector.
 */
export const CAMERA_UP: Vector3 = new Vector3(0, 1, 0);

/** Squared-cosine threshold for treating two normals as antipodal. */
const ANTIPODAL_DOT = -0.999999;

/** Cosine tolerance for grouping coplanar triangles onto the same face. */
const FACE_NORMAL_EPS = 1e-3;

export interface DieDescriptor {
  sides: number;
  /** Build a fresh geometry instance (caller owns disposal). */
  geometry(): BufferGeometry;
  /** Number shown on each face: `faceValue[i]` is the pip count on face `i`. */
  faceValue: number[];
  /** Outward unit normal of face `i`, in the geometry's local space. */
  faceNormal(faceIndex: number): Vector3;
  /** Quaternion rotating the mesh so the face showing `value` points at CAMERA_UP. */
  orientationFor(value: number): Quaternion;
  /** How many physical dice represent one rolled value (1 for all v1 dice). */
  renderCount: number;
}

/**
 * Derive one outward unit normal per geometric face from a (non-indexed)
 * geometry's position attribute.
 *
 * Each triangle contributes its geometric normal (the normalized cross product
 * of two edges). Coplanar triangles of the same flat face share that normal
 * exactly, so the box's two triangles per square side and the dodecahedron's
 * three triangles per pentagon collapse into one face normal. (A centroid-
 * direction heuristic does NOT work here: a square/pentagon split into
 * triangles produces triangle centroids that do not lie on the face's outward
 * ray, so the box reports 12 and the dodecahedron 36 spurious faces.)
 *
 * For the origin-centered convex polyhedra used as dice, three.js emits
 * triangle vertices counter-clockwise when viewed from outside, so the cross
 * product already points outward — no centroid-sign correction is needed.
 *
 * Faces are returned sorted by a stable lexicographic key on the rounded
 * normal components, giving a deterministic 1..N numbering across runs.
 */
function deriveFaceNormals(geometry: BufferGeometry): Vector3[] {
  const nonIndexed = geometry.index ? geometry.toNonIndexed() : geometry;
  const pos = nonIndexed.getAttribute("position");
  const triCount = pos.count / 3;

  const groups: Vector3[] = [];
  const a = new Vector3();
  const b = new Vector3();
  const c = new Vector3();
  const edge1 = new Vector3();
  const edge2 = new Vector3();
  const normal = new Vector3();

  for (let t = 0; t < triCount; t++) {
    a.fromBufferAttribute(pos, t * 3 + 0);
    b.fromBufferAttribute(pos, t * 3 + 1);
    c.fromBufferAttribute(pos, t * 3 + 2);
    edge1.subVectors(b, a);
    edge2.subVectors(c, a);
    normal.crossVectors(edge1, edge2);

    // Skip degenerate triangles (zero-area, no defined normal).
    if (normal.lengthSq() === 0) continue;
    const dir = normal.clone().normalize();

    const existing = groups.find((g) => g.dot(dir) > 1 - FACE_NORMAL_EPS);
    if (!existing) groups.push(dir);
  }

  // If we created a non-indexed copy, free it — the source geometry is the
  // caller's to keep; the copy was a transient working buffer.
  if (nonIndexed !== geometry) nonIndexed.dispose();

  // Deterministic ordering so face numbering is stable across runs.
  groups.sort((u, v) => {
    const ku = `${u.x.toFixed(4)},${u.y.toFixed(4)},${u.z.toFixed(4)}`;
    const kv = `${v.x.toFixed(4)},${v.y.toFixed(4)},${v.z.toFixed(4)}`;
    return ku < kv ? -1 : ku > kv ? 1 : 0;
  });

  return groups;
}

/**
 * Quaternion that rotates `from` onto `to`, guarding the antipodal degeneracy
 * where `Quaternion.setFromUnitVectors` is undefined (the two vectors are
 * exactly opposite). In that case we rotate 180° about any axis perpendicular
 * to `to`, which sends `from` onto `to` without producing a NaN quaternion.
 */
function rotationBetween(from: Vector3, to: Vector3): Quaternion {
  const dot = from.dot(to);
  if (dot < ANTIPODAL_DOT) {
    // Pick an axis perpendicular to `to`. Cross with a non-parallel basis
    // vector; fall back to a second basis vector if the first is parallel.
    let axis = new Vector3(1, 0, 0).cross(to);
    if (axis.lengthSq() < 1e-6) axis = new Vector3(0, 0, 1).cross(to);
    axis.normalize();
    return new Quaternion().setFromAxisAngle(axis, Math.PI);
  }
  return new Quaternion().setFromUnitVectors(from, to);
}

/**
 * Build a descriptor for a geometry whose faces are derived generically from
 * its triangle data. `valueForFace` assigns the printed number for face `i`
 * (default: `i + 1`, i.e. faces labelled 1..N in deterministic normal order).
 */
function buildDescriptor(
  sides: number,
  makeGeometry: () => BufferGeometry,
  valueForFace: (faceIndex: number, total: number) => number = (i) => i + 1,
): DieDescriptor {
  // Derive normals once from a throwaway geometry instance.
  const probe = makeGeometry();
  const normals = deriveFaceNormals(probe);
  probe.dispose();

  const faceValue = normals.map((_, i) => valueForFace(i, normals.length));

  return {
    sides,
    geometry: makeGeometry,
    faceValue,
    renderCount: 1,
    faceNormal(faceIndex: number): Vector3 {
      return normals[faceIndex].clone();
    },
    orientationFor(value: number): Quaternion {
      const faceIndex = faceValue.indexOf(value);
      // Unknown value: identity (no NaN, die simply keeps its tumble pose).
      if (faceIndex === -1) return new Quaternion();
      return rotationBetween(normals[faceIndex].clone(), CAMERA_UP);
    },
  };
}

// ─── Supported die registry ───
// `radius` arguments are arbitrary unit scales; orientation correctness is
// scale-invariant, and the renderer normalizes display size separately.

const DIE_BUILDERS: Record<number, () => DieDescriptor> = {
  4: () => buildDescriptor(4, () => new TetrahedronGeometry(1)),
  6: () => buildDescriptor(6, () => new BoxGeometry(1, 1, 1)),
  8: () => buildDescriptor(8, () => new OctahedronGeometry(1)),
  12: () => buildDescriptor(12, () => new DodecahedronGeometry(1)),
  20: () => buildDescriptor(20, () => new IcosahedronGeometry(1)),
};

const registry = new Map<number, DieDescriptor>();

/**
 * Fallback descriptor for unsupported side counts (d10, d100, exotic dice).
 * Backed by a sphere so it never crashes the renderer; it spins and the
 * integration's text overlay shows the actual value. `faceValue` covers
 * 1..min(sides, 6) and `orientationFor` returns identity (the sphere has no
 * canonical "up" face — the value is communicated by the overlay, not a facet).
 */
function fallbackDescriptor(sides: number): DieDescriptor {
  const count = Math.max(1, Math.min(sides, 6));
  const faceValue = Array.from({ length: count }, (_, i) => i + 1);
  // A single representative up-normal so faceNormal is well-defined and
  // non-degenerate for any index.
  const upNormal = new Vector3(0, 1, 0);

  return {
    sides,
    geometry: () => new SphereGeometry(1, 24, 16),
    faceValue,
    renderCount: 1,
    faceNormal(): Vector3 {
      return upNormal.clone();
    },
    orientationFor(): Quaternion {
      return new Quaternion();
    },
  };
}

/**
 * Resolve the descriptor for a die with `sides` faces. Supported v1 dice
 * (4, 6, 8, 12, 20) return a fully-derived descriptor; any other side count
 * returns a non-throwing fallback so the integration never crashes on an
 * exotic die.
 */
export function dieDescriptor(sides: number): DieDescriptor {
  const cached = registry.get(sides);
  if (cached) return cached;

  const builder = DIE_BUILDERS[sides];
  const descriptor = builder ? builder() : fallbackDescriptor(sides);
  registry.set(sides, descriptor);
  return descriptor;
}

/** Side counts with a fully-derived (non-fallback) descriptor. */
export const SUPPORTED_DIE_SIDES: readonly number[] = [4, 6, 8, 12, 20];
