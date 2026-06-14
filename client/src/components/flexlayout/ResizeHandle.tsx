import { useRef } from "react";

import { usePreferencesStore, type FlexScaleKey } from "../../stores/preferencesStore.ts";

/** Pixels of diagonal drag per +1.0 of scale. Lower = more sensitive. */
const SENSITIVITY_PX = 180;

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

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    // Don't let the press also start the widget's drag.
    e.stopPropagation();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    start.current = {
      x: e.clientX,
      y: e.clientY,
      scale: usePreferencesStore.getState().flexLayout.scales?.[scaleKey] ?? 1,
    };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const s = start.current;
    if (!s) return;
    const dx = e.clientX - s.x;
    const dy = e.clientY - s.y;
    // Growing = dragging away from the widget: down-right for "br", down-left for "bl".
    const delta = (corner === "bl" ? -dx : dx) + dy;
    setFlexScale(scaleKey, s.scale + delta / SENSITIVITY_PX);
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    start.current = null;
    (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
  };

  const placement =
    corner === "bl" ? "bottom-0 left-0 cursor-nesw-resize" : "bottom-0 right-0 cursor-nwse-resize";

  return (
    <div
      role="slider"
      aria-label="Resize"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      className={`pointer-events-auto absolute z-30 flex h-5 w-5 touch-none items-center justify-center rounded bg-sky-400 text-slate-950 shadow-[0_0_8px_2px_rgba(56,189,248,0.6)] ${placement}`}
    >
      <span aria-hidden className="text-[11px] leading-none">⤡</span>
    </div>
  );
}
