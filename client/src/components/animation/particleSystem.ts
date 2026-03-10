// ─── Canvas Particle System ───
// Lightweight 2D particle engine with additive blending for glowing VFX.
// Runs on requestAnimationFrame, independent of React render cycle.
// Ported from Alchemy's particle engine, adapted for MTG context.

export interface RGB {
  r: number;
  g: number;
  b: number;
}

export interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  life: number;
  maxLife: number;
  size: number;
  startSize: number;
  endSize: number;
  r: number;
  g: number;
  b: number;
  alpha: number;
  startAlpha: number;
  gravity: number;
  drag: number;
  glow: number;
  style: "circle" | "ring";
  ringWidth: number;
}

export interface ActiveEffect {
  startTime: number;
  duration: number;
  update: (t: number, system: ParticleSystem) => void;
  draw?: (t: number, ctx: CanvasRenderingContext2D) => void;
  onComplete?: (system: ParticleSystem) => void;
}

const DEFAULT_PARTICLE: Particle = {
  x: 0,
  y: 0,
  vx: 0,
  vy: 0,
  life: 0.5,
  maxLife: 0.5,
  size: 4,
  startSize: 4,
  endSize: 0,
  r: 255,
  g: 200,
  b: 100,
  alpha: 1,
  startAlpha: 1,
  gravity: 0,
  drag: 0,
  glow: 0,
  style: "circle",
  ringWidth: 2,
};

// Size of pre-rendered glow sprite canvases (px). Large enough for smooth gradients.
const GLOW_SPRITE_SIZE = 128;
const MAX_PARTICLE_DPR = 2;

export class ParticleSystem {
  private particles: Particle[] = [];
  private effects: ActiveEffect[] = [];
  private canvas: HTMLCanvasElement | null = null;
  private ctx: CanvasRenderingContext2D | null = null;
  private animFrameId = 0;
  private lastTime = 0;
  private dpr = 1;
  private running = false;
  private glowSprites = new Map<number, HTMLCanvasElement>();
  private colorStrings = new Map<number, string>();

