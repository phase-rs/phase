import { describe, expect, it } from "vitest";
import { Quaternion, Vector3 } from "three";

import {
  coverCrop,
  createTileGeometry,
  FLIGHT_MS,
  FLIGHT_TOTAL_MS,
  LANDING_REST_ANGLE,
  RELEASE_HEIGHT,
  sampleFlight,
  TILE_GROUP,
  tumbleRotation,
  TUMBLE_AXIS,
  type DexterityFlipResult,
  type DexterityLanding,
} from "../dexterityFlip.ts";

const UP = new Vector3(0, 1, 0);
// The camera looks down at the ground from +Z, so screen-up on the ground is -Z
// and screen-right is +X.
const SCREEN_UP_ON_GROUND = new Vector3(0, 0, -1);
const CONTACT_MS = FLIGHT_MS.hold + FLIGHT_MS.fall;
const REST_MS = CONTACT_MS + FLIGHT_MS.bounce;

const RESULTS: DexterityFlipResult[] = [
  { landing: "faceUp", fullTurns: 0 },
  { landing: "faceUp", fullTurns: 1 },
  { landing: "faceUp", fullTurns: 2 },
  { landing: "faceDown", fullTurns: 0 },
  { landing: "faceDown", fullTurns: 1 },
  { landing: "faceDown", fullTurns: 3 },
];

/** Whether the angle leaves `landing`'s side pointing up. */
function sideUp(angle: number, landing: DexterityLanding): boolean {
  const facing = new Vector3(0, 0, landing === "faceUp" ? 1 : -1).applyQuaternion(
    new Quaternion().setFromAxisAngle(TUMBLE_AXIS, angle),
  );
  return facing.dot(UP) > 0.999;
}

/** The tile's tumble, in half turns, between two times (sign dropped). */
function halfTurnsBetween(result: DexterityFlipResult, fromMs: number, toMs: number): number {
  return Math.abs(sampleFlight(toMs, result).angle - sampleFlight(fromMs, result).angle) / Math.PI;
}

/**
 * One face of the tile at `angle`: whether every vertex of its group points up,
 * and where its image's top and right edges point on the ground. The vertex with
 * the largest V lies toward the image's top, the one with the largest U toward
 * its right (textures keep three.js's default `flipY`, so the image top is V = 1).
 */
function restingFace(group: number, angle: number) {
  const geometry = createTileGeometry(1.36);
  const rest = new Quaternion().setFromAxisAngle(TUMBLE_AXIS, angle);
  const position = geometry.getAttribute("position");
  const normal = geometry.getAttribute("normal");
  const uv = geometry.getAttribute("uv");
  const range = geometry.groups.find((candidate) => candidate.materialIndex === group);
  if (!range) throw new Error(`tile geometry has no group ${group}`);

  const imageTop = new Vector3();
  const imageRight = new Vector3();
  let largestU = -Infinity;
  let largestV = -Infinity;
  let facesUp = true;
  for (let vertex = range.start; vertex < range.start + range.count; vertex++) {
    const facing = new Vector3().fromBufferAttribute(normal, vertex).applyQuaternion(rest);
    if (facing.dot(UP) < 0.999) facesUp = false;
    const point = new Vector3().fromBufferAttribute(position, vertex).applyQuaternion(rest);
    if (uv.getX(vertex) > largestU) {
      largestU = uv.getX(vertex);
      imageRight.copy(point);
    }
    if (uv.getY(vertex) > largestV) {
      largestV = uv.getY(vertex);
      imageTop.copy(point);
    }
  }
  geometry.dispose();
  return { facesUp, imageTop, imageRight, vertices: range.count };
}

describe("tumbleRotation: the turn count the engine reports, plus the last half turn for face down", () => {
  it("adds a full turn per complete turn and a half turn to land face down", () => {
    expect(tumbleRotation({ landing: "faceUp", fullTurns: 0 })).toBe(-0);
    expect(tumbleRotation({ landing: "faceUp", fullTurns: 2 })).toBeCloseTo(-4 * Math.PI);
    expect(tumbleRotation({ landing: "faceDown", fullTurns: 1 })).toBeCloseTo(-3 * Math.PI);
  });

  it("rejects a turn count that is negative or not whole", () => {
    expect(() => tumbleRotation({ landing: "faceUp", fullTurns: -1 })).toThrow(RangeError);
    expect(() => tumbleRotation({ landing: "faceUp", fullTurns: 1.5 })).toThrow(RangeError);
  });
});

