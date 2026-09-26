import { useEffect, useMemo } from "react";
import * as THREE from "three";

import {
  TABLETOP_MATERIAL_PLANE_DEPTH,
  TABLETOP_MATERIAL_PLANE_WIDTH,
  createTabletopSurfaceMaterial,
} from "./tabletopMaterialPlane.ts";

/**
 * The battlefield is a cropped piece of a larger environment, not a table.
 * This plane continues well beyond every supported camera crop so no border,
 * slab edge, or furniture silhouette competes with the cards.
 */
export function TabletopMaterialPlane() {
  const geometry = useMemo(
    () =>
      new THREE.PlaneGeometry(
        TABLETOP_MATERIAL_PLANE_WIDTH,
        TABLETOP_MATERIAL_PLANE_DEPTH,
      ),
    [],
  );
  const surfaceMaterial = useMemo(createTabletopSurfaceMaterial, []);

  useEffect(
    () => () => {
      geometry.dispose();
      surfaceMaterial.dispose();
    },
    [geometry, surfaceMaterial],
  );

  return (
    <mesh
      geometry={geometry}
      material={surfaceMaterial}
      rotation={[-Math.PI / 2, 0, 0]}
      position={[0, 0, 0]}
      receiveShadow
    />
  );
}
