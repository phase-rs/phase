import { describe, expect, it } from "vitest";
import * as THREE from "three";

import {
  TABLETOP_MATERIAL_PLANE_DEPTH,
  TABLETOP_MATERIAL_PLANE_WIDTH,
  createTabletopSurfaceMaterial,
} from "../tabletopMaterialPlane.ts";

describe("tabletop material plane", () => {
  it("extends beyond the supported camera crop", () => {
    expect(TABLETOP_MATERIAL_PLANE_WIDTH).toBe(46);
    expect(TABLETOP_MATERIAL_PLANE_DEPTH).toBe(42);
  });

  it("uses a bundled-asset-free matte material", () => {
    const material = createTabletopSurfaceMaterial();

    expect(material).toBeInstanceOf(THREE.MeshStandardMaterial);
    expect(material.color.getHex()).toBe(0x59656f);
    expect(material.map).toBeNull();
    expect(material.normalMap).toBeNull();
    expect(material.roughnessMap).toBeNull();
    expect(material.roughness).toBe(0.96);
    expect(material.metalness).toBe(0);
  });
});
