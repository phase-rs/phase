// ─── DexterityFlip3D ───
// Imperative three.js dexterity flip, mounted into its own <canvas>. Same mount /
// rAF / dispose discipline as Coin3D and Dice3D.
//
// A permanent's art tile is held above a ground plane, let go, and tumbles end
// over end to land with the engine-reported side up after the engine-reported
// number of complete turns (`dexterityFlip.ts` owns the path). It casts a
// shadow that sharpens as it falls, kicks up a dust ring on contact, and an
// optional accent glow rises under it once it is at rest. `onSettle` fires once
// at rest. The component never decides what the landing means: the caller owns
// the verdict and passes the accent that goes with it.

import { useEffect, useRef } from "react";
import {
  AdditiveBlending,
  AmbientLight,
  CanvasTexture,
  Color,
  DirectionalLight,
  Mesh,
  MeshBasicMaterial,
  MeshStandardMaterial,
  PerspectiveCamera,
  PlaneGeometry,
  Scene,
  SRGBColorSpace,
  TextureLoader,
  WebGLRenderer,
  type Texture,
} from "three";
import {
  coverCrop,
  createTileGeometry,
  sampleFlight,
  TILE_DEPTH,
  TUMBLE_AXIS,
  type DexterityFlipResult,
} from "./dexterityFlip.ts";
import { useFrameFailure } from "./useFrameFailure.ts";

interface DexterityFlip3DProps {
  /** Which side lands up and how many complete turns it made, from the engine. */
  result: DexterityFlipResult;
  /** The permanent's art (an `art_crop` URL from `useCardImage`). While it is
   *  loading, or if it fails, the face shows a neutral slate panel. */
  artSrc?: string | null;
  /** Accent for the glow under the resting tile, as an `"r,g,b"` triple (the
   *  overlay's convention). Omit for no glow. */
  accentRgb?: string;
  /** Scales animation duration (default 1). */
  speedMultiplier?: number;
  /** Fired once when the tile comes to rest. */
  onSettle?: () => void;
  /** Canvas width in px (the component owns its own <canvas>). */
  width?: number;
  /** Canvas height in px. */
  height?: number;
}

const DEFAULT_WIDTH = 240;
const DEFAULT_HEIGHT = 300;
/** Scryfall `art_crop` images are close to 4:3 landscape; the tile keeps that
 *  shape so the art reads as it does on the battlefield. */
const TILE_ASPECT = 1.36;
const TILE_HALF_HEIGHT = 0.5;
const BACK_TEXTURE_PX = 256;
const SHADOW_TEXTURE_PX = 128;
const FACE_PLACEHOLDER = 0x334155; // slate-700
const SIDE_COLOR = 0x0f172a; // slate-900
const DUST_COLOR = 0xcbb89a;

/** The reverse of the tile: a dark slate panel with a fine lattice, so a
 *  face-down landing reads as "the art is underneath". */
function makeBackTexture(): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = BACK_TEXTURE_PX;
  canvas.height = Math.round(BACK_TEXTURE_PX / TILE_ASPECT);
  const ctx = canvas.getContext("2d");
  if (ctx) {
    const { width, height } = canvas;
    const grad = ctx.createLinearGradient(0, 0, width, height);
    grad.addColorStop(0, "#1e293b");
    grad.addColorStop(1, "#0b1120");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, width, height);

    ctx.strokeStyle = "rgba(148,163,184,0.16)";
    ctx.lineWidth = 2;
    for (let d = -height; d < width + height; d += 16) {
      ctx.beginPath();
      ctx.moveTo(d, 0);
      ctx.lineTo(d + height, height);
      ctx.moveTo(d + height, 0);
      ctx.lineTo(d, height);
      ctx.stroke();
    }

    ctx.strokeStyle = "rgba(251,191,36,0.55)";
    ctx.lineWidth = 3;
    ctx.beginPath();
    ctx.arc(width / 2, height / 2, height * 0.22, 0, Math.PI * 2);
    ctx.stroke();
  }
  const texture = new CanvasTexture(canvas);
  texture.colorSpace = SRGBColorSpace;
  return texture;
}

