import { describe, expect, it } from "vitest";
import { Quaternion, Vector3 } from "three";

import { COIN_REST_ANGLE, createCoinGeometry, FLIP_AXIS, type CoinFace } from "../coinGeometry.ts";

// Each face's material group, as Coin3D's material array assigns them:
// [side, heads, tails] over CylinderGeometry's [side, top cap, bottom cap].
const FACE_GROUP = { heads: 1, tails: 2 } as const satisfies Record<CoinFace, number>;
const FACES: CoinFace[] = ["heads", "tails"];
// Coin3D's camera sits on +Z looking at the origin with +Y up, so screen-right
// is +X and screen-up is +Y.
const TOWARD_CAMERA = new Vector3(0, 0, 1);

/**
 * How a resting coin shows `face`: whether every vertex of that face's cap
 * faces the camera, and where its label points on screen. The cap's rim vertex
 * with the largest U lies in the direction the label's text runs, and the one
 * with the largest V in the direction its letters stand (the face textures
 * keep three.js's default `flipY`, so the canvas top is V = 1).
 */
function restingFace(face: CoinFace) {
  const geometry = createCoinGeometry();
  const rest = new Quaternion().setFromAxisAngle(FLIP_AXIS, COIN_REST_ANGLE[face]);
  const position = geometry.getAttribute("position");
  const normal = geometry.getAttribute("normal");
  const uv = geometry.getAttribute("uv");
  const index = geometry.getIndex();
  const group = geometry.groups.find((candidate) => candidate.materialIndex === FACE_GROUP[face]);
  if (!index || !group) throw new Error(`coin geometry has no indexed group for ${face}`);

  const textRuns = new Vector3();
  const lettersStand = new Vector3();
  let largestU = -Infinity;
  let largestV = -Infinity;
  let facesCamera = true;
  for (let slot = group.start; slot < group.start + group.count; slot++) {
    const vertex = index.getX(slot);
    const facing = new Vector3().fromBufferAttribute(normal, vertex).applyQuaternion(rest);
    if (facing.dot(TOWARD_CAMERA) < 0.999) facesCamera = false;
    const point = new Vector3().fromBufferAttribute(position, vertex).applyQuaternion(rest);
    if (uv.getX(vertex) > largestU) {
      largestU = uv.getX(vertex);
      textRuns.copy(point);
    }
    if (uv.getY(vertex) > largestV) {
      largestV = uv.getY(vertex);
      lettersStand.copy(point);
    }
  }
  geometry.dispose();
  return { facesCamera, textRuns, lettersStand, vertices: group.count };
}

describe("coinGeometry: each face rests toward the camera with an upright label", () => {
  for (const face of FACES) {
    it(`rests ${face} toward the camera, reading left to right, upright`, () => {
      const { facesCamera, textRuns, lettersStand, vertices } = restingFace(face);

      // The requested face's own cap, not the other one, faces the camera.
      expect(vertices).toBeGreaterThan(0);
      expect(facesCamera).toBe(true);
      // Text runs toward screen-right, not down or up the coin (a quarter turn).
      expect(textRuns.x).toBeCloseTo(1);
      expect(textRuns.y).toBeCloseTo(0);
      // Letters stand toward screen-up, so the label is not mirrored or flipped.
      expect(lettersStand.x).toBeCloseTo(0);
      expect(lettersStand.y).toBeCloseTo(1);
    });
  }
});
