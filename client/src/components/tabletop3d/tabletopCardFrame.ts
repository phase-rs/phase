import * as THREE from "three";

/** Physical Magic cards use an approximately 2.5 mm radius on a 63 mm edge. */
export const PHYSICAL_CARD_CORNER_RADIUS_RATIO = 2.5 / 63;

export function physicalCardCornerRadius(width: number): number {
  return width * PHYSICAL_CARD_CORNER_RADIUS_RATIO;
}

export function makeRoundedRectangleShape(
  width: number,
  height: number,
  radius: number,
): THREE.Shape {
  const halfWidth = width / 2;
  const halfHeight = height / 2;
  const cornerRadius = Math.min(
    Math.max(radius, 0),
    halfWidth,
    halfHeight,
  );
  const shape = new THREE.Shape();
  shape.moveTo(-halfWidth + cornerRadius, -halfHeight);
  shape.lineTo(halfWidth - cornerRadius, -halfHeight);
  shape.absarc(
    halfWidth - cornerRadius,
    -halfHeight + cornerRadius,
    cornerRadius,
    -Math.PI / 2,
    0,
    false,
  );
  shape.lineTo(halfWidth, halfHeight - cornerRadius);
  shape.absarc(
    halfWidth - cornerRadius,
    halfHeight - cornerRadius,
    cornerRadius,
    0,
    Math.PI / 2,
    false,
  );
  shape.lineTo(-halfWidth + cornerRadius, halfHeight);
  shape.absarc(
    -halfWidth + cornerRadius,
    halfHeight - cornerRadius,
    cornerRadius,
    Math.PI / 2,
    Math.PI,
    false,
  );
  shape.lineTo(-halfWidth, -halfHeight + cornerRadius);
  shape.absarc(
    -halfWidth + cornerRadius,
    -halfHeight + cornerRadius,
    cornerRadius,
    Math.PI,
    Math.PI * 1.5,
    false,
  );
  shape.closePath();
  return shape;
}

export function makeRoundedCardFaceGeometry(
  width: number,
  height: number,
  radius = physicalCardCornerRadius(width),
): THREE.ShapeGeometry {
  const geometry = new THREE.ShapeGeometry(
    makeRoundedRectangleShape(width, height, radius),
    8,
  );
  const halfWidth = width / 2;
  const halfHeight = height / 2;
  const positions = geometry.getAttribute("position");
  const uvs = new Float32Array(positions.count * 2);
  for (let index = 0; index < positions.count; index += 1) {
    uvs[index * 2] = (positions.getX(index) + halfWidth) / width;
    uvs[index * 2 + 1] = (positions.getY(index) + halfHeight) / height;
  }
  geometry.setAttribute("uv", new THREE.BufferAttribute(uvs, 2));
  return geometry;
}

interface RoundedCardBodyOptions {
  bevelSegments?: number;
  bevelSize?: number;
  bevelThickness?: number;
  radius?: number;
}

/**
 * Builds a rounded, beveled card body whose maximum overhead footprint is the
 * requested width and height. ExtrudeGeometry expands a bevel outwards, so the
 * source outline is inset by exactly that amount before extrusion.
 */
export function makeRoundedCardBodyGeometry(
  width: number,
  height: number,
  depth: number,
  options: RoundedCardBodyOptions = {},
): THREE.ExtrudeGeometry {
  const bevelSize = Math.min(
    options.bevelSize ?? 0.012,
    width / 4,
    height / 4,
  );
  const radius = options.radius ?? physicalCardCornerRadius(width);
  const bevelThickness = Math.min(
    options.bevelThickness ?? 0.009,
    depth * 0.49,
  );
  const geometry = new THREE.ExtrudeGeometry(
    makeRoundedRectangleShape(
      width - bevelSize * 2,
      height - bevelSize * 2,
      Math.max(radius - bevelSize, 0),
    ),
    {
      // ExtrudeGeometry adds bevelThickness beyond both nominal caps. Keep the
      // finished body at the requested physical thickness.
      depth: Math.max(depth - bevelThickness * 2, 0.0001),
      bevelEnabled: true,
      bevelSegments: options.bevelSegments ?? 2,
      bevelSize,
      bevelThickness,
      steps: 1,
      curveSegments: 8,
    },
  );
  return alignGeometryFrontToPlane(geometry);
}

/**
 * Moves an extruded shell so its actual front-most vertex rests at z=0.
 * Three.js bevels extend beyond an extrusion's nominal depth, so translating
 * by the configured depth alone can leave the bevel in front of a face layer.
 */
export function alignGeometryFrontToPlane<T extends THREE.BufferGeometry>(
  geometry: T,
): T {
  geometry.computeBoundingBox();
  const front = geometry.boundingBox?.max.z;
  if (front == null || !Number.isFinite(front)) return geometry;
  geometry.translate(0, 0, -front);
  return geometry;
}