/** A soft ring for the dust kicked up on contact: clear inside and out. */
function makeDustTexture(): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = SHADOW_TEXTURE_PX;
  canvas.height = SHADOW_TEXTURE_PX;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    const half = SHADOW_TEXTURE_PX / 2;
    const grad = ctx.createRadialGradient(half, half, 0, half, half, half);
    grad.addColorStop(0, "rgba(255,255,255,0)");
    grad.addColorStop(0.55, "rgba(255,255,255,0.05)");
    grad.addColorStop(0.78, "rgba(255,255,255,0.7)");
    grad.addColorStop(1, "rgba(255,255,255,0)");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, SHADOW_TEXTURE_PX, SHADOW_TEXTURE_PX);
  }
  return new CanvasTexture(canvas);
}

/** A soft round blot for the ground shadow and the accent glow. */
function makeBlobTexture(): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = SHADOW_TEXTURE_PX;
  canvas.height = SHADOW_TEXTURE_PX;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    const half = SHADOW_TEXTURE_PX / 2;
    const grad = ctx.createRadialGradient(half, half, 0, half, half, half);
    grad.addColorStop(0, "rgba(255,255,255,1)");
    grad.addColorStop(0.55, "rgba(255,255,255,0.55)");
    grad.addColorStop(1, "rgba(255,255,255,0)");
    ctx.fillStyle = grad;
    ctx.fillRect(0, 0, SHADOW_TEXTURE_PX, SHADOW_TEXTURE_PX);
  }
  return new CanvasTexture(canvas);
}

function parseRgb(triple: string): Color | null {
  const parts = triple.split(",").map((part) => Number(part.trim()));
  if (parts.length !== 3 || parts.some((part) => !Number.isFinite(part))) return null;
  return new Color(parts[0] / 255, parts[1] / 255, parts[2] / 255);
}

/** Show `art` on the tile's front, or the slate placeholder when there is none. */
function applyArt(front: MeshStandardMaterial, art: Texture | null): void {
  front.map = art;
  front.color.set(art ? 0xffffff : FACE_PLACEHOLDER);
  front.needsUpdate = true;
}

/** Tint the resting glow with `accentRgb`, or hide it when there is none. */
function applyAccent(glow: Mesh, glowMaterial: MeshBasicMaterial, accentRgb: string | undefined): void {
  const accent = accentRgb ? parseRgb(accentRgb) : null;
  if (accent) glowMaterial.color.copy(accent);
  glow.visible = accent !== null;
}

