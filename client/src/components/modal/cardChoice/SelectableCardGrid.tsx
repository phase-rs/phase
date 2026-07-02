import { useCallback, useEffect, useMemo, useRef, type CSSProperties } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { GameObject, ObjectId } from "../../../adapter/types.ts";
import { CardImage } from "../../card/CardImage.tsx";
import { objectImageProps } from "../../../services/cardImageLookup.ts";
import { applyBulk, rangeAdd } from "./gridSelection.ts";
import { useCardOrganizer } from "./useCardOrganizer.ts";
import { CardOrganizerToolbar } from "./CardOrganizerToolbar.tsx";

export interface SelectableCardGridProps {
  cards: ObjectId[];
  objects: Record<ObjectId, GameObject | undefined>;
  value: Set<ObjectId>;
  onChange: (next: Set<ObjectId>) => void;
  cap: number;
  tone: { ring: string; overlay: string; badge: string };
  badgeLabel: string;
  counterText: string;
  hoverProps: (id: ObjectId) => Record<string, unknown>;
  onConfirm?: () => void;
  canConfirm?: boolean;
  showToolbar?: boolean;
}

// Shrunk tile dimensions. ChoiceOverlay applies `.card-scale-reset`, which hard-
// declares --card-w/--card-h, so we must override BOTH here (not via CardImage
// `size`, which only changes fetched image resolution). ~92px keeps ~7-8 columns
// of full card frames per row so a 30-card hand fits without horizontal scroll.
const GRID_TILE_VARS = {
  "--card-w": "92px",
  "--card-h": "129px",
} as CSSProperties;

export default function SelectableCardGrid({
  cards,
  objects,
  value,
  onChange,
  cap,
  tone,
  badgeLabel,
  counterText,
  hoverProps,
  onConfirm,
  canConfirm,
  showToolbar,
}: SelectableCardGridProps) {
  const { t } = useTranslation("game");
  // Shared organize mechanism. The grid surfaces sort + group (no hide-filter):
  // hiding eligible cards mid-prompt would confuse a "choose N" selection, so the
  // filter axis stays "none" here while the player's hand opts into it.
  const { sort, setSort, group, setGroup, ordered, groups } = useCardOrganizer({ cards, objects });
  const lastIndexRef = useRef<number | null>(null);

  const orderedIndexMap = useMemo(
    () => new Map(ordered.map((id, i) => [id, i])),
    [ordered],
  );

  // Reset the shift-range anchor whenever the displayed order changes, so a stale
  // ordered-index can't anchor a range against a reordered or mutated list.
  // `ordered` folds in the sort key + the underlying cards/objects (stable store
  // slices during a prompt), so it is the single source of truth for index
  // validity — covering sort changes and any card-list mutation. Grouping only
  // re-buckets the same ids without changing ordered-indices, so a regroup keeps
  // a valid anchor; `group` is intentionally not a dependency.
  useEffect(() => {
    lastIndexRef.current = null;
  }, [ordered]);

  const bulk = useCallback(
    (action: "all" | "invert" | "clear") => onChange(applyBulk(action, ordered, value, cap)),
    [ordered, value, cap, onChange],
  );

  const clickTile = useCallback(
    (id: ObjectId, orderedIndex: number, shiftKey: boolean) => {
      if (shiftKey && lastIndexRef.current != null) {
        onChange(rangeAdd(ordered, lastIndexRef.current, orderedIndex, value, cap));
        return;
      }
      lastIndexRef.current = orderedIndex;
      const next = new Set(value);
      if (next.has(id)) next.delete(id);
      else if (next.size < cap) next.add(id);
      else return;
      onChange(next);
    },
    [ordered, value, cap, onChange],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col gap-2"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement) return;
        switch (e.key) {
          case "Enter":
            // Only confirm when the grid container itself is focused; otherwise
            // Enter must reach the focused child (toolbar/card button) so its
            // native activation isn't shadowed by a modal-closing confirm.
            if (e.target === e.currentTarget && canConfirm && onConfirm) {
              e.preventDefault();
              onConfirm();
            }
            break;
          case "a": e.preventDefault(); bulk("all"); break;
          case "i": e.preventDefault(); bulk("invert"); break;
          case "c": e.preventDefault(); bulk("clear"); break;
        }
      }}
    >
      <div
        role="status"
        aria-live="polite"
        className="px-1 text-sm font-semibold text-slate-200"
      >
        {counterText}
      </div>
      {showToolbar !== false && (
        <div className="flex flex-wrap items-center gap-2 px-1 text-xs">
          <button type="button" className="rounded-md border border-white/15 bg-black/30 px-2 py-1 hover:bg-white/10" onClick={() => bulk("all")}>{t("cardChoice.bulk.selectAll")}</button>
          <button type="button" className="rounded-md border border-white/15 bg-black/30 px-2 py-1 hover:bg-white/10" onClick={() => bulk("invert")}>{t("cardChoice.bulk.invert")}</button>
          <button type="button" className="rounded-md border border-white/15 bg-black/30 px-2 py-1 hover:bg-white/10" onClick={() => bulk("clear")}>{t("cardChoice.bulk.clear")}</button>
          <CardOrganizerToolbar
            className="ml-auto flex items-center gap-2 text-slate-300"
            sort={sort}
            onSortChange={setSort}
            group={group}
            onGroupChange={setGroup}
            showSort
            showGroup
          />
        </div>
      )}
      <div style={GRID_TILE_VARS} className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-1">
        {groups.map((g) => (
          <div key={g.key || "all"} className="flex flex-col gap-1">
            {g.key && (
              <div className="px-1 text-[11px] font-bold uppercase tracking-wide text-slate-400">
                {g.key} ({g.ids.length})
              </div>
            )}
            <div className="grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(92px,1fr))] justify-items-center gap-2 sm:grid-cols-[repeat(auto-fill,minmax(104px,1fr))]">
              {g.ids.map((id) => {
                const obj = objects[id];
                if (!obj) return null;
                const orderedIndex = orderedIndexMap.get(id) ?? -1;
                const isSelected = value.has(id);
                return (
                  <motion.button
                    key={id}
                    type="button"
                    className={`relative rounded-lg transition ${isSelected ? `z-10 ring-2 ${tone.ring}` : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"}`}
                    initial={{ opacity: 0, y: 24, scale: 0.9 }}
                    animate={{ opacity: isSelected ? 1 : 0.78, y: 0, scale: 1 }}
                    transition={{ duration: 0.18 }}
                    onClick={(e) => clickTile(id, orderedIndex, e.shiftKey)}
                    {...hoverProps(id)}
                  >
                    <CardImage {...objectImageProps(obj)} size="small" />
                    {isSelected && (
                      <div className={`absolute inset-0 flex items-center justify-center rounded-lg ${tone.overlay}`}>
                        <span className={`rounded-full px-2 py-0.5 text-[11px] font-bold text-white ${tone.badge}`}>{badgeLabel}</span>
                      </div>
                    )}
                  </motion.button>
                );
              })}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
