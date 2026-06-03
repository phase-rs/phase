import { motion, type Easing, type Variants } from "framer-motion";

/**
 * Thematic hover motif for a {@link MenuActionTile}. The motif names the
 * *behavior* of the particle field that haloes the tile's section icon on
 * hover — sharp combat sparks, pulsing network nodes, a pack spilling soft
 * motes — while the tile's `tone` supplies the color. The two axes compose, so
 * each motif covers a whole class of tiles (e.g. `network` serves both the home
 * "Online" tile and the draft "Pod" tile) rather than one card. The tile keeps
 * its own section icon throughout; the motif only adds the surrounding field.
 */
export type TileMotif = "swords" | "network" | "pack";

/** How a motif's particle field behaves. `spark` flings sharp motes outward
 *  fast (combat), `pulse` blinks nodes in place at orbit (network), `float`
 *  drifts soft embers gently upward (a pack spilling cards). */
type ParticleStyle = "spark" | "pulse" | "float";

const MOTIFS: Record<TileMotif, { particle: ParticleStyle; count: number }> = {
  swords: { particle: "spark", count: 16 },
  network: { particle: "pulse", count: 11 },
  pack: { particle: "float", count: 13 },
};

const PARTICLE_CONFIG: Record<ParticleStyle, { duration: number; radius: number; rise: number; ease: Easing }> = {
  spark: { duration: 0.9, radius: 50, rise: 0, ease: "easeOut" },
  pulse: { duration: 1.6, radius: 38, rise: 0, ease: "easeInOut" },
  float: { duration: 2.3, radius: 34, rise: -22, ease: "easeOut" },
};

/** Even ring placement with a slight per-index wobble so the field doesn't read
 *  as a rigid polygon. Deterministic (index-derived) — no per-render jitter. */
function ringOffset(i: number, n: number, radius: number): { dx: number; dy: number } {
  const angle = (i / n) * Math.PI * 2 + (i % 2) * 0.5;
  const r = radius * (0.7 + ((i * 3) % 5) / 10);
  return { dx: Math.cos(angle) * r, dy: Math.sin(angle) * r };
}

function Particle({ i, n, color, style }: { i: number; n: number; color: string; style: ParticleStyle }) {
  const cfg = PARTICLE_CONFIG[style];
  const { dx, dy } = ringOffset(i, n, cfg.radius);
  // Pulse holds its orbit position (a node blinking); the others travel out
  // from centre. Stagger the loop so the field shimmers rather than pulsing in
  // unison. Animation is driven entirely by the parent motion.button's hover
  // label propagating down to these `variants` — no local state.
  const isPulse = style === "pulse";
  const variants: Variants = {
    rest: { opacity: 0, scale: 0, x: isPulse ? dx : 0, y: isPulse ? dy : 0 },
    hover: {
      opacity: [0, 1, 0],
      scale: style === "spark" ? [0, 1, 0.2] : [0, 1, 0.35],
      x: isPulse ? dx : [0, dx],
      y: isPulse ? dy : [0, dy + cfg.rise],
      transition: {
        duration: cfg.duration,
        repeat: Infinity,
        delay: (i / n) * cfg.duration,
        ease: cfg.ease,
      },
    },
  };
  return (
    <motion.span
      variants={variants}
      className="absolute left-1/2 top-1/2 h-1.5 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full"
      style={{ background: color, boxShadow: `0 0 6px ${color}` }}
    />
  );
}

/**
 * The hover particle field for a tile's art window — a tone-colored ring of
 * motes that emanate around the (unchanged) section icon. The caller renders it
 * in front of the icon so the motes sparkle over it. `color` is the tile tone
 * as an `rgb(...)` string. Animates purely by inheriting the parent
 * motion.button's `whileHover="hover"` label, so it carries no state of its own.
 */
export function TileMotifLayer({ motif, color }: { motif: TileMotif; color: string }) {
  const spec = MOTIFS[motif];
  return (
    <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
      {Array.from({ length: spec.count }, (_, i) => (
        <Particle key={i} i={i} n={spec.count} color={color} style={spec.particle} />
      ))}
    </div>
  );
}