export function DexterityFlip3D({
  result,
  artSrc,
  accentRgb,
  speedMultiplier = 1,
  onSettle,
  width = DEFAULT_WIDTH,
  height = DEFAULT_HEIGHT,
}: DexterityFlip3DProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const settledRef = useRef(false);
  const onSettleRef = useRef(onSettle);
  onSettleRef.current = onSettle;
  // Art and accent can arrive or change mid-flight (`useCardImage` resolves
  // asynchronously), so they live outside the flight effect: changing either
  // restyles the tile in place instead of dropping it again.
  const accentRef = useRef(accentRgb);
  accentRef.current = accentRgb;
  const artRef = useRef<Texture | null>(null);
  const frontRef = useRef<MeshStandardMaterial | null>(null);
  const glowRef = useRef<{ mesh: Mesh; material: MeshBasicMaterial } | null>(null);
  // Redraws a finished flight after an in-place restyle; null once disposed.
  const redrawRef = useRef<(() => void) | null>(null);
  const failFrame = useFrameFailure();
  const { landing, fullTurns } = result;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    settledRef.current = false;
    let disposed = false;
    let finished = false;
    let rafId = 0;
    const flight: DexterityFlipResult = { landing, fullTurns };

    // ─── Scene / camera / renderer ───
    // Looking down at the ground from the front at about 50°, so a tile lying
    // flat still reads, with headroom above for the release height.
    const scene = new Scene();
    const camera = new PerspectiveCamera(42, width / height, 0.1, 100);
    camera.position.set(0, 3.9, 3.8);
    camera.lookAt(0, 1.5, 0);

    const renderer = new WebGLRenderer({ alpha: true, antialias: true });
    renderer.setSize(width, height);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    container.appendChild(renderer.domElement);

    scene.add(new AmbientLight(0xffffff, 1.1));
    const keyLight = new DirectionalLight(0xffffff, 1.4);
    keyLight.position.set(1.5, 5, 3);
    scene.add(keyLight);

    // ─── Ground shadow, dust ring, accent glow ───
    const blobTexture = makeBlobTexture();
    const groundGeometry = new PlaneGeometry(1, 1);
    groundGeometry.rotateX(-Math.PI / 2);

    const shadowMaterial = new MeshBasicMaterial({
      map: blobTexture,
      color: 0x000000,
      transparent: true,
      depthWrite: false,
    });
    const shadow = new Mesh(groundGeometry, shadowMaterial);
    shadow.position.y = 0.002;
    scene.add(shadow);

    const glowMaterial = new MeshBasicMaterial({
      map: blobTexture,
      transparent: true,
      depthWrite: false,
      blending: AdditiveBlending,
      opacity: 0,
    });
    const glow = new Mesh(groundGeometry, glowMaterial);
    glow.position.y = 0.001;
    glow.scale.set(TILE_ASPECT * 2.1, 1, 2.1);
    applyAccent(glow, glowMaterial, accentRef.current);
    glowRef.current = { mesh: glow, material: glowMaterial };
    scene.add(glow);

    const dustTexture = makeDustTexture();
    const dustMaterial = new MeshBasicMaterial({
      map: dustTexture,
      color: DUST_COLOR,
      transparent: true,
      depthWrite: false,
      opacity: 0,
    });
    const dust = new Mesh(groundGeometry, dustMaterial);
    dust.position.y = 0.004;
    scene.add(dust);

    // ─── Tile: [side, front (art), back] ───
    const tileGeometry = createTileGeometry(TILE_ASPECT);
    const backTexture = makeBackTexture();
    const sideMaterial = new MeshStandardMaterial({ color: SIDE_COLOR, roughness: 0.6, metalness: 0.1 });
    const frontMaterial = new MeshStandardMaterial({ color: FACE_PLACEHOLDER, roughness: 0.55, metalness: 0 });
    const backMaterial = new MeshStandardMaterial({ map: backTexture, roughness: 0.5, metalness: 0.15 });
    const tile = new Mesh(tileGeometry, [sideMaterial, frontMaterial, backMaterial]);
    applyArt(frontMaterial, artRef.current);
    frontRef.current = frontMaterial;
    scene.add(tile);

    // ─── Animation ───
    const start = performance.now();

    const tick = (now: number) => {
      if (disposed) return;

      const sample = sampleFlight(now - start, flight, speedMultiplier);

      tile.quaternion.setFromAxisAngle(TUMBLE_AXIS, sample.angle);
      // `height` is the clearance under the tile, so lift its centre by the
      // turned tile's vertical half-extent: half its thickness when flat, half
      // its height when on edge. Its corners never dip into the ground.
      const verticalHalfExtent =
        Math.abs(Math.cos(sample.angle)) * TILE_HALF_HEIGHT + Math.abs(Math.sin(sample.angle)) * (TILE_DEPTH / 2);
      tile.position.y = sample.height + verticalHalfExtent;

      // Shadow: wide and faint high up, tight and dark on the ground.
      const nearGround = sample.fall;
      const spread = 1.8 - 0.75 * nearGround;
      shadow.scale.set(TILE_ASPECT * spread, 1, spread);
      shadowMaterial.opacity = 0.18 + 0.42 * nearGround;

      // Dust ring: expands and fades over the bounce.
      if (sample.impact > 0 && sample.impact < 1) {
        const ring = 1.2 + 1.3 * Math.sqrt(sample.impact);
        dust.scale.set(ring * TILE_ASPECT, 1, ring);
        dustMaterial.opacity = 0.7 * (1 - sample.impact);
      } else {
        dustMaterial.opacity = 0;
      }

      glowMaterial.opacity = 0.85 * sample.reveal;

      renderer.render(scene, camera);

      if (sample.settled && !settledRef.current) {
        settledRef.current = true;
        onSettleRef.current?.();
      }
      if (sample.reveal >= 1) {
        finished = true;
        return;
      }

      rafId = requestAnimationFrame(guardedTick);
    };

    // A frame that throws (e.g. a lost WebGL context) stops the loop and hands
    // the error to the overlay's boundary, which shows the static result.
    const guardedTick = (now: number) => {
      try {
        tick(now);
      } catch (error) {
        cancelAnimationFrame(rafId);
        failFrame(error);
      }
    };

    // Mid-flight the next frame picks a restyle up; once the loop has stopped,
    // draw one frame so the change reaches the canvas.
    redrawRef.current = () => {
      if (disposed || !finished) return;
      try {
        renderer.render(scene, camera);
      } catch (error) {
        failFrame(error);
      }
    };

    rafId = requestAnimationFrame(guardedTick);

    return () => {
      disposed = true;
      cancelAnimationFrame(rafId);
      redrawRef.current = null;
      frontRef.current = null;
      glowRef.current = null;
      tileGeometry.dispose();
      groundGeometry.dispose();
      sideMaterial.dispose();
      frontMaterial.dispose();
      backMaterial.dispose();
      shadowMaterial.dispose();
      glowMaterial.dispose();
      dustMaterial.dispose();
      backTexture.dispose();
      blobTexture.dispose();
      dustTexture.dispose();
      renderer.dispose();
      if (renderer.domElement.parentNode === container) {
        container.removeChild(renderer.domElement);
      }
    };
  }, [landing, fullTurns, speedMultiplier, width, height, failFrame]);

  // ─── Art: loads on its own; the face swaps in when it arrives ───
  // Declared after the flight effect so the tile's material exists when this
  // runs; the flight effect also picks up an already-loaded texture on restart.
  useEffect(() => {
    if (!artSrc) return;
    let cancelled = false;
    let loaded: Texture | null = null;
    const loader = new TextureLoader();
    loader.setCrossOrigin("anonymous");
    loader.load(
      artSrc,
      (texture) => {
        if (cancelled) {
          texture.dispose();
          return;
        }
        const image = texture.image as { width?: number; height?: number } | undefined;
        const imageAspect = image?.width && image?.height ? image.width / image.height : TILE_ASPECT;
        const { repeat, offset } = coverCrop(imageAspect, TILE_ASPECT);
        texture.repeat.set(repeat[0], repeat[1]);
        texture.offset.set(offset[0], offset[1]);
        texture.colorSpace = SRGBColorSpace;
        loaded = texture;
        artRef.current = texture;
        if (frontRef.current) applyArt(frontRef.current, texture);
        redrawRef.current?.();
      },
      undefined,
      // A missing image keeps the placeholder face; the flip is cosmetic.
      () => {},
    );
    return () => {
      cancelled = true;
      if (!loaded) return;
      if (artRef.current === loaded) artRef.current = null;
      if (frontRef.current?.map === loaded) {
        applyArt(frontRef.current, null);
        redrawRef.current?.();
      }
      loaded.dispose();
    };
  }, [artSrc]);

  // ─── Accent: retints the glow in place ───
  useEffect(() => {
    const glow = glowRef.current;
    if (!glow) return;
    applyAccent(glow.mesh, glow.material, accentRgb);
    redrawRef.current?.();
  }, [accentRgb]);

  return (
    <div
      ref={containerRef}
      style={{
        width,
        height,
        pointerEvents: "none",
      }}
    />
  );
}
