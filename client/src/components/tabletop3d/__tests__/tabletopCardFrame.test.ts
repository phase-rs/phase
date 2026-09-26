import { describe, expect, it } from "vitest";
import * as THREE from "three";

import {
  alignGeometryFrontToPlane,
  makeRoundedCardBodyGeometry,
  makeRoundedCardFaceGeometry,
  physicalCardCornerRadius,
} from "../tabletopCardFrame.ts";

describe("alignGeometryFrontToPlane", () => {
  it("keeps a beveled extrusion entirely behind its textured face", () => {
    const shape = new THREE.Shape();
    shape.moveTo(-0.5, -0.7);
    shape.lineTo(0.5, -0.7);
    shape.lineTo(0.5, 0.7);
    shape.lineTo(-0.5, 0.7);
    shape.closePath();
    const geometry = new THREE.ExtrudeGeometry(shape, {
      depth: 0.038,
      bevelEnabled: true,
      bevelSize: 0.012,
      bevelThickness: 0.009,
    });

    alignGeometryFrontToPlane(geometry);
    geometry.computeBoundingBox();

    expect(geometry.boundingBox?.max.z).toBeCloseTo(0);
    expect(geometry.boundingBox?.min.z).toBeLessThan(0);
  });
});

describe("physical card geometry", () => {
  it("uses the 2.5 mm physical corner radius and normalized face UVs", () => {
    const width = 1.3;
    const height = 1.82;
    const geometry = makeRoundedCardFaceGeometry(width, height);
    geometry.computeBoundingBox();

    expect(physicalCardCornerRadius(width)).toBeCloseTo(1.3 * 2.5 / 63);
    expect(geometry.boundingBox?.min.x).toBeCloseTo(-width / 2);
    expect(geometry.boundingBox?.max.x).toBeCloseTo(width / 2);
    expect(geometry.boundingBox?.min.y).toBeCloseTo(-height / 2);
    expect(geometry.boundingBox?.max.y).toBeCloseTo(height / 2);

    const uvs = geometry.getAttribute("uv");
    const values = Array.from(uvs.array);
    expect(Math.min(...values)).toBeCloseTo(0);
    expect(Math.max(...values)).toBeCloseTo(1);
  });

  it("keeps the beveled shell inside the exact overhead face footprint", () => {
    const width = 1.3;
    const height = 1.82;
    const depth = 0.03;
    const geometry = makeRoundedCardBodyGeometry(width, height, depth);
    geometry.computeBoundingBox();

    expect(geometry.boundingBox?.min.x).toBeCloseTo(-width / 2);
    expect(geometry.boundingBox?.max.x).toBeCloseTo(width / 2);
    expect(geometry.boundingBox?.min.y).toBeCloseTo(-height / 2);
    expect(geometry.boundingBox?.max.y).toBeCloseTo(height / 2);
    expect(geometry.boundingBox?.max.z).toBeCloseTo(0);
    expect(geometry.boundingBox?.min.z).toBeCloseTo(-depth);
  });
});
