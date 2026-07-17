import type { CSSProperties } from "react";

import type { PTColor, PTDisplay } from "../../viewmodel/cardProps";
import { formatPTDelta, type PTContribution } from "../../viewmodel/attribution";

interface PTBoxProps {
  ptDisplay: PTDisplay;
  /** Keeps a creature-planeswalker's P/T clear of its bottom-right loyalty badge. */
  position?: "left" | "right";
  /**
   * Per-source +N/+M contributions from CR 613 layer 7c (ModifyPT).
   * When present and non-empty, attaches a hover tooltip listing each
   * contributor — "P/T (base X/Y) · +1/+1 from Lord · +2/+0 from Anthem".
   * Absent or empty when the engine attribution side-table is empty or
   * legacy serialized state predates attribution.
   */
  ptSources?: PTContribution[];
  basePower?: number | null;
  baseToughness?: number | null;
}

const COLOR_CLASSES: Record<PTColor, string> = {
  green: "text-green-400",
  red: "text-red-400",
  white: "text-white",
};

const ptBoxStyle = {
  fontSize: "clamp(8px, calc(var(--card-h, 88px) * 0.095), 12px)",
  paddingInline: "clamp(3px, calc(var(--card-w, 63px) * 0.055), 6px)",
  paddingBlock: "clamp(1px, calc(var(--card-h, 88px) * 0.012), 2px)",
} as CSSProperties;

function formatTooltip(
  ptDisplay: PTDisplay,
  sources: PTContribution[],
  basePower: number | null | undefined,
  baseToughness: number | null | undefined,
): string {
  const baseLine =
    basePower != null && baseToughness != null
      ? `${ptDisplay.power}/${ptDisplay.toughness} (base ${basePower}/${baseToughness})`
      : `${ptDisplay.power}/${ptDisplay.toughness}`;
  const lines = [baseLine];
  for (const c of sources) {
    lines.push(`${formatPTDelta(c)} from ${c.sourceName}`);
  }
  return lines.join("\n");
}

export function PTBox({
  ptDisplay,
  position = "right",
  ptSources,
  basePower,
  baseToughness,
}: PTBoxProps) {
  const title =
    ptSources && ptSources.length > 0
      ? formatTooltip(ptDisplay, ptSources, basePower, baseToughness)
      : undefined;
  return (
    <div
      className={`absolute bottom-0 z-20 flex items-center gap-px bg-black/80 font-bold leading-none ${position === "left" ? "left-0 rounded-tr" : "right-0 rounded-tl"}`}
      title={title}
      style={ptBoxStyle}
    >
      <span className={COLOR_CLASSES[ptDisplay.powerColor]}>
        {ptDisplay.power}
      </span>
      <span className="text-gray-400">/</span>
      <span className={COLOR_CLASSES[ptDisplay.toughnessColor]}>
        {ptDisplay.toughness}
      </span>
    </div>
  );
}
