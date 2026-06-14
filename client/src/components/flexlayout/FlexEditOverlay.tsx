import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useUiStore } from "../../stores/uiStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { FLEX_PRESETS } from "./presets.ts";
import { ZoneSplitter } from "./ZoneSplitter.tsx";
import { ColumnSplitter } from "./ColumnSplitter.tsx";

/** Widget zones the overlay outlines + labels (the `data-flex-zone` values that
 *  map to a `settings:flexLayout.zones.*` key). The two board rows used only for
 *  splitter placement ("opp-row"/"player-row") are intentionally excluded. */
const LABELLED_ZONES = new Set([
  "playerHud",
  "opponentHud",
  "stackPanel",
  "logPanel",
  "actionRail",
  "playerPiles",
  "opponentPiles",
]);

interface ZoneRect {
  key: string;
  left: number;
  top: number;
  width: number;
  height: number;
}

/** The lands↔support boundary the vertical {@link ColumnSplitter} straddles:
 *  `x` is the current boundary, `left`/`right` the fixed outer edges of the
 *  two-column region (lands.left / support.right). Null when the viewer's middle
 *  row isn't measurable (no own-area columns rendered). */
interface ColumnBoundary {
  x: number;
  top: number;
  height: number;
  left: number;
  right: number;
}

interface Measured {
  topBoundary: number | null;
  bottomBoundary: number | null;
  columnBoundary: ColumnBoundary | null;
  zones: ZoneRect[];
}

const EMPTY: Measured = {
  topBoundary: null,
  bottomBoundary: null,
  columnBoundary: null,
  zones: [],
};

/** Read the live `data-flex-zone` element rects from the DOM. */
function measure(): Measured {
  const oppRow = document.querySelector('[data-flex-zone="opp-row"]');
  const playerRow = document.querySelector('[data-flex-zone="player-row"]');
  const landsCol = document.querySelector('[data-flex-zone="lands-col"]');
  const supportCol = document.querySelector('[data-flex-zone="support-col"]');
  const zones: ZoneRect[] = [];
  for (const el of document.querySelectorAll<HTMLElement>("[data-flex-zone]")) {
    const key = el.dataset.flexZone ?? "";
    if (!LABELLED_ZONES.has(key)) continue;
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue; // not currently rendered
    zones.push({ key, left: r.left, top: r.top, width: r.width, height: r.height });
  }
  let columnBoundary: ColumnBoundary | null = null;
  if (landsCol && supportCol) {
    const l = landsCol.getBoundingClientRect();
    const s = supportCol.getBoundingClientRect();
    if (l.width > 0 && s.width > 0) {
      columnBoundary = {
        x: (l.right + s.left) / 2, // midpoint of the gap between the columns
        top: l.top,
        height: l.height,
        left: l.left,
        right: s.right,
      };
    }
  }
  return {
    topBoundary: oppRow ? oppRow.getBoundingClientRect().bottom : null,
    bottomBoundary: playerRow ? playerRow.getBoundingClientRect().top : null,
    columnBoundary,
    zones,
  };
}

/** The interactive Flex Layout editing surface. Mounted only while
 *  `flexEditMode` is on. The root is `pointer-events-none` so it never blocks
 *  the widget drags it enables — only the toolbar and the splitters capture
 *  pointers. Sits at the top of the stack (z-[70]+) above the game log (z-[60])
 *  and hovered cards (z-60) so it can annotate every target. */
