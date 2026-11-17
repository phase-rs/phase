// ─── 3D Coin Geometry ───
// PURE module: no WebGL context, no DOM. Like dieGeometry.ts it depends only on
// `three` math/geometry classes, so the coin's resting orientation is
// unit-testable under happy-dom without a GPU.

import { CylinderGeometry, Vector3 } from "three";

export type CoinFace = "heads" | "tails";

/** The flip axis: horizontal (X). The coin's flat faces point along ±Y in its
 *  own space (CylinderGeometry's axis is Y), so flipping about X swaps which
 *  face points toward the camera on +Z. */
export const FLIP_AXIS: Vector3 = new Vector3(1, 0, 0);

/** Rotation about FLIP_AXIS that rests each face toward the camera: +90° maps
 *  the top cap (+Y, heads) onto +Z, and -90° maps the bottom cap (-Y, tails). */
export const COIN_REST_ANGLE = {
  heads: Math.PI / 2,
  tails: -Math.PI / 2,
} as const satisfies Record<CoinFace, number>;

/**
 * Build the coin's disc (caller owns disposal). Material groups follow
 * CylinderGeometry: [side, top cap (heads), bottom cap (tails)].
 */
export function createCoinGeometry(): CylinderGeometry {
  const geometry = new CylinderGeometry(1, 1, 0.18, 48);
  // three.js lays a cap's texture along the cylinder's own axes: U runs along
  // +Z, and V along +X on the top cap and -X on the bottom one. Each rest
  // rotation turns +Z to the screen's vertical, so both labels would land a
  // quarter turn from upright, in opposite directions. A quarter turn about the
  // coin's own axis, which leaves the disc's shape unchanged, carries U onto
  // screen-right and V onto screen-up for both faces at once.
  geometry.rotateY(Math.PI / 2);
  return geometry;
}