describe("sampleFlight: the tile lands exactly as reported", () => {
  for (const result of RESULTS) {
    const label = `${result.landing}, ${result.fullTurns} full turn(s)`;

    it(`${label}: starts face up, flat, at the release height`, () => {
      const first = sampleFlight(0, result);
      expect(first.height).toBe(RELEASE_HEIGHT);
      expect(sideUp(first.angle, "faceUp")).toBe(true);
    });

    it(`${label}: touches down with the reported side up`, () => {
      const contact = sampleFlight(CONTACT_MS, result);
      expect(contact.height).toBeCloseTo(0);
      expect(sideUp(contact.angle, result.landing)).toBe(true);
      expect(sideUp(sampleFlight(REST_MS, result).angle, result.landing)).toBe(true);
    });

    it(`${label}: turns exactly the reported number of half turns in the air`, () => {
      const expected = result.fullTurns * 2 + (result.landing === "faceDown" ? 1 : 0);
      expect(halfTurnsBetween(result, FLIGHT_MS.hold, CONTACT_MS)).toBeCloseTo(expected);
    });

    it(`${label}: falls without ever rising, and only bounces after contact`, () => {
      let previous = Infinity;
      for (let ms = FLIGHT_MS.hold; ms < CONTACT_MS; ms += 10) {
        const { height } = sampleFlight(ms, result);
        expect(height).toBeLessThanOrEqual(previous);
        previous = height;
      }
      for (let ms = CONTACT_MS; ms <= REST_MS; ms += 10) {
        expect(sampleFlight(ms, result).height).toBeGreaterThanOrEqual(0);
      }
    });

    it(`${label}: the bounce wobble never shows the other side`, () => {
      for (let ms = CONTACT_MS; ms <= FLIGHT_TOTAL_MS; ms += 5) {
        const facing = new Vector3(0, 0, result.landing === "faceUp" ? 1 : -1).applyQuaternion(
          new Quaternion().setFromAxisAngle(TUMBLE_AXIS, sampleFlight(ms, result).angle),
        );
        expect(facing.dot(UP)).toBeGreaterThan(0.9);
      }
    });

    it(`${label}: settles once the bounce ends, then reveals`, () => {
      expect(sampleFlight(REST_MS - 1, result).settled).toBe(false);
      expect(sampleFlight(REST_MS, result).settled).toBe(true);
      expect(sampleFlight(REST_MS, result).reveal).toBe(0);
      expect(sampleFlight(FLIGHT_TOTAL_MS, result).reveal).toBe(1);
    });
  }

  it("stretches every phase by the speed multiplier", () => {
    const result: DexterityFlipResult = { landing: "faceUp", fullTurns: 1 };
    expect(sampleFlight(REST_MS * 2 - 1, result, 2).settled).toBe(false);
    expect(sampleFlight(REST_MS * 2, result, 2).settled).toBe(true);
  });
});

describe("createTileGeometry: each face rests up with its image upright", () => {
  const cases = [
    { name: "front", group: TILE_GROUP.front, angle: LANDING_REST_ANGLE.faceUp },
    { name: "back", group: TILE_GROUP.back, angle: LANDING_REST_ANGLE.faceDown },
  ] as const;

  for (const { name, group, angle } of cases) {
    it(`rests the ${name} up, reading left to right, top away from the camera`, () => {
      const { facesUp, imageTop, imageRight, vertices } = restingFace(group, angle);
      expect(vertices).toBeGreaterThan(0);
      expect(facesUp).toBe(true);
      // The image's right edge points screen-right (+X), not mirrored.
      expect(imageRight.x).toBeGreaterThan(0.5);
      // The image's top points screen-up on the ground (-Z), not upside down.
      expect(imageTop.dot(SCREEN_UP_ON_GROUND)).toBeGreaterThan(0.3);
    });
  }

  it("puts every triangle in exactly one of side, front and back", () => {
    const geometry = createTileGeometry(1.36);
    const total = geometry.getAttribute("position").count;
    const grouped = geometry.groups.reduce((sum, group) => sum + group.count, 0);
    expect(geometry.groups.map((group) => group.materialIndex)).toEqual([
      TILE_GROUP.side,
      TILE_GROUP.front,
      TILE_GROUP.back,
    ]);
    expect(grouped).toBe(total);
    geometry.dispose();
  });
});

describe("coverCrop: fills the tile without stretching", () => {
  it("crops the sides of a wider image, centred", () => {
    const { repeat, offset } = coverCrop(2, 1);
    expect(repeat).toEqual([0.5, 1]);
    expect(offset).toEqual([0.25, 0]);
  });

  it("crops the top and bottom of a taller image, centred", () => {
    const { repeat, offset } = coverCrop(1, 2);
    expect(repeat).toEqual([1, 0.5]);
    expect(offset).toEqual([0, 0.25]);
  });

  it("leaves a matching image whole, and ignores a degenerate size", () => {
    expect(coverCrop(1.36, 1.36)).toEqual({ repeat: [1, 1], offset: [0, 0] });
    expect(coverCrop(0, 1.36)).toEqual({ repeat: [1, 1], offset: [0, 0] });
  });
});
