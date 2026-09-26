import * as THREE from "three";

const MAX_READABLE_TEXTURE_ANISOTROPY = 16;

/**
 * Configures a color texture for stable minification on the tilted tabletop
 * camera. Card frames and small type contain much higher-frequency detail than
 * the artwork, so sampling the base level directly produces shimmer and
 * stair-stepped glyphs as permanents move across the battlefield.
 */
export function configureTabletopReadableTexture(
  texture: THREE.Texture,
  maxAnisotropy: number,
): void {
  const anisotropy = Math.min(
    Math.max(maxAnisotropy, 1),
    MAX_READABLE_TEXTURE_ANISOTROPY,
  );
  const requiresUpload =
    texture.colorSpace !== THREE.SRGBColorSpace
    || texture.minFilter !== THREE.LinearMipmapLinearFilter
    || texture.magFilter !== THREE.LinearFilter
    || !texture.generateMipmaps
    || texture.anisotropy !== anisotropy;

  texture.colorSpace = THREE.SRGBColorSpace;
  texture.minFilter = THREE.LinearMipmapLinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = true;
  texture.anisotropy = anisotropy;
  if (requiresUpload) texture.needsUpdate = true;
}
