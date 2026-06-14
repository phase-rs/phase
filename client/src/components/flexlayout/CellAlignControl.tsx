import { useTranslation } from "react-i18next";

import {
  DEFAULT_CELL_ALIGN,
  usePreferencesStore,
  type CellAlign,
  type MiddleCell,
} from "../../stores/preferencesStore.ts";

/** The three alignment choices, with a glyph that reads as left / center / right
 *  and the settings i18n key for its accessible label. */
const ALIGNMENTS: ReadonlyArray<{ value: CellAlign; glyph: string; labelKey: string }> = [
  { value: "start", glyph: "⇤", labelKey: "flexLayout.align.left" },
  { value: "center", glyph: "↔", labelKey: "flexLayout.align.center" },
  { value: "end", glyph: "⇥", labelKey: "flexLayout.align.right" },
];

/**
 * Edit-mode segmented control to set a middle-row cell's content alignment
 * (left / center / right → flexbox `justify-*`). Rendered on the cell's top-right
 * corner. Stops `pointerdown` propagation so pressing it adjusts alignment
 * instead of starting the cell's reorder drag (the cell starts its drag from its
 * own `onPointerDown`, so a synthetic stop here is sufficient).
 */
export function CellAlignControl({ cell }: { cell: MiddleCell }) {
  const { t } = useTranslation("settings");
  const current = usePreferencesStore((s) => s.flexLayout.cellAlign?.[cell]) ?? DEFAULT_CELL_ALIGN[cell];
  const setFlexCellAlign = usePreferencesStore((s) => s.setFlexCellAlign);

  return (
    <div
      onPointerDown={(e) => e.stopPropagation()}
      className="pointer-events-auto absolute -top-2.5 right-1 z-20 flex items-center gap-0.5 rounded bg-slate-900/90 p-0.5 shadow ring-1 ring-white/15"
    >
      {ALIGNMENTS.map(({ value, glyph, labelKey }) => (
        <button
          key={value}
          type="button"
          aria-label={t(labelKey)}
          aria-pressed={current === value}
          onClick={() => setFlexCellAlign(cell, value)}
          className={`flex h-4 w-4 items-center justify-center rounded text-[11px] leading-none transition-colors ${
            current === value ? "bg-sky-400 text-slate-950" : "text-slate-300 hover:bg-white/10"
          }`}
        >
          <span aria-hidden>{glyph}</span>
        </button>
      ))}
    </div>
  );
}
