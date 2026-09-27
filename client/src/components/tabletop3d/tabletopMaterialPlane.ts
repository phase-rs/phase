import * as THREE from "three";

export const TABLETOP_MATERIAL_PLANE_WIDTH = 46;
export const TABLETOP_MATERIAL_PLANE_DEPTH = 42;
export function createTabletopSurfaceMaterial(): THREE.MeshStandardMaterial {
  return new THREE.MeshStandardMaterial({
    color: "#59656f",
    roughness: 0.96,
    metalness: 0,
    envMapIntensity: 0.18,
  });
}
