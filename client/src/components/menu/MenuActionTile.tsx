import { motion } from "framer-motion";
import type { ReactNode } from "react";

import { TileMotifLayer, type TileMotif } from "./TileMotif";

/** Tonal accent for a menu action tile. Mirrors the design system's four-tone
 *  vocabulary (the home dashboard uses arcane/jade/ember for its three tiles). */
export type MenuTileTone = "arcane" | "jade" | "ember";

interface ToneStyle {
  text: string;
  border: string;
  wash: string;
  token: string;
  /** Soft accent disc that blooms behind the art-window icon on hover. */
  glow: string;
  /** Accent-tinted lift shadow on hover (panel shadow + a colored halo). */
  glowShadow: string;
  /** Tone as a space-separated rgb channel, for the motif particle field /
   *  hero glyph (`rgb(<rgb>)`). Mirrors the rgb used by `glowShadow`. */
  rgb: string;
}

const TONE: Record<MenuTileTone, ToneStyle> = {
  arcane: {
    text: "text-arcane-text",
    border: "border-arcane/40",
    wash: "from-arcane/12",
    token: "border-arcane/60 text-arcane-soft",
    glow: "bg-arcane/30",
    glowShadow: "hover:shadow-[0_18px_54px_rgba(0,0,0,0.22),0_0_34px_-10px_rgba(56,189,248,0.55)]",
    rgb: "56 189 248",
  },
  jade: {
    text: "text-jade-text",
    border: "border-jade/40",
    wash: "from-jade/12",
    token: "border-jade/60 text-jade-soft",
    glow: "bg-jade/30",
    glowShadow: "hover:shadow-[0_18px_54px_rgba(0,0,0,0.22),0_0_34px_-10px_rgba(52,211,153,0.55)]",
    rgb: "52 211 153",
  },
  ember: {
    text: "text-ember-text",
    border: "border-ember/40",
    wash: "from-ember/12",
    token: "border-ember/60 text-ember-soft",
    glow: "bg-ember/30",
    glowShadow: "hover:shadow-[0_18px_54px_rgba(0,0,0,0.22),0_0_34px_-10px_rgba(245,158,11,0.55)]",
    rgb: "245 158 11",
  },
};

interface MenuActionTileProps {
  title: string;
  description: string;
  tone: MenuTileTone;
  /** Label for the call-to-action footer (e.g. "Enter"). */
  enterLabel: string;
  onClick: () => void;
  disabled?: boolean;
  /** Renders the section icon at the requested size — the tile draws it twice
   *  (a large faint art-window backdrop and a small title-bar token), so the
   *  caller controls whether that's an <img> section icon or an inline SVG. */
  renderIcon: (className: string) => ReactNode;
  /** Optional thematic hover treatment. When set, the art window's rest-state
   *  section ghost cross-fades on hover into a crisp themed hero glyph wrapped
   *  in a tone-colored particle field (see {@link TileMotifLayer}). */
  motif?: TileMotif;
}

/**
 * The signature "bento" action tile used across the menu surfaces: a serif
 * title bar with a tone-ringed icon token, a tone-washed art window holding a
 * large faint icon, and a flavor/description body ending in an "ENTER →" cue.
 * Shared by the home dashboard and the draft landing so both read identically.
 */
export function MenuActionTile({
  title,
  description,
  tone,
  enterLabel,
  onClick,
  disabled = false,
  renderIcon,
  motif,
}: MenuActionTileProps) {
  const t = TONE[tone];
  // When a motif owns the hover, the section icon resolves from its faint
  // rotated rest state into a crisp, upright, brightened focus while the
  // particle field haloes it — same icon throughout, no jarring glyph swap.
  // Without a motif it keeps its subtler brighten-on-hover. Disabled tiles
  // never animate (no hover label fires).
  const showMotif = Boolean(motif) && !disabled;
  const ghostHover = showMotif
    ? "group-hover:rotate-0 group-hover:scale-110 group-hover:opacity-90"
    : "group-hover:-rotate-3 group-hover:scale-110 group-hover:opacity-30";
  return (
    <motion.button
      type="button"
      disabled={disabled}
      onClick={onClick}
      initial="rest"
      animate="rest"
      whileHover={disabled ? undefined : "hover"}
      className={`group relative flex flex-col gap-2 rounded-card border p-[7px] text-left transition-all duration-200 surface-card hover:-translate-y-[3px] ${
        disabled ? "cursor-not-allowed opacity-50" : `cursor-pointer ${t.border} hover:border-hairline-hover ${t.glowShadow}`
      }`}
    >
      <div className="flex items-center justify-between gap-2 rounded-[8px] bg-white/[0.06] px-3 py-2 shadow-[inset_0_0_0_1px_rgba(255,255,255,0.05)]">
        <span className="font-display text-[1.18rem] font-semibold tracking-[-0.02em] text-fg">
          {title}
        </span>
        <span className={`flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-full border-[1.5px] bg-black/40 ${t.token}`}>
          {renderIcon("h-4 w-4")}
        </span>
      </div>
      <div className={`relative flex min-h-[120px] flex-1 items-center justify-center overflow-hidden rounded-[6px] bg-gradient-to-br ${t.wash} to-black/40 shadow-[inset_0_0_0_1px_rgba(0,0,0,0.35)]`}>
        {/* Accent bloom: a soft tone disc that fades + swells in behind the icon
            on hover, giving each tile its own colored glow. */}
        <div className={`pointer-events-none absolute h-24 w-24 scale-75 rounded-full opacity-0 blur-2xl transition-all duration-300 group-hover:scale-110 group-hover:opacity-100 ${t.glow}`} />
        <span className={`relative -rotate-6 opacity-[0.14] transition-all duration-300 ${ghostHover}`}>
          {renderIcon("h-28 w-28")}
        </span>
        {/* Particle field renders in front of the icon so motes sparkle over it. */}
        {showMotif && <TileMotifLayer motif={motif!} color={`rgb(${t.rgb})`} />}
      </div>
      <div className="flex flex-col gap-2 rounded-[7px] bg-black/24 px-3 py-2.5 shadow-[inset_0_0_0_1px_rgba(255,255,255,0.04)]">
        <p className="text-[0.82rem] leading-snug text-fg-card-body">{description}</p>
        <span className={`inline-flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-[0.08em] ${t.text}`}>
          {enterLabel}
          <svg viewBox="0 0 24 24" className="h-3.5 w-3.5 fill-current"><path d="m13.2 5.4 1.4-1.4 8 8-8 8-1.4-1.4 5.6-5.6H2v-2h16.8l-5.6-5.6Z" /></svg>
        </span>
      </div>
    </motion.button>
  );
}
