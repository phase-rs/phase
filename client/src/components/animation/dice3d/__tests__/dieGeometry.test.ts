import { describe, expect, it } from "vitest";
import { Quaternion, Vector3 } from "three";

import {
  CAMERA_UP,
  dieDescriptor,
  SUPPORTED_DIE_SIDES,
} from "../dieGeometry.ts";

const EPS = 1e-4;

function quaternionHasNaN(q: Quaternion): boolean {
  return (
    Number.isNaN(q.x) ||
    Number.isNaN(q.y) ||
    Number.isNaN(q.z) ||
    Number.isNaN(q.w)
  );
}

describe("dieGeometry — orientationFor brings the requested face to CAMERA_UP", () => {
  for (const sides of SUPPORTED_DIE_SIDES) {
    describe(`d${sides}`, () => {
      const descriptor = dieDescriptor(sides);

      it(`has exactly ${sides} faces numbered 1..${sides}`, () => {
        expect(descriptor.faceValue).toHaveLength(sides);
        const sorted = [...descriptor.faceValue].sort((a, b) => a - b);
        expect(sorted).toEqual(
          Array.from({ length: sides }, (_, i) => i + 1),
        );
      });

      for (let value = 1; value <= sides; value++) {
        it(`orientationFor(${value}) points face ${value} at CAMERA_UP`, () => {
          const faceIndex = descriptor.faceValue.indexOf(value);
          expect(faceIndex).toBeGreaterThanOrEqual(0);

          const normal = descriptor.faceNormal(faceIndex);
          const q = descriptor.orientationFor(value);
          expect(quaternionHasNaN(q)).toBe(false);

          const rotated = normal.clone().applyQuaternion(q);
          // The rotated face normal must coincide with CAMERA_UP.
          expect(rotated.dot(CAMERA_UP)).toBeGreaterThan(1 - EPS);
        });
      }
    });
  }
});

describe("dieGeometry — antipodal degeneracy guard", () => {
  it("never returns a NaN quaternion even when a face normal opposes CAMERA_UP", () => {
    // Walk every supported die and every face; the down-pointing face (normal
    // ≈ -CAMERA_UP) is the antipodal case setFromUnitVectors cannot handle.
    for (const sides of SUPPORTED_DIE_SIDES) {
      const descriptor = dieDescriptor(sides);
      for (const value of descriptor.faceValue) {
        const q = descriptor.orientationFor(value);
        expect(quaternionHasNaN(q)).toBe(false);
      }
    }
  });

  it("d6 explicitly handles a face whose normal is exactly -CAMERA_UP", () => {
    const descriptor = dieDescriptor(6);
    // The box has an axis-aligned face at -Y. Find the value on it.
    const downIndex = descriptor.faceValue.findIndex(
      (_, i) =>
        descriptor.faceNormal(i).dot(CAMERA_UP) < -0.999,
    );
    expect(downIndex).toBeGreaterThanOrEqual(0);

    const value = descriptor.faceValue[downIndex];
    const normal = descriptor.faceNormal(downIndex);
    const q = descriptor.orientationFor(value);
    expect(quaternionHasNaN(q)).toBe(false);

    const rotated = normal.clone().applyQuaternion(q);
    expect(rotated.dot(CAMERA_UP)).toBeGreaterThan(1 - EPS);
  });
});

describe("dieGeometry — unsupported sides fall back gracefully", () => {
  it("dieDescriptor(10) returns a non-throwing fallback", () => {
    const descriptor = dieDescriptor(10);
    expect(descriptor.sides).toBe(10);
    expect(descriptor.faceValue.length).toBeGreaterThan(0);
    expect(() => descriptor.geometry().dispose()).not.toThrow();
    const q = descriptor.orientationFor(7);
    expect(quaternionHasNaN(q)).toBe(false);
    // Identity orientation for the fallback.
    expect(q.equals(new Quaternion())).toBe(true);
  });

  it("dieDescriptor(100) returns a non-throwing fallback", () => {
    const descriptor = dieDescriptor(100);
    expect(descriptor.sides).toBe(100);
    expect(descriptor.faceValue.length).toBeGreaterThan(0);
    expect(() => descriptor.geometry().dispose()).not.toThrow();
    const normal = descriptor.faceNormal(0);
    expect(normal).toBeInstanceOf(Vector3);
    expect(quaternionHasNaN(descriptor.orientationFor(1))).toBe(false);
  });
});
