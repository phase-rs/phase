// ─── Coin3D ───
// Imperative three.js coin flip, mounted into its own <canvas>. Same mount /
// rAF / dispose discipline as Dice3D and DeathShatter.
//
// A flattened cylinder (disc) flips about a horizontal axis with decaying
// angular velocity, then settles showing the requested `face`. Heads and tails
// each get a labelled CanvasTexture. `onSettle` fires once at rest.

import { useEffect, useRef } from "react";
import {
  AmbientLight,
  CanvasTexture,
  CylinderGeometry,
  DirectionalLight,
  Mesh,
  MeshStandardMaterial,
  PerspectiveCamera,
  Scene,
  Vector3,
  WebGLRenderer,
} from "three";

interface Coin3DProps {
  /** Which face to land on after the flip. */
  face: "heads" | "tails";
  /** Scales animation duration (default 1). */
  speedMultiplier?: number;
  /** Fired once when the coin finishes settling. */
  onSettle?: () => void;
  /** Render size in px (the component owns its own <canvas>). */
  size?: number;
  /** Display text per face (caller supplies, may be i18n'd). */
  labels?: { heads: string; tails: string };
}

const DEFAULT_SIZE = 120;
const TOTAL_MS = 1500;
const FACE_TEXTURE_PX = 256;
const DEFAULT_LABELS = { heads: "H", tails: "T" } as const;

// The flip axis: a horizontal axis (X). The coin's flat faces point along ±Y
// in local space (CylinderGeometry's axis is Y), so flipping about X swaps
// which flat face points toward the camera.
const FLIP_AXIS = new Vector3(1, 0, 0);

/** Quadratic ease-out. */
function easeOutQuad(t: number): number {
  return 1 - (1 - t) * (1 - t);
}

/** Draw a coin face label onto a CanvasTexture. */
function makeFaceTexture(label: string, accent: string): CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = FACE_TEXTURE_PX;
  canvas.height = FACE_TEXTURE_PX;
  const ctx = canvas.getContext("2d");
  if (ctx) {
    const half = FACE_TEXTURE_PX / 2;
    const grad = ctx.createRadialGradient(half, half, half * 0.1, half, half, half);
    grad.addColorStop(0, accent);
    grad.addColorStop(1, "#b8860b");
    ctx.fillStyle = grad;
    ctx.beginPath();
    ctx.arc(half, half, half, 0, Math.PI * 2);
    ctx.fill();

    ctx.fillStyle = "#1a1a2e";
    ctx.font = `bold ${FACE_TEXTURE_PX * 0.4}px sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(label, half, half);
  }
  const texture = new CanvasTexture(canvas);
  texture.needsUpdate = true;
  return texture;
}

export function Coin3D({
  face,
  speedMultiplier = 1,
  onSettle,
  size = DEFAULT_SIZE,
  labels = DEFAULT_LABELS,
}: Coin3DProps) {
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

    // ─── Scene / camera / renderer ───
    const scene = new Scene();
    const camera = new PerspectiveCamera(40, 1, 0.1, 100);
    camera.position.set(0, 0, 4.5);
    camera.lookAt(0, 0, 0);

    const renderer = new WebGLRenderer({ alpha: true, antialias: true });
    renderer.setSize(size, size);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    container.appendChild(renderer.domElement);

    scene.add(new AmbientLight(0xffffff, 0.7));
    const dirLight = new DirectionalLight(0xffffff, 0.9);
    dirLight.position.set(2, 3, 4);
    scene.add(dirLight);

    // ─── Coin mesh: flattened cylinder ───
    // Material group order for CylinderGeometry: [side, top (+Y), bottom (-Y)].
    const headsTexture = makeFaceTexture(labels.heads, "#ffd700");
    const tailsTexture = makeFaceTexture(labels.tails, "#e0c060");
    const sideMaterial = new MeshStandardMaterial({
      color: 0xc9a227,
      roughness: 0.5,
      metalness: 0.3,
    });
    const headsMaterial = new MeshStandardMaterial({
      map: headsTexture,
      roughness: 0.4,
      metalness: 0.3,
    });
    const tailsMaterial = new MeshStandardMaterial({
      map: tailsTexture,
      roughness: 0.4,
      metalness: 0.3,
    });

    const geometry = new CylinderGeometry(1, 1, 0.18, 48);
    const mesh = new Mesh(geometry, [sideMaterial, headsMaterial, tailsMaterial]);

    // Orient the coin so its flat faces face the camera at rest. The cylinder's
    // top (+Y, heads) must point toward +Z (camera). Rotate +90° about X maps
    // local +Y → +Z, showing heads; rotate -90° about X shows tails (local -Y
    // → +Z).
    const headsRest = Math.PI / 2;
    const tailsRest = -Math.PI / 2;
    const restAngle = face === "heads" ? headsRest : tailsRest;

    scene.add(mesh);

    // ─── Animation ───
    // Flip several full turns, decaying to the rest angle showing `face`.
    const totalMs = TOTAL_MS * speedMultiplier;
    const flipTurns = 5;
    // Final accumulated angle must equal restAngle modulo 2π so the chosen face
    // ends pointing at the camera. We interpolate angle from 0 → target where
    // target = flipTurns * 2π + restAngle.
    const targetAngle = flipTurns * Math.PI * 2 + restAngle;

    const start = performance.now();

    const tick = (now: number) => {
      if (disposed) return;

      const elapsed = now - start;
      const t = Math.min(elapsed / totalMs, 1);
      const eased = easeOutQuad(t);
      const angle = eased * targetAngle;

      mesh.quaternion.setFromAxisAngle(FLIP_AXIS, angle);
      // A small vertical hop for liveliness.
      mesh.position.y = Math.sin(t * Math.PI) * 0.5;

      renderer.render(scene, camera);

      if (t >= 1) {
        mesh.quaternion.setFromAxisAngle(FLIP_AXIS, targetAngle);
        mesh.position.y = 0;
        renderer.render(scene, camera);
        if (!settledRef.current) {
          settledRef.current = true;
          onSettleRef.current?.();
        }
        return;
      }

      rafId = requestAnimationFrame(tick);
    };

    rafId = requestAnimationFrame(tick);

    return () => {
      disposed = true;
      cancelAnimationFrame(rafId);
      geometry.dispose();
      sideMaterial.dispose();
      headsMaterial.dispose();
      tailsMaterial.dispose();
      headsTexture.dispose();
      tailsTexture.dispose();
      renderer.dispose();
      if (renderer.domElement.parentNode === container) {
        container.removeChild(renderer.domElement);
      }
    };
  }, [face, speedMultiplier, size, labels.heads, labels.tails]);

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
