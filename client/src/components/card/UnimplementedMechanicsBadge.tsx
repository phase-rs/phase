import { useTranslation } from "react-i18next";

/**
 * Render scale for the badge. Mirrors the `artCrop` / `fullCard` variant
 * vocabulary `CardArtFallback` already uses, so every card-surface component in
 * this directory names its render modes the same way.
 *
 * - `overlay` — pinned inside the art of a hand/battlefield card, where the
 *   badge shares the corner with the tap indicator and must stay unobtrusive.
 * - `corner` — hung outside the border of a stack entry, matching the ×N and
 *   status pills that already sit on that surface.
 */
export type UnimplementedMechanicsBadgeVariant = "overlay" | "corner";

export interface UnimplementedMechanicsBadgeProps {
  /**
   * Engine-provided `unimplemented_mechanics` for the object. The badge is the
   * single authority on when the warning shows: it renders nothing when this is
   * absent or empty, so no call site has to repeat the emptiness guard.
   */
  mechanics?: string[];
  variant?: UnimplementedMechanicsBadgeVariant;
}

const VARIANT_CLASSES: Record<UnimplementedMechanicsBadgeVariant, string> = {
  overlay: "absolute top-0.5 left-0.5 text-[8px] px-0.5 rounded-sm leading-tight",
  // Bottom-right is the stack entry's only free corner: ×N and the chosen-X
  // pill sit top-left, "Casting…"/"Next" top-right, and the auto-pass yield
  // pill bottom-left. Verified visually — a bottom-left badge overlapped the
  // yield pill, which no DOM assertion would have caught.
  corner: "absolute -bottom-2 -right-2 z-10 text-[11px] px-2 py-0.5 rounded-full shadow-md",
};

/**
 * Amber `!` warning shown on a card surface whose printed abilities include
 * mechanics the engine does not implement yet.
 *
 * Extracted from `CardImage` so hand, battlefield and stack surfaces render one
 * badge instead of three drifting copies (issue #4711). Display-only: the
 * `unimplemented_mechanics` projection is computed by the engine and forwarded
 * verbatim — this component neither derives nor filters it.
 */
export function UnimplementedMechanicsBadge({
  mechanics,
  variant = "overlay",
}: UnimplementedMechanicsBadgeProps) {
  const { t } = useTranslation("game");
  if (!mechanics || mechanics.length === 0) return null;

  const label = t("card.unimplemented", { mechanics: mechanics.join(", ") });
  return (
    <span
      className={`${VARIANT_CLASSES[variant]} bg-amber-500 font-bold text-black`}
      title={label}
      aria-label={label}
      data-testid="unimplemented-mechanics-badge"
    >
      !
    </span>
  );
}
