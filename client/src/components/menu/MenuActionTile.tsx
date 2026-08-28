import { motion } from "framer-motion";
import type { ReactNode } from "react";

import { TileMotifLayer, type TileMotif } from "./TileMotif";

/** Tonal accent for a menu action tile. Mirrors the design system's four-tone
 *  vocabulary (the home dashboard uses arcane/jade/ember for its three tiles). */
export type MenuTileTone = "arcane" | "jade" | "ember";

interface ToneStyle {
  text: string;
  border: string;
  token: string;
  wash: string;
  /** Tone as a space-separated rgb channel for the motif particle field. */
  rgb: string;
}

const TONE: Record<MenuTileTone, ToneStyle> = {
  arcane: {
    text: "text-arcane-text",
    border: "border-white/10",
    token: "border-arcane/60 text-arcane-soft",
    wash: "bg-[radial-gradient(100%_120%_at_100%_0%,rgba(56,189,248,0.14),transparent_62%)]",
    rgb: "56 189 248",
  },
  jade: {
    text: "text-jade-text",
    border: "border-white/10",
    token: "border-jade/60 text-jade-soft",
    wash: "bg-[radial-gradient(100%_120%_at_100%_0%,rgba(52,211,153,0.14),transparent_62%)]",
    rgb: "52 211 153",
  },
  ember: {
    text: "text-ember-text",
    border: "border-white/10",
    token: "border-ember/60 text-ember-soft",
    wash: "bg-[radial-gradient(100%_120%_at_100%_0%,rgba(245,158,11,0.16),transparent_62%)]",
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
  /** Renders the section icon at the requested size. The tile uses the same
   *  icon for its quiet watermark and its compact action token. */
  renderIcon: (className: string) => ReactNode;
  /** Optional, restrained hover particle treatment around the watermark. */
  motif?: TileMotif;
}

/**
 * The shared primary action control for the home dashboard and draft landing.
 * One quiet material surface holds the icon, title, explanation, and CTA so a
 * choice reads as one confident button rather than a stack of small panels.
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
  const showMotif = Boolean(motif) && !disabled;
  return (
    <motion.button
      type="button"
      disabled={disabled}
      onClick={onClick}
      initial="rest"
      animate="rest"
      whileHover={disabled ? undefined : "hover"}
      className={`group relative isolate flex min-h-[178px] overflow-hidden rounded-[12px] border text-left shadow-[0_12px_30px_rgba(0,0,0,0.22)] transition-all duration-200 surface-card focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/35 focus-visible:ring-offset-2 focus-visible:ring-offset-[#060a16] ${
        disabled ? "cursor-not-allowed opacity-50" : `cursor-pointer ${t.border} hover:-translate-y-0.5 hover:border-hairline-hover hover:shadow-[0_18px_38px_rgba(0,0,0,0.32)]`
      }`}
    >
      <span aria-hidden="true" className={`pointer-events-none absolute inset-0 ${t.wash}`} />
      <div aria-hidden="true" className="pointer-events-none absolute -right-4 -top-5 h-32 w-32 opacity-[0.10] transition-all duration-300 group-hover:-right-1 group-hover:opacity-[0.16]">
        {renderIcon("h-32 w-32")}
        {showMotif && <TileMotifLayer className="inset-0" motif={motif!} color={`rgb(${t.rgb})`} />}
      </div>
      <div className="relative z-10 flex w-full flex-col items-start px-5 py-5">
        <span className={`flex h-10 w-10 items-center justify-center rounded-[9px] border bg-black/28 shadow-[inset_0_1px_rgba(255,255,255,0.07)] ${t.token}`}>
          {renderIcon("h-5 w-5")}
        </span>
        <div className="mt-auto max-w-[15.5rem] pt-5">
          <h2 className="font-display text-[1.35rem] font-semibold leading-none tracking-[-0.025em] text-fg">
            {title}
          </h2>
          <p className="mt-2 text-[0.84rem] leading-snug text-fg-card-body">{description}</p>
          <span className={`mt-4 inline-flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-[0.11em] ${t.text}`}>
            {enterLabel}
            <svg viewBox="0 0 24 24" className="h-3.5 w-3.5 fill-current transition-transform duration-150 group-hover:translate-x-0.5"><path d="m13.2 5.4 1.4-1.4 8 8-8 8-1.4-1.4 5.6-5.6H2v-2h16.8l-5.6-5.6Z" /></svg>
          </span>
        </div>
      </div>
    </motion.button>
  );
}
