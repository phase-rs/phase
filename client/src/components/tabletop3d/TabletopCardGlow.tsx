import { useMemo } from "react";
import * as THREE from "three";

interface TabletopCardGlowProps {
  width: number;
  height: number;
  padding: number;
  color: string;
  opacity: number;
  y?: number;
}

/**
 * A low-energy, engine-authored underlight. The analytic shader has one broad
 * falloff and no animated noise, particles, or hard outline.
 */
export function TabletopCardGlow({
  width,
  height,
  padding,
  color,
  opacity,
  y = -0.006,
}: TabletopCardGlowProps) {
  const uniforms = useMemo(
    () => ({
      uColor: { value: new THREE.Color(color) },
      uInnerHalfSize: {
        value: new THREE.Vector2(
          width / (width + padding * 1.7),
          height / (height + padding * 1.7),
        ),
      },
      uOpacity: { value: opacity },
    }),
    [color, height, opacity, padding, width],
  );

  return (
    <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, y, 0]}>
      <planeGeometry
        args={[width + padding * 1.7, height + padding * 1.7]}
      />
      <shaderMaterial
        uniforms={uniforms}
        vertexShader={UNDERGLOW_VERTEX_SHADER}
        fragmentShader={UNDERGLOW_FRAGMENT_SHADER}
        transparent
        blending={THREE.AdditiveBlending}
        depthWrite={false}
        toneMapped={false}
      />
    </mesh>
  );
}

const UNDERGLOW_VERTEX_SHADER = `
  varying vec2 vUv;

  void main() {
    vUv = uv;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  }
`;

const UNDERGLOW_FRAGMENT_SHADER = `
  uniform vec3 uColor;
  uniform vec2 uInnerHalfSize;
  uniform float uOpacity;
  varying vec2 vUv;

  float roundedBoxDistance(vec2 point, vec2 halfSize, float radius) {
    vec2 offset = abs(point) - halfSize + radius;
    return min(max(offset.x, offset.y), 0.0)
      + length(max(offset, 0.0))
      - radius;
  }

  void main() {
    vec2 point = (vUv - 0.5) * 2.0;
    float distanceFromCard = max(
      roundedBoxDistance(point, uInnerHalfSize, 0.055),
      0.0
    );
    float availableFalloff = max(
      0.08,
      1.0 - min(uInnerHalfSize.x, uInnerHalfSize.y)
    );
    float alpha = (
      1.0 - smoothstep(0.0, availableFalloff, distanceFromCard)
    ) * uOpacity;

    if (alpha < 0.004) discard;
    gl_FragColor = vec4(uColor, alpha);
  }
`;
