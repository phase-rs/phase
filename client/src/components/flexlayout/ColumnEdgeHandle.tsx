import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { ratioFromPointerX } from "./gridBandMath.ts";

/**
 * A resize grip on the shared edge between the adjacent lands & support cells.
 * Unlike the old floating divider, it rides the cell (rendered inside the left
 * cell of the pair), so it travels with reordering and never inverts: it reads
 * the live `lands-col`/`support-col` rects at drag time and maps the pointer to
 * lands' share, flipping when support is the left cell. The store clamps.
 */
export function ColumnEdgeHandle() {
  const setFlexLandSupportRatio = usePreferencesStore((s) => s.setFlexLandSupportRatio);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    // Don't let the press start the cell's reorder drag.
    e.stopPropagation();
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  };

  const handlePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.buttons === 0) return;
    const lands = document.querySelector('[data-flex-zone="lands-col"]')?.getBoundingClientRect();
    const support = document.querySelector('[data-flex-zone="support-col"]')?.getBoundingClientRect();
    if (!lands || !support) return;
    const left = Math.min(lands.left, support.left);
    const right = Math.max(lands.right, support.right);
    // ratioFromPointerX is "fraction from the left edge"; that's lands' share
    // only when lands is the left cell — otherwise it's support's share.
    let landsShare = ratioFromPointerX(e.clientX, left, right);
    if (lands.left > support.left) landsShare = 1 - landsShare;
    setFlexLandSupportRatio(landsShare);
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
      // Straddles the cell's right edge so it sits on the lands↔support seam.
      className="absolute -right-2 top-0 bottom-0 z-30 flex w-4 cursor-col-resize touch-none items-center justify-center"
    >
      <span className="h-12 w-1 rounded-full bg-sky-300 shadow-[0_0_8px_2px_rgba(56,189,248,0.7)]" />
    </div>
  );
}
