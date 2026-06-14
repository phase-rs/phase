import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { ratioFromPointerX } from "./gridBandMath.ts";

interface ColumnSplitterProps {
  /** Viewport X (px) of the lands↔support boundary this grabber straddles. */
  x: number;
  /** Viewport Y (px) of the top of the middle row the boundary spans. */
  top: number;
  /** Height (px) of the middle row, so the grabber matches the columns' extent. */
  height: number;
  /** Left/right viewport bounds of the combined two-column region — the fixed
   *  outer edges the boundary slides between (lands.left / support.right). */
  left: number;
  right: number;
}

/**
 * A thin vertical grabber on the lands↔support boundary. Dragging it sets the
 * global `landSupportRatio` (lands' share of the row) from the pointer's
 * absolute X via {@link ratioFromPointerX}; the support column takes the
 * remainder. Positioned by {@link FlexEditOverlay} at the measured boundary, so
 * it never needs to know the column geometry itself. The store clamps the ratio
 * so neither column can starve.
 */
export function ColumnSplitter({ x, top, height, left, right }: ColumnSplitterProps) {
  const setFlexLandSupportRatio = usePreferencesStore((s) => s.setFlexLandSupportRatio);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    // Only while a pointer is captured (a button is held during the drag).
    if (e.buttons === 0) return;
    setFlexLandSupportRatio(ratioFromPointerX(e.clientX, left, right));
  };

  const handlePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      data-flex-splitter="lands-support"
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={handlePointerUp}
      className="fixed z-[71] flex w-4 -translate-x-1/2 cursor-col-resize items-center justify-center"
      style={{ left: x, top, height }}
    >
      <span className="h-16 w-1 rounded-full bg-sky-400/80 shadow-[0_0_8px_2px_rgba(56,189,248,0.6)]" />
    </div>
  );
}