function FlexEditOverlayInner() {
  const { t } = useTranslation("settings");
  const setFlexEditMode = useUiStore((s) => s.setFlexEditMode);
  const applyFlexPreset = usePreferencesStore((s) => s.applyFlexPreset);
  const resetFlexLayout = usePreferencesStore((s) => s.resetFlexLayout);
  const activePreset = usePreferencesStore((s) => s.flexLayout.activePreset);
  const setFlexScale = usePreferencesStore((s) => s.setFlexScale);
  const stackScale = usePreferencesStore((s) => s.flexLayout.scales?.stack) ?? 1;
  const summaryScale = usePreferencesStore((s) => s.flexLayout.scales?.summaryTile) ?? 1;

  const [m, setM] = useState<Measured>(EMPTY);

  // Re-measure on every animation frame while editing so the outlines/labels
  // and splitter positions track live: widget drags (which mutate the DOM but
  // not the store until release), splitter drags, and window resizes. The key
  // guard re-renders only when the measured geometry actually changes, so idle
  // frames are a cheap measure + compare with no React work.
  useEffect(() => {
    let raf = 0;
    let prevKey = "";
    const loop = () => {
      const next = measure();
      const key = JSON.stringify(next);
      if (key !== prevKey) {
        prevKey = key;
        setM(next);
      }
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);

  return (
    <>
      {/* Decorative chrome — never captures pointers. */}
      <div className="pointer-events-none fixed inset-0 z-[70]">
        {m.zones.map((z) => (
          <div
            key={z.key}
            className="absolute rounded-lg ring-2 ring-sky-400/70 shadow-[0_0_12px_rgba(56,189,248,0.35)]"
            style={{ left: z.left, top: z.top, width: z.width, height: z.height }}
          >
            <span className="absolute left-0 top-0 -translate-y-full rounded-t-md bg-sky-400 px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wide text-slate-950">
              {t(`flexLayout.zones.${z.key}`)}
            </span>
          </div>
        ))}
      </div>

      {/* Zone-resize grabbers (own their own pointer capture). */}
      {m.topBoundary != null && <ZoneSplitter side="top" top={m.topBoundary} />}
      {m.bottomBoundary != null && <ZoneSplitter side="bottom" top={m.bottomBoundary} />}
      {m.columnBoundary != null && (
        <ColumnSplitter
          x={m.columnBoundary.x}
          top={m.columnBoundary.top}
          height={m.columnBoundary.height}
          left={m.columnBoundary.left}
          right={m.columnBoundary.right}
        />
      )}

      {/* Toolbar. */}
      <div className="pointer-events-auto fixed left-1/2 top-3 z-[72] flex -translate-x-1/2 flex-col items-center gap-1.5 rounded-xl border border-sky-400/40 bg-slate-950/90 px-3 py-2 shadow-xl backdrop-blur">
        <div className="flex items-center gap-2">
          <span className="text-xs font-semibold uppercase tracking-wider text-sky-300">
            {t("flexLayout.title")}
          </span>
          <div className="flex items-center gap-1">
            {FLEX_PRESETS.map((preset) => (
              <button
                key={preset.id}
                type="button"
                onClick={() => applyFlexPreset(preset.config)}
                title={t(preset.descriptionKey)}
                className={`rounded-md px-2 py-1 text-xs font-medium transition-colors ${
                  activePreset === preset.id
                    ? "bg-sky-400 text-slate-950"
                    : "bg-slate-800 text-slate-200 hover:bg-slate-700"
                }`}
              >
                {t(preset.labelKey)}
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={() => resetFlexLayout()}
            className="rounded-md bg-slate-800 px-2 py-1 text-xs font-medium text-slate-200 hover:bg-slate-700"
          >
            {t("flexLayout.reset")}
          </button>
          <button
            type="button"
            onClick={() => setFlexEditMode(false)}
            className="rounded-md bg-emerald-500 px-3 py-1 text-xs font-bold text-slate-950 hover:bg-emerald-400"
          >
            {t("flexLayout.done")}
          </button>
        </div>
        <div className="flex items-center gap-3">
          <ScaleStepper
            label={t("flexLayout.scale.stack")}
            value={stackScale}
            onChange={(v) => setFlexScale("stack", v)}
          />
          <ScaleStepper
            label={t("flexLayout.scale.tiles")}
            value={summaryScale}
            onChange={(v) => setFlexScale("summaryTile", v)}
          />
        </div>
        <span className="text-[11px] text-slate-400">{t("flexLayout.hint")}</span>
      </div>
    </>
  );
}

/** Step size for the scale steppers — one tenth, matching the store's clamp
 *  granularity (0.5–2.0). */
const SCALE_STEP = 0.1;

/** A compact −/value/+ control for an aspect-preserving zone scale. The store
 *  clamps the committed value, so the buttons can overshoot freely. */
function ScaleStepper({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (next: number) => void;
}) {
  const { t } = useTranslation("settings");
  return (
    <span className="flex items-center gap-1">
      <span className="text-[11px] text-slate-300">{label}</span>
      <button
        type="button"
        onClick={() => onChange(value - SCALE_STEP)}
        aria-label={t("flexLayout.scale.smaller", { label })}
        className="flex h-5 w-5 items-center justify-center rounded bg-slate-800 text-sm font-bold text-slate-200 hover:bg-slate-700"
      >
        −
      </button>
      <span className="w-9 text-center text-[11px] tabular-nums text-slate-200">
        {Math.round(value * 100)}%
      </span>
      <button
        type="button"
        onClick={() => onChange(value + SCALE_STEP)}
        aria-label={t("flexLayout.scale.larger", { label })}
        className="flex h-5 w-5 items-center justify-center rounded bg-slate-800 text-sm font-bold text-slate-200 hover:bg-slate-700"
      >
        +
      </button>
    </span>
  );
}

/** Gate wrapper: render nothing (and run no measurement effects) outside edit
 *  mode. Split so the hooks in {@link FlexEditOverlayInner} never run when the
 *  feature is off. */
export function FlexEditOverlay() {
  const flexEditMode = useUiStore((s) => s.flexEditMode);
  if (!flexEditMode) return null;
  return <FlexEditOverlayInner />;
}
