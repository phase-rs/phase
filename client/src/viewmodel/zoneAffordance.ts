/**
 * Per-zone color theme for the "castable from elsewhere" affordance.
 *
 * Exile keeps the violet "magical banish" identity; graveyard uses emerald to
 * match MTG's recursion color (escape/disturb/regrowth are green-black) and to
 * read instantly distinct from exile's purple over card art. Both deliberately
 * avoid cyan (the in-hand castable ring) and amber (the commander / pile
 * affordance) so every glow color means exactly one thing.
 *
 * Tailwind requires complete literal class strings for JIT extraction, so each
 * field is a full class list rather than an interpolated color token.
 *
 * Consumed by `PlayerHand` for the in-fan castable graveyard/exile "wings".
 */
export interface ZoneTheme {
  /** Border + fill for the collapsed shadow-stack layers. */
  stackLayer: string;
  /** Card image border. */
  cardBorder: string;
  /** Translucent overlay for the collapsed stack's top card (lighter hover). */
  overlayStack: string;
  /** Translucent overlay for an individual expanded / in-fan card. */
  overlayCard: string;
  /** Count badge fill + text + ring. */
  badge: string;
  /** Hover "view" pill fill + text. */
  expandPill: string;
  /** Expanded-row container border. */
  expandedBorder: string;
  /** Zone label chip fill + text. */
  label: string;
  /** Castable glow ring + colored drop shadow. */
  ring: string;
}

export const ZONE_THEME: Record<"exile" | "graveyard", ZoneTheme> = {
  exile: {
    stackLayer: "border-purple-500/30 bg-purple-950/40",
    cardBorder: "border-purple-400/60",
    overlayStack: "bg-purple-600/30 group-hover:bg-purple-600/15",
    overlayCard: "bg-purple-600/30 group-hover:bg-purple-600/10",
    badge: "bg-purple-900 text-purple-200 ring-purple-500/60",
    expandPill: "bg-purple-800/80 text-purple-100",
    expandedBorder: "border-purple-500/40",
    label: "bg-purple-700 text-purple-100",
    ring: "ring-2 ring-purple-400/70 shadow-[0_0_12px_3px_rgba(147,51,234,0.5)]",
  },
  graveyard: {
    stackLayer: "border-emerald-500/30 bg-emerald-950/40",
    cardBorder: "border-emerald-400/60",
    overlayStack: "bg-emerald-500/30 group-hover:bg-emerald-500/15",
    overlayCard: "bg-emerald-500/30 group-hover:bg-emerald-500/10",
    badge: "bg-emerald-900 text-emerald-200 ring-emerald-500/60",
    expandPill: "bg-emerald-800/80 text-emerald-100",
    expandedBorder: "border-emerald-500/40",
    label: "bg-emerald-700 text-emerald-100",
    ring: "ring-2 ring-emerald-400/70 shadow-[0_0_12px_3px_rgba(16,185,129,0.5)]",
  },
};
