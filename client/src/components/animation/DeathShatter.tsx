import { useEffect, useRef } from "react";

interface Fragment {
  // Source region on the card image
  sx: number;
  sy: number;
  sw: number;
  sh: number;
  // Current position and state
  x: number;
  y: number;
  vx: number;
  vy: number;
  rotation: number;
  rotationSpeed: number;
  alpha: number;
}

interface DeathShatterProps {
  position: { x: number; y: number; width: number; height: number };
  imageUrl: string;
  onComplete: () => void;
}

const DURATION = 0.6; // seconds
const GRAVITY = 200; // px/s^2
const FRAGMENT_COLS = 3;
const FRAGMENT_ROWS = 4;
const RED_FLASH_DURATION = 0.1; // seconds

function generateFragments(width: number, height: number): Fragment[] {
  const fragments: Fragment[] = [];
  const cellW = width / FRAGMENT_COLS;
  const cellH = height / FRAGMENT_ROWS;
  const centerX = width / 2;
  const centerY = height / 2;

  for (let row = 0; row < FRAGMENT_ROWS; row++) {
    for (let col = 0; col < FRAGMENT_COLS; col++) {
      // Add random perturbation for organic look
      const perturbX = (Math.random() - 0.5) * cellW * 0.3;
      const perturbY = (Math.random() - 0.5) * cellH * 0.3;

      const sx = col * cellW;
      const sy = row * cellH;

      // Calculate outward velocity from center
      const fragCenterX = sx + cellW / 2;
      const fragCenterY = sy + cellH / 2;
      const dx = fragCenterX - centerX;
      const dy = fragCenterY - centerY;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      const speed = 150 + Math.random() * 150; // 150-300 px/s

      fragments.push({
        sx,
        sy,
        sw: cellW,
        sh: cellH,
        x: sx + perturbX,
        y: sy + perturbY,
        vx: (dx / dist) * speed,
        vy: (dy / dist) * speed,
        rotation: 0,
        rotationSpeed: (180 + Math.random() * 180) * (Math.random() > 0.5 ? 1 : -1), // 180-360 deg/s
        alpha: 1,
      });
    }
  }

  return fragments;
}

export function DeathShatter({ position, imageUrl, onComplete }: DeathShatterProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const startTimeRef = useRef<number>(0);
  const completedRef = useRef(false);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // Canvas covers the card area with generous padding for scattered fragments
    const padding = 200;
    canvas.width = position.width + padding * 2;
    canvas.height = position.height + padding * 2;

    const img = new Image();
    img.crossOrigin = "anonymous";

    img.onload = () => {
      // Create offscreen canvas with the card image
      const offscreen = document.createElement("canvas");
      offscreen.width = position.width;
      offscreen.height = position.height;
      const offCtx = offscreen.getContext("2d");
      if (!offCtx) return;
      offCtx.drawImage(img, 0, 0, position.width, position.height);

      const fragments = generateFragments(position.width, position.height);
      startTimeRef.current = performance.now();

      const tick = (now: number) => {
        if (completedRef.current) return;

        const elapsed = (now - startTimeRef.current) / 1000;
        const progress = Math.min(elapsed / DURATION, 1);

        ctx.clearRect(0, 0, canvas.width, canvas.height);

        for (const frag of fragments) {
          const dt = elapsed;
          const currentX = frag.x + frag.vx * dt + padding;
          const currentY = frag.y + frag.vy * dt + 0.5 * GRAVITY * dt * dt + padding;
          const currentRotation = frag.rotation + frag.rotationSpeed * dt;
          const currentAlpha = 1 - progress;

          if (currentAlpha <= 0) continue;

          ctx.save();
          ctx.globalAlpha = currentAlpha;
          ctx.translate(currentX + frag.sw / 2, currentY + frag.sh / 2);
          ctx.rotate((currentRotation * Math.PI) / 180);

          // Red tint flash in first 0.1s
          if (elapsed < RED_FLASH_DURATION) {
            // Draw the fragment
            ctx.drawImage(
              offscreen,
              frag.sx, frag.sy, frag.sw, frag.sh,
              -frag.sw / 2, -frag.sh / 2, frag.sw, frag.sh,
            );
            // Red overlay
            ctx.globalCompositeOperation = "source-atop";
            ctx.fillStyle = `rgba(239, 68, 68, ${0.5 * (1 - elapsed / RED_FLASH_DURATION)})`;
            ctx.fillRect(-frag.sw / 2, -frag.sh / 2, frag.sw, frag.sh);
            ctx.globalCompositeOperation = "source-over";
          } else {
            ctx.drawImage(
              offscreen,
              frag.sx, frag.sy, frag.sw, frag.sh,
              -frag.sw / 2, -frag.sh / 2, frag.sw, frag.sh,
            );
          }

          ctx.restore();
        }

        if (progress >= 1) {
          completedRef.current = true;
          onComplete();
          return;
        }

        requestAnimationFrame(tick);
      };

      requestAnimationFrame(tick);
    };

    img.onerror = () => {
      // If image fails to load, complete immediately
      onComplete();
    };

    img.src = imageUrl;

    return () => {
      completedRef.current = true;
    };
  }, [position, imageUrl, onComplete]);

  const padding = 200;

  return (
    <canvas
      ref={canvasRef}
      style={{
        position: "fixed",
        left: position.x - padding,
        top: position.y - padding,
        width: position.width + padding * 2,
        height: position.height + padding * 2,
        pointerEvents: "none",
        zIndex: 46,
      }}
    />
  );
}
