// ─── Dice3D ───
// Imperative three.js die roll, mounted into its own <canvas>. Mirrors the
// rAF + cleanup/dispose idiom of DeathShatter.tsx, swapping the 2D context for
// a WebGLRenderer.
//
// The die tumbles with decaying angular velocity about a deterministic axis
// (derived from `result` + `sides`, never Math.random — so the same roll looks
// identical across re-renders), then slerps to the orientation that shows
// `result` facing CAMERA_UP. `onSettle` fires exactly once when it comes to
// rest.

import { useEffect, useRef } from "react";
import {
  AmbientLight,
  BoxGeometry,
  CanvasTexture,
  Color,
  DirectionalLight,
  Mesh,
  MeshStandardMaterial,
  PerspectiveCamera,
  Quaternion,
  Scene,
  Sprite,
  SpriteMaterial,
  Vector3,
  WebGLRenderer,
} from "three";

import { dieDescriptor } from "./dieGeometry.ts";

interface Dice3DProps {
  sides: number;
  /** The engine's rolled value — the die MUST settle showing this. */
  result: number;
  /** Scales animation duration (default 1). */
  speedMultiplier?: number;
  /** Fired once when the die finishes settling. */
  onSettle?: () => void;
  /** Render size in px (the component owns its own <canvas>). */
  size?: number;
}

const DEFAULT_SIZE = 120;
const TOTAL_MS = 1600; // base roll duration (before speedMultiplier)
const TUMBLE_FRACTION = 0.6; // first 60% tumbles, last 40% settles
const FACE_TEXTURE_PX = 128;

/** Quadratic ease-out, matching CardSlamAnimation's return easing. */
function easeOutQuad(t: number): number {
  return 1 - (1 - t) * (1 - t);
}

/**
 * Deterministic pseudo-random unit axis from integer inputs. A small integer
 * hash spread across three components, normalized — stable per (result, sides)
 * so a given roll always tumbles the same way without touching Math.random.
 */
function deterministicAxis(result: number, sides: number): Vector3 {
  const seed = (result * 73856093) ^ (sides * 19349663);
  const x = ((seed & 0xff) / 255) * 2 - 1;
  const y = (((seed >> 8) & 0xff) / 255) * 2 - 1;
  const z = (((seed >> 16) & 0xff) / 255) * 2 - 1;
  const axis = new Vector3(x, y, z);
  if (axis.lengthSq() < 1e-6) axis.set(1, 1, 1);
  return axis.normalize();
}

/** Draw a die face number onto a square CanvasTexture. */
function makeNumberTexture(value: number): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = FACE_TEXTURE_PX;
  canvas.height = FACE_TEXTURE_PX;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    ctx.fillStyle = "#f5f1e6";
    ctx.fillRect(0, 0, FACE_TEXTURE_PX, FACE_TEXTURE_PX);
    ctx.fillStyle = "#1a1a2e";
    ctx.font = `bold ${FACE_TEXTURE_PX * 0.55}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(String(value), FACE_TEXTURE_PX / 2, FACE_TEXTURE_PX / 2);
  }
  const texture = new CanvasTexture(canvas);
  texture.needsUpdate = true;
  return texture;
}

/** Transparent sprite texture showing a number (for non-box polyhedra). */
function makeSpriteTexture(value: number): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = FACE_TEXTURE_PX;
  canvas.height = FACE_TEXTURE_PX;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    ctx.clearRect(0, 0, FACE_TEXTURE_PX, FACE_TEXTURE_PX);
    ctx.fillStyle = "#1a1a2e";
    ctx.font = `bold ${FACE_TEXTURE_PX * 0.6}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(String(value), FACE_TEXTURE_PX / 2, FACE_TEXTURE_PX / 2);
  }
  const texture = new CanvasTexture(canvas);
  texture.needsUpdate = true;
  return texture;
}

