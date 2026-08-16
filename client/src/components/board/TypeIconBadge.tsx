import { memo, type CSSProperties } from "react";

import { cardTypeIconClass } from "../../viewmodel/typeIcons.ts";
import { ManaFontIcon } from "../icons/ManaFontIcon.tsx";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

interface TypeIconBadgeProps {
  /** `obj.card_types.core_types`, already in canonical print order. */
  coreTypes: string[];
  className?: string;
  /** Inline style on the container — parents set `fontSize` here to a
   * card-relative `calc(...)` so the glyphs scale with the card. */
  style?: CSSProperties;
}

/**
 * Prominent mana-font glyph strip for a permanent's card types. One glyph per
 * core type that has a shipped glyph, in the engine's canonical type-line
 * order. Glyphs inherit `font-size` (set by the parent via `style`) and render
 * white on a subtle dark backing so they read on any art. Hovering a glyph
 * shows its type name in the shared `GameplayTooltip`.
 */
export const TypeIconBadge = memo(function TypeIconBadge({
  coreTypes,
  className,
  style,
}: TypeIconBadgeProps) {
  const icons = coreTypes.flatMap((type) => {
    const iconClass = cardTypeIconClass(type);
    return iconClass ? [{ type, iconClass }] : [];
  });
  if (icons.length === 0) return null;

  return (
    <div
      className={[
        "flex flex-row items-center gap-[0.2em] rounded-[0.25em] bg-black/45 px-[0.2em] py-[0.15em] text-white ring-1 ring-white/10 drop-shadow-[0_1px_2px_rgba(0,0,0,0.9)]",
        className,
      ]
        .filter(Boolean)
        .join(" ")}
      style={style}
    >
      {icons.map(({ type, iconClass }) => (
        <span key={type} className="group relative inline-flex leading-none">
          <ManaFontIcon iconClass={iconClass} fallbackText="" label={type} />
          <GameplayTooltip>{type}</GameplayTooltip>
        </span>
      ))}
    </div>
  );
});
