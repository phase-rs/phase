import { useRef, useState, useEffect } from "react";

import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
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
  const min = isArtCrop ? 0.78 : 0.72;
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
  const isCompactHeight = useIsCompactHeight();
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState<{ width: number; height: number } | null>(null);

  // groups.length in deps ensures the observer is set up after the first
  // non-empty render (the early return below means the ref is null when empty).
  const hasGroups = groups.length > 0;
  useEffect(() => {
    if (rowType !== "creatures" || !hasGroups) return;
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
  }, [rowType, hasGroups]);

  if (!hasGroups) return null;

  const isArtCrop = battlefieldCardDisplay === "art_crop";

  // Non-creature rows keep a min-height from CSS vars
  const minH = rowType !== "creatures"
    ? (isArtCrop ? "min-h-[calc(var(--art-crop-h)+8px)]" : "min-h-[calc(var(--card-h)+8px)]")
    : "";

  let rowStyle: React.CSSProperties | undefined;
  /** Minimum readable card height — below this, switch to multi-row wrapping.
   *  Lowered on compact-height (landscape phones) so creatures stay single-row
   *  in the limited vertical space rather than wrapping into a tiny grid. */
  const MIN_CARD_H = isCompactHeight ? 56 : 80;
  /** Maximum creature card height — prevents oversized cards with few creatures */
  const MAX_CARD_H = 150;
  let creatureWrap = false;

  if (rowType === "creatures") {
    if (containerSize && containerSize.height > 0) {
      // Measure-based sizing: fill available space
      const { width: cw, height: ch } = containerSize;
      const gap = 8; // gap-2
      const n = groups.length;
      const activeAr = isArtCrop ? ART_CROP_AR : FULL_CARD_AR;

      // Account for stagger width on stacked groups (each extra copy adds 20px)
      const staggerPx = 20;
      const totalStagger = groups.reduce((sum, g) => sum + Math.max(0, g.count - 1) * staggerPx, 0);

      // Try single-row first. Use the *natural* height (dictated by width
      // per group) as the wrap-or-not decision signal — if it's below
      // MIN_CARD_H, cards would shrink illegibly, so wrap to multi-row.
      // Only the rendered height is clamped up to MIN_CARD_H, keeping the
      // decision independent of the clamp.
      const availableForCards = cw - (n - 1) * gap - totalStagger;
      const widthPerGroup = n > 0 ? availableForCards / n : cw;
      const naturalCardH = Math.min(ch, widthPerGroup / activeAr, MAX_CARD_H);
      const singleRowCardH = Math.max(MIN_CARD_H, naturalCardH);

      if (naturalCardH >= MIN_CARD_H) {
        // Single row — cards fit at readable size
        rowStyle = {
          "--art-crop-w": `${singleRowCardH * ART_CROP_AR}px`,
          "--art-crop-h": `${singleRowCardH}px`,
          "--card-w": `${singleRowCardH * FULL_CARD_AR}px`,
          "--card-h": `${singleRowCardH}px`,
        } as React.CSSProperties;
      } else {
        // Multi-row wrapping — pick the row count that gives the largest
        // card height. Floors are the same MIN_CARD_H readability threshold
        // used for the single-row decision above; `bestH` is then clamped
        // up to MIN_CARD_H so the render never drops below the legibility
        // floor even under pathological creature counts.
        creatureWrap = true;
        const rowGap = 12; // gap-y-3
        let bestH = MIN_CARD_H;
        for (let rows = 2; rows <= 4; rows++) {
          const cardHFromHeight = (ch - (rows - 1) * rowGap) / rows;
          const groupsPerRow = Math.ceil(n / rows);
          const staggerPerRow = totalStagger / rows; // approximate
          const cardW = (cw - (groupsPerRow - 1) * gap - staggerPerRow) / groupsPerRow;
          const cardHFromWidth = cardW / activeAr;
          const cardH = Math.max(MIN_CARD_H, Math.min(cardHFromHeight, cardHFromWidth));
          if (cardH > bestH) {
            bestH = cardH;
          }
        }

        rowStyle = {
          "--art-crop-w": `${bestH * ART_CROP_AR}px`,
          "--art-crop-h": `${bestH}px`,
          "--card-w": `${bestH * FULL_CARD_AR}px`,
          "--card-h": `${bestH}px`,
        } as React.CSSProperties;
      }
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

  const creatureClass = creatureWrap
    ? "flex-wrap items-end content-end"
    : "flex-nowrap items-end";

  return (
    <div
      ref={containerRef}
      className={`flex ${minH} ${rowType === "creatures" ? `${creatureClass} ${creatureWrap ? "gap-x-2 gap-y-3" : "gap-2"}` : "flex-wrap items-center gap-2"} ${ROW_JUSTIFY[rowType]} ${className ?? ""}`}
      style={rowStyle}
    >
      {groups.map((group) => (
        <GroupedPermanentDisplay key={group.ids[0]} group={group} />
      ))}
    </div>
  );
}
