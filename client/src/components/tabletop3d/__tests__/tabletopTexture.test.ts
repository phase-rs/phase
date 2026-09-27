import { describe, expect, it } from "vitest";
import * as THREE from "three";

import { configureTabletopReadableTexture } from "../tabletopTexture.ts";

describe("configureTabletopReadableTexture", () => {
  it("uses trilinear mipmaps and bounded anisotropy for tilted card faces", () => {
    const texture = new THREE.Texture();

    configureTabletopReadableTexture(texture, 32);

    expect(texture.colorSpace).toBe(THREE.SRGBColorSpace);
    expect(texture.minFilter).toBe(THREE.LinearMipmapLinearFilter);
    expect(texture.magFilter).toBe(THREE.LinearFilter);
    expect(texture.generateMipmaps).toBe(true);
    expect(texture.anisotropy).toBe(16);
    expect(texture.version).toBeGreaterThan(0);
  });

  it("never requests less than one anisotropic sample", () => {
    const texture = new THREE.Texture();

    configureTabletopReadableTexture(texture, 0);

    expect(texture.anisotropy).toBe(1);
  });

  it("does not re-upload a cached texture whose sampling is already configured", () => {
    const texture = new THREE.Texture();
    configureTabletopReadableTexture(texture, 8);
    const configuredVersion = texture.version;

    configureTabletopReadableTexture(texture, 8);

    expect(texture.version).toBe(configuredVersion);
  });
});
