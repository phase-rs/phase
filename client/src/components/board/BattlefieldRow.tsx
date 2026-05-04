import { useRef, useState, useEffect } from "react";

import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { GroupedPermanentDisplay } from "./GroupedPermanent.tsx";
import {
  GROUP_STAGGER_PX,
  getCreatureGroupRenderMode,
  type BattlefieldRowType,
  visibleCardSlotCount,
  visibleStaggerCount,
} from "./groupRenderMode.ts";

interface BattlefieldRowProps {
  groups: GroupedPermanent[];
  rowType: BattlefieldRowType;
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
  const combatMode = useUiStore((s) => s.combatMode);
  const { committedAttackerIds } = useBoardInteractionState();
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerSize, setContainerSize] = useState<{ width: number; height: number } | null>(null);
  const [expandedGroupIds, setExpandedGroupIds] = useState<Set<number>>(() => new Set());

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

  useEffect(() => {
    const currentGroupIds = new Set(groups.map((group) => group.ids[0]));
    setExpandedGroupIds((previous) => {
      let changed = false;
      const next = new Set<number>();
      for (const groupId of previous) {
        if (currentGroupIds.has(groupId)) {
          next.add(groupId);
        } else {
          changed = true;
        }
      }
      return changed ? next : previous;
    });
  }, [groups]);

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
  const MAX_CARD_H = isCompactHeight ? 90 : 150;
  let creatureWrap = false;
  const renderedGroups = groups.map((group) => {
    const manualExpanded = expandedGroupIds.has(group.ids[0]);
    const containsCommittedAttackerDuringBlockers =
      rowType === "creatures"
      && combatMode === "blockers"
      && group.ids.some((id) => committedAttackerIds.has(id));
    const renderMode = getCreatureGroupRenderMode(group, rowType, {
      manualExpanded,
      containsCommittedAttackerDuringBlockers,
    });
    return { group, manualExpanded, renderMode };
  });
  const totalVisibleCardSlots = renderedGroups.reduce(
    (total, { group, renderMode }) => total + visibleCardSlotCount(renderMode, group),
    0,
  );
  const totalVisibleStagger = renderedGroups.reduce(
    (total, { group, renderMode }) => total + visibleStaggerCount(renderMode, group) * GROUP_STAGGER_PX,
    0,
  );

  if (rowType === "creatures") {
    if (containerSize && containerSize.height > 0) {
      // Measure-based sizing: fill available space
      const { width: cw, height: ch } = containerSize;
      const gap = 8; // gap-2
      const n = totalVisibleCardSlots;
      const activeAr = isArtCrop ? ART_CROP_AR : FULL_CARD_AR;

      // Try single-row first. Use the *natural* height (dictated by width
      // per group) as the wrap-or-not decision signal — if it's below
      // MIN_CARD_H, cards would shrink illegibly, so wrap to multi-row.
      // Only the rendered height is clamped up to MIN_CARD_H, keeping the
      // decision independent of the clamp.
      const availableForCards = cw - Math.max(0, n - 1) * gap - totalVisibleStagger;
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
          const staggerPerRow = totalVisibleStagger / rows; // approximate
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
      const creatureScale = getCreatureScale(totalVisibleCardSlots, battlefieldCardDisplay);
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
      className={`relative flex ${minH} ${rowType === "creatures" ? `${creatureClass} ${creatureWrap ? "gap-x-2 gap-y-3" : "gap-2"}` : "flex-wrap items-center gap-2"} ${ROW_JUSTIFY[rowType]} ${className ?? ""}`}
      style={rowStyle}
    >
      {rowType === "creatures" && expandedGroupIds.size > 0 && (
        <button
          type="button"
          className="absolute right-1 top-1 z-50 flex h-7 w-7 items-center justify-center rounded-full bg-black/85 text-white ring-1 ring-white/70 shadow-[0_2px_8px_rgba(0,0,0,0.7)] transition-transform hover:scale-105 hover:bg-slate-800"
          onClick={() => setExpandedGroupIds(new Set())}
          aria-label="Regroup duplicate creature groups"
          title="Regroup duplicate creature groups"
        >
          <span aria-hidden="true" className="flex flex-col items-center gap-0.5">
            <span className="block h-0.5 w-3 rounded bg-current" />
            <span className="block h-0.5 w-2.5 rounded bg-current" />
            <span className="block h-0.5 w-2 rounded bg-current" />
          </span>
        </button>
      )}
      {renderedGroups.map(({ group, manualExpanded }) => (
        <GroupedPermanentDisplay
          key={group.ids[0]}
          group={group}
          rowType={rowType}
          manualExpanded={manualExpanded}
          onExpand={() => {
            setExpandedGroupIds((previous) => {
              const next = new Set(previous);
              next.add(group.ids[0]);
              return next;
            });
          }}
        />
      ))}
    </div>
  );
}
