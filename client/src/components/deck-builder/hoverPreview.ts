import type { PointerEvent } from "react";

/**
 * Hover-to-preview handlers for the deck-builder card surfaces (list, stack,
 * search grid).
 *
 * Two subtleties, both observed in the wild:
 *
 * 1. The preview is shown on `pointerenter`/cleared on `pointerleave` for mouse
 *    pointers only — touch drives the preview via the explicit tap (list/stack
 *    `onClick`) or long-press (grid), and the overlay's own `pointerdown`
 *    dismisses it.
 *
 * 2. On narrow widths / mobile the preview renders as a *full-screen overlay*
 *    that sits on top of the card. The instant it opens, the pointer "leaves"
 *    the card onto the overlay and the browser fires `pointerleave` on the card
 *    (with `pointerType: "mouse"` — even for the touch-compat pointer). If that
 *    cleared the preview it would close the overlay the same gesture just opened
 *    — the "disappears immediately after clicking" bug. So we ignore a leave
 *    whose `relatedTarget` is the preview overlay itself. (Desktop is naturally
 *    immune: its preview is `pointer-events-none` and never the relatedTarget.)
 */
export function mouseHoverPreview(
  onCardHover: ((name: string | null) => void) | undefined,
  name: string,
) {
  return {
    onPointerEnter: (e: PointerEvent) => {
      if (e.pointerType === "mouse") onCardHover?.(name);
    },
    onPointerLeave: (e: PointerEvent) => {
      if (e.pointerType !== "mouse") return;
      const related = e.relatedTarget;
      if (related instanceof Element && related.closest("[data-card-preview]")) return;
      onCardHover?.(null);
    },
  };
}
