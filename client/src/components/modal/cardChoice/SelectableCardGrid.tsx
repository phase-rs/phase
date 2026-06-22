import { useCallback, type CSSProperties } from "react";
import { motion } from "framer-motion";

import type { GameObject, ObjectId } from "../../../adapter/types.ts";
import { CardImage } from "../../card/CardImage.tsx";
import { objectImageProps } from "../../../services/cardImageLookup.ts";

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
}: SelectableCardGridProps) {
  const toggle = useCallback(
    (id: ObjectId) => {
      const next = new Set(value);
      if (next.has(id)) {
        next.delete(id);
      } else if (next.size < cap) {
        next.add(id);
      } else {
        return; // at cap: ignore new additions, mirroring the old strip behavior
      }
      onChange(next);
    },
    [value, cap, onChange],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col gap-2"
      onKeyDown={(e) => {
        if (e.key === "Enter" && canConfirm && onConfirm) {
          e.preventDefault();
          onConfirm();
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
      <div
        style={GRID_TILE_VARS}
        className="grid auto-rows-min grid-cols-[repeat(auto-fill,minmax(92px,1fr))] justify-items-center gap-2 overflow-y-auto p-1 sm:grid-cols-[repeat(auto-fill,minmax(104px,1fr))]"
      >
        {cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = value.has(id);
          return (
            <motion.button
              key={id}
              type="button"
              className={`relative rounded-lg transition ${
                isSelected
                  ? `z-10 ring-2 ${tone.ring}`
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 24, scale: 0.9 }}
              animate={{ opacity: isSelected ? 1 : 0.78, y: 0, scale: 1 }}
              transition={{ delay: Math.min(0.4, index * 0.012), duration: 0.2 }}
              onClick={() => toggle(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="small" />
              {isSelected && (
                <div
                  className={`absolute inset-0 flex items-center justify-center rounded-lg ${tone.overlay}`}
                >
                  <span
                    className={`rounded-full px-2 py-0.5 text-[11px] font-bold text-white ${tone.badge}`}
                  >
                    {badgeLabel}
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </div>
    </div>
  );
}
