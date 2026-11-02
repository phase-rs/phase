import type { ComponentType, ReactNode } from "react";

import type { Phase } from "../../adapter/types";

/**
 * Turn-phase glyphs for the phase-stop bar and the mobile phase chip/sheet.
 *
 * Hand-written SVG rather than an icon-library dependency, matching
 * `chrome/SparkleIcon.tsx` and `chrome/navIcons.tsx`: one file per icon family,
 * each glyph a `ComponentType<{ className?: string }>` so callers size and tint
 * it with their own classes.
 *
 * Shape language: a 24-unit grid, stroked with `currentColor` so every glyph
 * follows the button's text color through hover / active / disabled states
 * without any caller-side work. The phase-stop bar renders these at 12px
 * (below `lg`) and 16px (`lg`), so the paths stay legible at 12px — rounded
 * caps and joins, and no detail thinner than the shared 2-unit stroke.
 *
 * Two pairs of phases previously shipped visually indistinguishable glyphs, so
 * the bar gave no clue which phase a click would set a stop on:
 *   - `PreCombatMain` and `PostCombatMain` were the *same* diamond path.
 *   - `BeginCombat` and `CombatDamage` were both crossed swords, differing only
 *     by a small center dot.
 * Every glyph below is now a distinct shape; the two main phases are numbered
 * boxes, and combat damage is a flame.
 *
 * Geometry is adapted from Tabler Icons (https://tabler.io/icons, MIT licensed,
 * Copyright (c) 2020-2026 Paweł Kuna). See NOTICE. Keeping the paths in-repo —
 * rather than depending on `@tabler/icons-react` for these twelve glyphs —
 * matches how every other icon in the client is sourced, and lets each glyph be
 * tuned for the 12px box it actually renders in.
 */

interface PhaseIconProps {
  className?: string;
}

/** Shared frame: the 24-unit grid, `currentColor` stroke, and an explicit
 *  `aria-hidden` — these glyphs are always decorative, because every surface
 *  that renders one is a button or row carrying its own accessible name. */
function PhaseGlyph({ className, children }: PhaseIconProps & { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={className ?? "h-4 w-4"}
    >
      {children}
    </svg>
  );
}

/** Untap: a clockwise arrow. */
export function UntapIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M4.05 11a8 8 0 1 1 .5 4m-.5 5v-5h5" />
    </PhaseGlyph>
  );
}

/** Upkeep: a gear, for the turn-based housekeeping step. */
export function UpkeepIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 0 0-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 0 0-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 0 0-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 0 0-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 0 0 1.066-2.573c-.94-1.543.826-3.31 2.37-2.37c1 .608 2.296.07 2.572-1.065" />
      <path d="M9 12a3 3 0 1 0 6 0a3 3 0 0 0-6 0" />
    </PhaseGlyph>
  );
}

/** Draw: a card back, for the draw step. */
export function DrawIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M19 5v14a2 2 0 0 1-2 2h-10a2 2 0 0 1-2-2v-14a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2" />
      <path d="M12 16l-3-4l3-4l3 4l-3 4" />
    </PhaseGlyph>
  );
}

/** Pre-combat main phase: a boxed "1". */
export function PreCombatMainIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M3 5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-14a2 2 0 0 1-2-2v-14" />
      <path d="M10 10l2-2v8" />
    </PhaseGlyph>
  );
}

/** Begin combat: crossed swords, for the start-of-combat step. */
export function BeginCombatIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M21 3v5l-11 9l-4 4l-3-3l4-4l9-11l5 0" />
      <path d="M5 13l6 6" />
      <path d="M14.32 17.32l3.68 3.68l3-3l-3.365-3.365" />
      <path d="M10 5.5l-2-2.5h-5v5l3 2.5" />
    </PhaseGlyph>
  );
}

/** Declare attackers: a single raised sword. Deliberately one blade, so it
 *  reads distinctly from the crossed swords that open combat. */