export function Dice3D({
  sides,
  result,
  speedMultiplier = 1,
  onSettle,
  size = DEFAULT_SIZE,
}: Dice3DProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const settledRef = useRef(false);
  const onSettleRef = useRef(onSettle);
  onSettleRef.current = onSettle;

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    settledRef.current = false;
    let disposed = false;
    let rafId = 0;

    const descriptor = dieDescriptor(sides);

    // ─── Scene / camera / renderer ───
    const scene = new Scene();
    const camera = new PerspectiveCamera(40, 1, 0.1, 100);
    camera.position.set(0, 1.4, 4.2);
    camera.lookAt(0, 0, 0);

    const renderer = new WebGLRenderer({ alpha: true, antialias: true });
    renderer.setSize(size, size);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    container.appendChild(renderer.domElement);

    scene.add(new AmbientLight(0xffffff, 0.7));
    const dirLight = new DirectionalLight(0xffffff, 0.9);
    dirLight.position.set(2, 4, 3);
    scene.add(dirLight);

    // ─── Geometry, materials, number labels ───
    const geometry = descriptor.geometry();
    const isBox = geometry instanceof BoxGeometry;

    // Track everything that needs disposal.
    const textures: CanvasTexture[] = [];
    const materials: MeshStandardMaterial[] = [];
    const spriteMaterials: SpriteMaterial[] = [];

    let mesh: Mesh;
    if (isBox) {
      // BoxGeometry exposes 6 material groups in the order
      // +X, -X, +Y, -Y, +Z, -Z. Map each to the descriptor's face value by
      // matching that side's outward normal to a derived face normal.
      const boxNormals: Vector3[] = [
        new Vector3(1, 0, 0),
        new Vector3(-1, 0, 0),
        new Vector3(0, 1, 0),
        new Vector3(0, -1, 0),
        new Vector3(0, 0, 1),
        new Vector3(0, 0, -1),
      ];
      const boxMaterials = boxNormals.map((normal) => {
        // Find the descriptor face whose normal best matches this box side.
        let bestFace = 0;
        let bestDot = -Infinity;
        for (let i = 0; i < descriptor.faceValue.length; i++) {
          const d = descriptor.faceNormal(i).dot(normal);
          if (d > bestDot) {
            bestDot = d;
            bestFace = i;
          }
        }
        const value = descriptor.faceValue[bestFace];
        const texture = makeNumberTexture(value);
        textures.push(texture);
        const material = new MeshStandardMaterial({
          map: texture,
          roughness: 0.4,
          metalness: 0.05,
        });
        materials.push(material);
        return material;
      });
      mesh = new Mesh(geometry, boxMaterials);
    } else {
      // Non-box polyhedron: single body material, number Sprites at centroids.
      const body = new MeshStandardMaterial({
        color: new Color(0xf5f1e6),
        roughness: 0.4,
        metalness: 0.05,
      });
      materials.push(body);
      mesh = new Mesh(geometry, body);

      for (let i = 0; i < descriptor.faceValue.length; i++) {
        const value = descriptor.faceValue[i];
        const texture = makeSpriteTexture(value);
        textures.push(texture);
        const spriteMaterial = new SpriteMaterial({
          map: texture,
          transparent: true,
          depthTest: false,
        });
        spriteMaterials.push(spriteMaterial);
        const sprite = new Sprite(spriteMaterial);
        // Park the label just outside the face along its normal so it sits on
        // the facet. Parented to the mesh, it inherits all roll rotation.
        const normal = descriptor.faceNormal(i);
        sprite.position.copy(normal.multiplyScalar(0.62));
        sprite.scale.setScalar(0.5);
        mesh.add(sprite);
      }
    }

    scene.add(mesh);

    // ─── Orientation targets ───
    const settleQuat = descriptor.orientationFor(result);
    const tumbleAxis = deterministicAxis(result, sides);
    // Total tumble revolutions: a few full spins for a satisfying roll.
    const tumbleTurns = 4 + (Math.abs(result) % 3);
    const totalMs = TOTAL_MS * speedMultiplier;
    const tumbleMs = totalMs * TUMBLE_FRACTION;
    const settleMs = totalMs - tumbleMs;

    // Orientation captured at the tumble→settle handoff, so the slerp starts
    // from wherever the tumble left the die.
    let handoffQuat: Quaternion | null = null;
    const baseQuat = new Quaternion();
    const tumbleQuat = new Quaternion();

    const start = performance.now();

    const tick = (now: number) => {
      if (disposed) return;

      const elapsed = now - start;

      if (elapsed < tumbleMs) {
        // Tumble: decaying angular velocity → angle eases out toward its max.
        const t = elapsed / tumbleMs;
        const angle = easeOutQuad(t) * tumbleTurns * Math.PI * 2;
        tumbleQuat.setFromAxisAngle(tumbleAxis, angle);
        mesh.quaternion.copy(baseQuat).multiply(tumbleQuat);

        // Vertical bounce arc (quad ease), purely visual.
        const bounce = Math.sin(t * Math.PI) * 0.4;
        mesh.position.y = bounce;
      } else if (elapsed < totalMs) {
        // Settle: slerp from the handoff pose to the result-showing pose.
        if (!handoffQuat) handoffQuat = mesh.quaternion.clone();
        const t = (elapsed - tumbleMs) / settleMs;
        const eased = easeOutQuad(t);
        mesh.quaternion.copy(handoffQuat).slerp(settleQuat, eased);
        mesh.position.y = (1 - eased) * 0.15;
      } else {
        mesh.quaternion.copy(settleQuat);
        mesh.position.y = 0;
        renderer.render(scene, camera);
        if (!settledRef.current) {
          settledRef.current = true;
          onSettleRef.current?.();
        }
        return;
      }

      renderer.render(scene, camera);
      rafId = requestAnimationFrame(tick);
    };

    rafId = requestAnimationFrame(tick);

    return () => {
      disposed = true;
      cancelAnimationFrame(rafId);
      geometry.dispose();
      for (const m of materials) m.dispose();
      for (const m of spriteMaterials) m.dispose();
      for (const t of textures) t.dispose();
      renderer.dispose();
      if (renderer.domElement.parentNode === container) {
        container.removeChild(renderer.domElement);
      }
    };
  }, [sides, result, speedMultiplier, size]);

  return (
    <div
      ref={containerRef}
      style={{
        width: size,
        height: size,
        pointerEvents: "none",
      }}
    />
  );
}
