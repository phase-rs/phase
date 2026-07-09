import { useEffect, useRef } from "react";

/**
 * Makes an overflowing horizontal strip easy to navigate:
 *   - vertical mousewheel scrolls horizontally
 *   - mouse click-and-drag scrolls horizontally (Figma / Miro style)
 *   - native touch swipe is untouched (browser momentum keeps working)
 *
 * Drag is suppressed when the pointer started on an interactive child (button,
 * anchor, input) so we don't fight existing handlers. A click that follows a
 * drag is blocked in the capture phase so cards aren't accidentally selected
 * while panning.
 *
 * Pass `{ drag: false }` to keep only the wheel behavior — useful when the
 * children own their own pointer-drag gesture (e.g. framer-motion Reorder) and
 * a scroll-pan drag would conflict with it.
 */
export function useHorizontalScroll<T extends HTMLElement>({
  drag = true,
}: { drag?: boolean } = {}) {
  const ref = useRef<T | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const isOverflowing = () => el.scrollWidth > el.clientWidth + 1;

    const onWheel = (e: WheelEvent) => {
      if (!isOverflowing()) return;
      // Only convert when the user's intent is clearly vertical; preserve
      // horizontal trackpad gestures that already carry deltaX.
      if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
      e.preventDefault();
      el.scrollLeft += e.deltaY;
    };

    let dragging = false;
    let pointerId: number | null = null;
    let startX = 0;
    let startScroll = 0;
    let moved = false;
    const DRAG_THRESHOLD_PX = 5;

    const onPointerDown = (e: PointerEvent) => {
      if (e.pointerType !== "mouse") return; // let touch/pen use native scroll
      if (e.button !== 0) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest("button, a, input, textarea, select, [role='button']")) return;
      if (!isOverflowing()) return;
      dragging = true;
      moved = false;
      pointerId = e.pointerId;
      startX = e.clientX;
      startScroll = el.scrollLeft;
    };

    const onPointerMove = (e: PointerEvent) => {
      if (!dragging || e.pointerId !== pointerId) return;
      const dx = e.clientX - startX;
      if (!moved && Math.abs(dx) > DRAG_THRESHOLD_PX) {
        moved = true;
        try {
          el.setPointerCapture(e.pointerId);
        } catch {
          // Pointer capture can fail if the element lost focus — safe to ignore.
        }
        el.style.cursor = "grabbing";
        el.style.userSelect = "none";
      }
      if (moved) {
        e.preventDefault();
        el.scrollLeft = startScroll - dx;
      }
    };

    const endDrag = (e: PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      dragging = false;
      pointerId = null;
      el.style.cursor = "";
      el.style.userSelect = "";
      try {
        el.releasePointerCapture(e.pointerId);
      } catch {
        // Already released; no-op.
      }
    };

    // Suppress the click that would otherwise follow a drag-release.
    const onClickCapture = (e: MouseEvent) => {
      if (moved) {
        e.stopPropagation();
        e.preventDefault();
        moved = false;
      }
    };

    el.addEventListener("wheel", onWheel, { passive: false });
    if (drag) {
      el.addEventListener("pointerdown", onPointerDown);
      el.addEventListener("pointermove", onPointerMove);
      el.addEventListener("pointerup", endDrag);
      el.addEventListener("pointercancel", endDrag);
      el.addEventListener("click", onClickCapture, { capture: true });
    }

    return () => {
      el.removeEventListener("wheel", onWheel);
      if (drag) {
        el.removeEventListener("pointerdown", onPointerDown);
        el.removeEventListener("pointermove", onPointerMove);
        el.removeEventListener("pointerup", endDrag);
        el.removeEventListener("pointercancel", endDrag);
        el.removeEventListener("click", onClickCapture, { capture: true });
      }
      el.style.cursor = "";
      el.style.userSelect = "";
    };
  }, [drag]);

  return ref;
}
