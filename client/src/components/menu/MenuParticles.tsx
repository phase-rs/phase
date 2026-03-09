import { useEffect, useRef } from "react";

interface Particle {
  x: number;
  y: number;
  radius: number;
  speed: number;
  opacity: number;
  color: string;
  phase: number;
  frequency: number;
  amplitude: number;
}

const PARTICLE_COLORS = [
  "99, 102, 241", // indigo
  "34, 211, 238", // cyan
  "251, 191, 36", // amber
];

const PARTICLE_COUNT = 50;

function createParticle(canvasWidth: number, canvasHeight: number): Particle {
  return {
    x: Math.random() * canvasWidth,
    y: Math.random() * canvasHeight,
    radius: 2 + Math.random() * 2,
    speed: 10 + Math.random() * 20,
    opacity: 0.1 + Math.random() * 0.2,
    color: PARTICLE_COLORS[Math.floor(Math.random() * PARTICLE_COLORS.length)],
    phase: Math.random() * Math.PI * 2,
    frequency: 0.5 + Math.random() * 1.5,
    amplitude: 10 + Math.random() * 20,
  };
}

export function MenuParticles() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let animationId: number;
    let particles: Particle[] = [];
    let lastTime = performance.now();

    function resize() {
      canvas!.width = window.innerWidth;
      canvas!.height = window.innerHeight;
    }

    function initParticles() {
      particles = [];
      for (let i = 0; i < PARTICLE_COUNT; i++) {
        particles.push(createParticle(canvas!.width, canvas!.height));
      }
    }

    function animate(now: number) {
      const dt = (now - lastTime) / 1000;
      lastTime = now;

      ctx!.clearRect(0, 0, canvas!.width, canvas!.height);

      for (const p of particles) {
        p.y -= p.speed * dt;
        const xOffset = Math.sin(now / 1000 * p.frequency + p.phase) * p.amplitude;

        if (p.y + p.radius < 0) {
          p.y = canvas!.height + p.radius;
          p.x = Math.random() * canvas!.width;
        }

        ctx!.beginPath();
        ctx!.arc(p.x + xOffset, p.y, p.radius, 0, Math.PI * 2);
        ctx!.fillStyle = `rgba(${p.color}, ${p.opacity})`;
        ctx!.fill();
      }

      animationId = requestAnimationFrame(animate);
    }

    resize();
    initParticles();
    animationId = requestAnimationFrame(animate);

    window.addEventListener("resize", resize);

    return () => {
      cancelAnimationFrame(animationId);
      window.removeEventListener("resize", resize);
    };
  }, []);

  return (
    <canvas
      ref={canvasRef}
      className="pointer-events-none fixed inset-0"
      aria-hidden="true"
    />
  );
}
