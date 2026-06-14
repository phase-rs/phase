import { useRef } from "react";

import { usePreferencesStore, type CappedTrack } from "../../stores/preferencesStore.ts";
import { resizeBand } from "./gridBandMath.ts";

interface ZoneSplitterProps {
  /** Which band this grabber resizes. "top" sits on the opponent/battlefield
   *  boundary (drag down to grow); "bottom" on the battlefield/player boundary
   *  (drag up to grow). */
  side: "top" | "bottom";
  /** Viewport Y (px) of the boundary this grabber straddles. */
  top: number;
}

/**
 * A thin horizontal grabber on a board row boundary. Dragging it resizes the
 * adjacent {@link CappedTrack} via {@link resizeBand}; the `1fr` middle row
 * absorbs the change. Positioned by {@link FlexEditOverlay} at the measured
 * boundary, so it never needs to know the grid geometry itself.
 */
export function ZoneSplitter({ side, top }: ZoneSplitterProps) {
  const setFlexBand = usePreferencesStore((s) => s.setFlexBand);
  const startRef = useRef<{ y: number; track: CappedTrack } | null>(null);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
    startRef.current = {
      y: e.clientY,
      track: usePreferencesStore.getState().flexLayout.gridBands[side],
    };
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const start = startRef.current;
    if (!start) return;
    const dragY = e.clientY - start.y;
    // Top boundary grows downward; bottom boundary grows upward.
    const deltaPx = side === "top" ? dragY : -dragY;
    setFlexBand(side, resizeBand(start.track, deltaPx, window.innerHeight));
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    startRef.current = null;
    (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
  };

  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      data-flex-splitter={side}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      className="fixed inset-x-0 z-[71] flex h-4 -translate-y-1/2 cursor-row-resize items-center justify-center"
      style={{ top }}
    >
      <span className="h-1 w-16 rounded-full bg-sky-400/80 shadow-[0_0_8px_2px_rgba(56,189,248,0.6)]" />
    </div>
  );
}
