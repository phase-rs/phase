import { useEffect, useState } from "react";
import { useThree } from "@react-three/fiber";
import * as THREE from "three";

import { configureTabletopReadableTexture } from "./tabletopTexture.ts";
import { PHYSICAL_CARD_CORNER_RADIUS_RATIO } from "./tabletopCardFrame.ts";

/**
 * Loads a CORS-clean card image into a rounded CanvasTexture. Clipping at the
 * texture boundary removes the opaque white JPEG corners present on some card
 * backs and keeps every 3D card surface on the same silhouette.
 */
export function useTabletopImageTexture(source: string | null): THREE.Texture | null {
  const maxAnisotropy = useThree(({ gl }) =>
    gl.capabilities.getMaxAnisotropy()
  );
  const [loaded, setLoaded] = useState<{
    source: string;
    texture: THREE.Texture;
  } | null>(null);

  useEffect(() => {
    if (!source) return;
    let cancelled = false;
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onload = () => {
      if (cancelled) return;
      const canvas = document.createElement("canvas");
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const context = canvas.getContext("2d");
      if (!context) return;
      context.beginPath();
      context.roundRect(
        0,
        0,
        canvas.width,
        canvas.height,
        Math.round(canvas.width * PHYSICAL_CARD_CORNER_RADIUS_RATIO),
      );
      context.clip();
      context.drawImage(image, 0, 0);
      const texture = new THREE.CanvasTexture(canvas);
      configureTabletopReadableTexture(texture, maxAnisotropy);
      setLoaded({ source, texture });
    };
    image.src = source;
    return () => {
      cancelled = true;
    };
  }, [maxAnisotropy, source]);

  useEffect(
    () => () => {
      loaded?.texture.dispose();
    },
    [loaded],
  );

  return loaded?.source === source ? loaded.texture : null;
}
