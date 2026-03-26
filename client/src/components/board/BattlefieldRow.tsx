import { useRef, useState, useEffect } from "react";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";

interface BattlefieldRowProps {
  groups: GroupedPermanent[];
  rowType: "creatures" | "lands" | "support" | "other";
  className?: string;
}

const ROW_JUSTIFY: Record<string, string> = {
  creatures: "justify-center",
  lands: "justify-start",
  support: "justify-end",
  other: "justify-end",
};

/** Aspect ratios: art crop is 4:3 (w:h), full card is 5:7 (w:h) */
const ART_CROP_AR = 4 / 3;
const FULL_CARD_AR = 5 / 7;

/**
 * Smooth creature scaling fallback (used before ResizeObserver measures container).
 * Starts large when few creatures are present, then shrinks continuously as more
 * are added. Uses inverse-sqrt decay past a threshold.
 */
function getCreatureScale(groupCount: number, display: "art_crop" | "full_card"): number {
  const isArtCrop = display === "art_crop";
  const max = isArtCrop ? 1.25 : 1.12;
  const min = isArtCrop ? 0.6 : 0.55;
  const threshold = 4;

  if (groupCount <= 1) return max;

  // Linear ramp-down from max to 1.0 between 2 and threshold
  if (groupCount <= threshold) {
    const t = (groupCount - 1) / (threshold - 1);
    return max - (max - 1) * t;
  }

  // Inverse-sqrt decay past threshold — continuous, no hard floor
  const excess = groupCount - threshold;
  return Math.max(min, 1 / Math.sqrt(1 + excess * 0.15));
}

export function BattlefieldRow({ groups, rowType, className }: BattlefieldRowProps) {
  const battlefieldCardDisplay = usePreferencesStore((s) => s.battlefieldCardDisplay);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState<{ width: number; height: number } | null>(null);

  useEffect(() => {
    if (rowType !== "creatures") return;
    const el = containerRef.current?.parentElement;
    if (!el) return;
    const observer = new ResizeObserver(([entry]) => {
      setContainerSize({
        width: entry.contentRect.width,
        height: entry.contentRect.height,
      });
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [rowType]);

  if (groups.length === 0) return null;

  const isArtCrop = battlefieldCardDisplay === "art_crop";

  // Non-creature rows keep a min-height from CSS vars
  const minH = rowType !== "creatures"
    ? (isArtCrop ? "min-h-[calc(var(--art-crop-h)+24px)]" : "min-h-[calc(var(--card-h)+8px)]")
    : "";

  let rowStyle: React.CSSProperties | undefined;

  if (rowType === "creatures") {
    if (containerSize && containerSize.height > 0) {
      // Measure-based sizing: fill available space
      const { width: cw, height: ch } = containerSize;
      const gap = 8; // gap-2
      const n = groups.length;
      const activeAr = isArtCrop ? ART_CROP_AR : FULL_CARD_AR;

      // Width per group if all fit in one row
      const widthPerGroup = n > 0 ? (cw - (n - 1) * gap) / n : cw;
      // Max card height from width constraint (all groups fit in one row)
      const fromWidth = widthPerGroup / activeAr;
      // Max card height from container height
      const fromHeight = ch;

      // Use the tighter constraint; minimum 40px
      const cardH = Math.max(40, Math.min(fromHeight, fromWidth));

      rowStyle = {
        "--art-crop-w": `${cardH * ART_CROP_AR}px`,
        "--art-crop-h": `${cardH}px`,
        "--card-w": `${cardH * FULL_CARD_AR}px`,
        "--card-h": `${cardH}px`,
      } as React.CSSProperties;
    } else {
      // Fallback before measurement
      const creatureScale = getCreatureScale(groups.length, battlefieldCardDisplay);
      rowStyle = {
        "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * var(--art-crop-viewport-scale) * ${creatureScale})`,
        "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * var(--art-crop-viewport-scale) * ${creatureScale} * 0.75)`,
        "--card-w": `calc(var(--card-base) * var(--card-size-scale) * var(--card-viewport-scale) * ${creatureScale})`,
        "--card-h": `calc(var(--card-base) * var(--card-size-scale) * var(--card-viewport-scale) * ${creatureScale} * 1.4)`,
      } as React.CSSProperties;
    }
  }

  return (
    <div
      ref={containerRef}
      className={`flex ${minH} flex-wrap items-center gap-2 ${ROW_JUSTIFY[rowType]} ${className ?? ""}`}
      style={rowStyle}
    >
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