export function DeclareAttackersIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M20 4v5l-9 7l-4 4l-3-3l4-4l7-9l5 0" />
      <path d="M6.5 11.5l6 6" />
    </PhaseGlyph>
  );
}

/** Declare blockers: a shield. */
export function DeclareBlockersIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M12 3l7 2.5v5.5c0 5.5-7 9-7 9s-7-3.5-7-9v-5.5l7-2.5" />
    </PhaseGlyph>
  );
}

/** Combat damage: a flame — previously crossed swords, which made it look like
 *  begin-combat with a dot in the middle. */
export function CombatDamageIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M12 10.941c2.333-3.308.167-7.823-1-8.941c0 3.395-2.235 5.299-3.667 6.706c-1.43 1.408-2.333 3.294-2.333 5.588c0 3.704 3.134 6.706 7 6.706c3.866 0 7-3.002 7-6.706c0-1.712-1.232-4.403-2.333-5.588c-2.084 3.353-3.257 3.353-4.667 2.235" />
    </PhaseGlyph>
  );
}

/** End combat: a pennant. */
export function EndCombatIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M5 14h14l-4.5-4.5l4.5-4.5h-14v16" />
    </PhaseGlyph>
  );
}

/** Post-combat main phase: a boxed "2", so it is never mistaken for the "1"
 *  that opens the turn — the two main phases used to share one diamond. */
export function PostCombatMainIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M3 5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-14a2 2 0 0 1-2-2v-14" />
      <path d="M10 8h3a1 1 0 0 1 1 1v2a1 1 0 0 1-1 1h-2a1 1 0 0 0-1 1v2a1 1 0 0 0 1 1h3" />
    </PhaseGlyph>
  );
}

/** End step: an hourglass. */
export function EndIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M7 3h10M7 21h10M8 3v3l4 4l4-4V3M8 21v-3l4-4l4 4v3" />
    </PhaseGlyph>
  );
}

/** Cleanup: a brush — there is no broom in the reference set, and the library
 *  the mapping is tracked against has none either. */
export function CleanupIcon({ className }: PhaseIconProps) {
  return (
    <PhaseGlyph className={className}>
      <path d="M3 21v-4a4 4 0 1 1 4 4h-4" />
      <path d="M21 3a16 16 0 0 0-12.8 10.2" />
      <path d="M21 3a16 16 0 0 1-10.2 12.8" />
      <path d="M10.6 9a9 9 0 0 1 4.4 4.4" />
    </PhaseGlyph>
  );
}

/** The glyph for each turn phase. Keyed by `Phase` so the record stays
 *  exhaustive: adding a phase to the wire type is a compile error here until
 *  it has an icon, rather than a blank cell at runtime. */
export const PHASE_ICON_COMPONENTS: Record<Phase, ComponentType<PhaseIconProps>> = {
  Untap: UntapIcon,
  Upkeep: UpkeepIcon,
  Draw: DrawIcon,
  PreCombatMain: PreCombatMainIcon,
  BeginCombat: BeginCombatIcon,
  DeclareAttackers: DeclareAttackersIcon,
  DeclareBlockers: DeclareBlockersIcon,
  CombatDamage: CombatDamageIcon,
  EndCombat: EndCombatIcon,
  PostCombatMain: PostCombatMainIcon,
  End: EndIcon,
  Cleanup: CleanupIcon,
};

/** Renders the glyph for `phase`.
 *
 *  The single way callers reach a phase glyph: each surface picks its own size
 *  through `className`, but none of them re-implements the `phase → component`
 *  lookup. Do not use this to bypass `className`: a glyph with no size class
 *  falls back to the 16px default above. */
export function PhaseIconFor({
  phase,
  className,
}: {
  phase: Phase;
  className?: string;
}) {
  const Glyph = PHASE_ICON_COMPONENTS[phase];
  return <Glyph className={className} />;
}
