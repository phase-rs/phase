import { describe, expect, it } from "vitest";
import { Quaternion, Vector3 } from "three";

import { COIN_REST_ANGLE, createCoinGeometry, FLIP_AXIS, type CoinFace } from "../coinGeometry.ts";

const FACES: CoinFace[] = ["heads", "tails"];
// Coin3D's camera sits on +Z looking at the origin with +Y up, so screen-right
// is +X and screen-up is +Y.
const TOWARD_CAMERA = new Vector3(0, 0, 1);

/**
 * Where a resting coin's camera-facing label points on screen. The cap's rim
 * vertex with the largest U lies in the direction the label's text runs, and
 * the one with the largest V in the direction its letters stand (the face
 * textures keep three.js's default `flipY`, so the canvas top is V = 1).
 */
function restingLabelAxes(face: CoinFace) {
  const geometry = createCoinGeometry();
  const rest = new Quaternion().setFromAxisAngle(FLIP_AXIS, COIN_REST_ANGLE[face]);
  const position = geometry.getAttribute("position");
  const normal = geometry.getAttribute("normal");
  const uv = geometry.getAttribute("uv");

  const textRuns = new Vector3();
  const lettersStand = new Vector3();
  let largestU = -Infinity;
  let largestV = -Infinity;
  let capVertices = 0;
  for (let index = 0; index < position.count; index++) {
    const facing = new Vector3().fromBufferAttribute(normal, index).applyQuaternion(rest);
    if (facing.dot(TOWARD_CAMERA) < 0.999) continue;
    capVertices++;
    const point = new Vector3().fromBufferAttribute(position, index).applyQuaternion(rest);
    if (uv.getX(index) > largestU) {
      largestU = uv.getX(index);
      textRuns.copy(point);
    }
    if (uv.getY(index) > largestV) {
      largestV = uv.getY(index);
      lettersStand.copy(point);
    }
  }
  geometry.dispose();
  return { textRuns, lettersStand, capVertices };
}

describe("coinGeometry: each face rests toward the camera with an upright label", () => {
  for (const face of FACES) {
    it(`reads ${face} left to right, upright`, () => {
      const { textRuns, lettersStand, capVertices } = restingLabelAxes(face);

      // Reach guard: the face's cap really does rest toward the camera.
      expect(capVertices).toBeGreaterThan(0);
      // Text runs toward screen-right, not down or up the coin (a quarter turn).
      expect(textRuns.x).toBeCloseTo(1);
      expect(textRuns.y).toBeCloseTo(0);
      // Letters stand toward screen-up, so the label is not mirrored or flipped.
      expect(lettersStand.x).toBeCloseTo(0);
      expect(lettersStand.y).toBeCloseTo(1);
    });
  }
});
