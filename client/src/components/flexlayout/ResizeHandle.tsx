import { useRef, useState } from "react";

import { usePreferencesStore, type FlexScaleKey } from "../../stores/preferencesStore.ts";

/** Pixels of diagonal drag per +1.0 of scale. Lower = more sensitive. */
const SENSITIVITY_PX = 180;
/** Within this of 1.0, the scale magnetically snaps back to the default size. */
const SNAP_SCALE = 0.06;

/**
 * A corner resize grip that scales a widget by dragging — direct manipulation in
 * place of an abstract stepper. It only nudges the stored `scales[scaleKey]`
 * multiplier (the store clamps it); each consumer decides how that multiplier is
 * applied (a box `transform`, the stack's card size, the summary-pill size), so
 * one handle serves them all. Delta-based, so it needs no host measurement.
 */
export function ResizeHandle({
  scaleKey,
  corner = "br",
}: {
  scaleKey: FlexScaleKey;
  corner?: "br" | "bl";
}) {
  const setFlexScale = usePreferencesStore((s) => s.setFlexScale);
  const start = useRef<{ x: number; y: number; scale: number } | null>(null);
  // Live scale shown in a readout badge while dragging (null = not dragging).
  const [liveScale, setLiveScale] = useState<number | null>(null);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    // Don't let the press also start the widget's drag.
    e.stopPropagation();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    const scale = usePreferencesStore.getState().flexLayout.scales?.[scaleKey] ?? 1;
    start.current = { x: e.clientX, y: e.clientY, scale };
    setLiveScale(scale);
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const s = start.current;
    if (!s) return;
    const dx = e.clientX - s.x;
    const dy = e.clientY - s.y;
    // Growing = dragging away from the widget: down-right for "br", down-left for "bl".
    const delta = (corner === "bl" ? -dx : dx) + dy;
    // Magnetic snap to the default size when close to it.
    const raw = s.scale + delta / SENSITIVITY_PX;
    const next = Math.abs(raw - 1) < SNAP_SCALE ? 1 : raw;
    setFlexScale(scaleKey, next);
    setLiveScale(usePreferencesStore.getState().flexLayout.scales?.[scaleKey] ?? 1);
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    start.current = null;
    setLiveScale(null);
    (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
  };

  // Overhang the corner so the (oversized, finger-friendly) hit area clears the
  // tile body. z-50 keeps it above sibling tile content so it's always grabbable.
  const placement =
    corner === "bl"
      ? "-bottom-2 -left-2 cursor-nesw-resize"
      : "-bottom-2 -right-2 cursor-nwse-resize";

  return (
    // Large TRANSPARENT hit target for touch (~36px); the visible grip is the
    // smaller inner span. Decoupling the two keeps the grip compact while the
    // finger target stays comfortable on mobile.
    <div
      role="slider"
      aria-label="Resize"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      className={`pointer-events-auto absolute z-50 flex h-9 w-9 touch-none items-center justify-center ${placement}`}
    >
      {liveScale != null && (
        // Live size readout (i18n-free percentage); turns green exactly at the
        // default size (100%) — the "you're home" signal, no text to localize.
        <span
          className={`pointer-events-none absolute bottom-full mb-0.5 whitespace-nowrap rounded px-1.5 py-0.5 text-[10px] font-black tabular-nums shadow ${
            liveScale === 1 ? "bg-emerald-400 text-emerald-950" : "bg-slate-900/95 text-sky-200 ring-1 ring-sky-400/40"
          } ${corner === "bl" ? "left-0" : "right-0"}`}
        >
          {`${Math.round(liveScale * 100)}%`}
        </span>
      )}
      <span
        aria-hidden
        className="flex h-5 w-5 items-center justify-center rounded bg-sky-400 text-[11px] leading-none text-slate-950 shadow-[0_0_8px_2px_rgba(56,189,248,0.6)]"
      >
        ⤡
      </span>
    </div>
  );
}