  attach(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: true })!;
    this.dpr = this.getTargetDpr();
    this.resize();
    this.lastTime = performance.now();
  }

  detach() {
    cancelAnimationFrame(this.animFrameId);
    this.running = false;
    this.particles = [];
    this.effects = [];
    this.canvas = null;
    this.ctx = null;
    this.glowSprites.clear();
    this.colorStrings.clear();
  }

  resize() {
    if (!this.canvas) return;
    this.dpr = this.getTargetDpr();
    const rect = this.canvas.getBoundingClientRect();
    this.canvas.width = rect.width * this.dpr;
    this.canvas.height = rect.height * this.dpr;
  }

  private getTargetDpr(): number {
    return Math.min(window.devicePixelRatio || 1, MAX_PARTICLE_DPR);
  }

  emit(partials: Partial<Particle>[]) {
    for (const partial of partials) {
      const p = { ...DEFAULT_PARTICLE, ...partial };
      p.maxLife = partial.maxLife ?? p.life;
      p.startAlpha = partial.startAlpha ?? p.alpha;
      p.startSize = partial.startSize ?? p.size;
      this.particles.push(p);
    }
    this.ensureRunning();
  }

  addEffect(effect: ActiveEffect) {
    this.effects.push(effect);
    this.ensureRunning();
  }

  private ensureRunning() {
    if (!this.running && this.canvas) {
      this.running = true;
      this.lastTime = performance.now();
      this.animFrameId = requestAnimationFrame(this.loop);
    }
  }

  private rgbKey(r: number, g: number, b: number): number {
    return (r << 16) | (g << 8) | b;
  }

  private getColorString(r: number, g: number, b: number): string {
    const key = this.rgbKey(r, g, b);
    let s = this.colorStrings.get(key);
    if (!s) {
      s = `rgb(${r}, ${g}, ${b})`;
      this.colorStrings.set(key, s);
    }
    return s;
  }

  /** Lazily create a 128x128 offscreen canvas with a radial gradient glow for this color. */
  private getGlowSprite(r: number, g: number, b: number): HTMLCanvasElement {
    const key = this.rgbKey(r, g, b);
    let sprite = this.glowSprites.get(key);
    if (!sprite) {
      sprite = document.createElement("canvas");
      sprite.width = GLOW_SPRITE_SIZE;
      sprite.height = GLOW_SPRITE_SIZE;
      const sctx = sprite.getContext("2d")!;
      const half = GLOW_SPRITE_SIZE / 2;
      const grad = sctx.createRadialGradient(half, half, 0, half, half, half);
      grad.addColorStop(0, `rgba(${r}, ${g}, ${b}, 1)`);
      grad.addColorStop(0.3, `rgba(${r}, ${g}, ${b}, 0.6)`);
      grad.addColorStop(1, `rgba(${r}, ${g}, ${b}, 0)`);
      sctx.fillStyle = grad;
      sctx.fillRect(0, 0, GLOW_SPRITE_SIZE, GLOW_SPRITE_SIZE);
      this.glowSprites.set(key, sprite);
    }
    return sprite;
  }

  /** Draw a filled circle directly (for projectile bodies, flashes). */
  drawGlowCircle(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    radius: number,
    color: RGB,
    alpha: number,
    glowRadius: number,
  ) {
    ctx.globalAlpha = alpha;
    if (glowRadius > 0) {
      const sprite = this.getGlowSprite(color.r, color.g, color.b);
      const spriteSize = (radius + glowRadius) * 2;
      ctx.drawImage(sprite, x - spriteSize / 2, y - spriteSize / 2, spriteSize, spriteSize);
    } else {
      ctx.fillStyle = this.getColorString(color.r, color.g, color.b);
      ctx.beginPath();
      ctx.arc(x, y, Math.max(radius, 0.5), 0, Math.PI * 2);
      ctx.fill();
    }
  }

  /** Draw a ring directly (for shockwaves). */
  drawGlowRing(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    radius: number,
    color: RGB,
    alpha: number,
    lineWidth: number,
    glowRadius: number,
  ) {
    const colorStr = this.getColorString(color.r, color.g, color.b);
    const r = Math.max(radius, 0.5);
    if (glowRadius > 0) {
      ctx.strokeStyle = colorStr;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);

      ctx.globalAlpha = alpha * 0.15;
      ctx.lineWidth = lineWidth + glowRadius;
      ctx.stroke();

      ctx.globalAlpha = alpha * 0.3;
      ctx.lineWidth = lineWidth + glowRadius * 0.5;
      ctx.stroke();

      ctx.globalAlpha = alpha;
      ctx.lineWidth = lineWidth;
      ctx.stroke();
    } else {
      ctx.globalAlpha = alpha;
      ctx.strokeStyle = colorStr;
      ctx.lineWidth = lineWidth;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.stroke();
    }
  }

  private loop = () => {
    if (this.particles.length === 0 && this.effects.length === 0) {
      this.running = false;
      if (this.ctx && this.canvas) {
        const w = this.canvas.width / this.dpr;
        const h = this.canvas.height / this.dpr;
        this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
        this.ctx.clearRect(0, 0, w, h);
      }
      return;
    }

    const now = performance.now();
    const dt = Math.min((now - this.lastTime) / 1000, 0.05);
    this.lastTime = now;

    this.updateEffects(now);
    this.updateParticles(dt);
    this.draw(now);

    this.animFrameId = requestAnimationFrame(this.loop);
  };

  private updateEffects(now: number) {
    for (let i = this.effects.length - 1; i >= 0; i--) {
      const effect = this.effects[i];
      const elapsed = now - effect.startTime;
      const t = Math.min(elapsed / effect.duration, 1);
      effect.update(t, this);
      if (t >= 1) {
        effect.onComplete?.(this);
        this.effects[i] = this.effects[this.effects.length - 1];
        this.effects.pop();
      }
    }
  }

  private updateParticles(dt: number) {
    for (let i = this.particles.length - 1; i >= 0; i--) {
      const p = this.particles[i];
      p.life -= dt;
      if (p.life <= 0) {
        this.particles[i] = this.particles[this.particles.length - 1];
        this.particles.pop();
        continue;
      }
      p.vy += p.gravity * dt;
      p.vx *= 1 - p.drag * dt;
      p.vy *= 1 - p.drag * dt;
      p.x += p.vx * dt;
      p.y += p.vy * dt;
      const t = 1 - p.life / p.maxLife;
      p.size = p.startSize + (p.endSize - p.startSize) * t;
      p.alpha = p.startAlpha * (p.life / p.maxLife);
    }
  }

  private draw(_now: number) {
    if (!this.ctx || !this.canvas) return;
    const ctx = this.ctx;
    const w = this.canvas.width / this.dpr;
    const h = this.canvas.height / this.dpr;

    ctx.save();
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    ctx.globalCompositeOperation = "lighter";

    // Draw active effects (projectile bodies, shockwaves)
    for (const effect of this.effects) {
      if (effect.draw) {
        const elapsed = _now - effect.startTime;
        const t = Math.min(elapsed / effect.duration, 1);
        effect.draw(t, ctx);
      }
    }

    // First pass: non-glowing circles
    for (const p of this.particles) {
      if (p.alpha <= 0 || p.size <= 0 || p.glow > 0 || p.style === "ring") continue;
      ctx.globalAlpha = p.alpha;
      ctx.fillStyle = this.getColorString(p.r, p.g, p.b);
      ctx.beginPath();
      ctx.arc(p.x, p.y, Math.max(p.size, 0.5), 0, Math.PI * 2);
      ctx.fill();
    }

    // Second pass: glowing circles — pre-rendered sprite instead of shadowBlur
    for (const p of this.particles) {
      if (p.alpha <= 0 || p.size <= 0 || p.glow <= 0 || p.style === "ring") continue;
      ctx.globalAlpha = p.alpha;
      const sprite = this.getGlowSprite(p.r, p.g, p.b);
      const spriteSize = (p.size + p.glow) * 2;
      ctx.drawImage(sprite, p.x - spriteSize / 2, p.y - spriteSize / 2, spriteSize, spriteSize);
    }

    // Third pass: rings (rare) — multi-pass glow
    for (const p of this.particles) {
      if (p.alpha <= 0 || p.size <= 0 || p.style !== "ring") continue;
      const colorStr = this.getColorString(p.r, p.g, p.b);
      const r = Math.max(p.size, 0.5);
      ctx.strokeStyle = colorStr;
      ctx.beginPath();
      ctx.arc(p.x, p.y, r, 0, Math.PI * 2);

      if (p.glow > 0) {
        ctx.globalAlpha = p.alpha * 0.15;
        ctx.lineWidth = p.ringWidth + p.glow;
        ctx.stroke();

        ctx.globalAlpha = p.alpha * 0.3;
        ctx.lineWidth = p.ringWidth + p.glow * 0.5;
        ctx.stroke();
      }

      ctx.globalAlpha = p.alpha;
      ctx.lineWidth = p.ringWidth;
      ctx.stroke();
    }

    ctx.globalCompositeOperation = "source-over";
    ctx.restore();
  }
}
