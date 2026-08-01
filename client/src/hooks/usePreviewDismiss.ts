import { useEffect } from "react";

import { useUiStore } from "../stores/uiStore.ts";

/**
 * Safety mechanism to dismiss the card preview when the pointer is no longer
 * over an inspectable element. This handles the case where framer-motion
 * animations (tap rotation, attack slide, layout transitions) move elements
 * out from under the cursor without firing onMouseLeave.
 *
 * On a 300ms interval it verifies, via the browser's own `:hover` state, that
 * the pointer is still over an element with `[data-card-hover]` — robust to the
 * stale JS-tracked pointer that sparse remote-desktop pointer streams produce.
 *
 * When `previewSticky` is true (set by long-press on touch devices), the
 * interval-based dismiss is skipped. A delayed capture-phase pointer listener
 * instead dismisses only a later interaction outside both a card and preview.
 */
export function usePreviewDismiss() {
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const previewSticky = useUiStore((s) => s.previewSticky);
  const altHeld = useUiStore((s) => s.altHeld);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const hoverObject = useUiStore((s) => s.hoverObject);

  // Mouse: periodically verify the pointer is still over a card-hover element.
  // Skipped when the preview is sticky (touch-initiated) or Alt-pinned (frozen
  // for reading): a pinned preview must not vanish while the user moves off the
  // card to click "Report a Problem" or scroll rulings. Alt-off or a click
  // outside (the pointerdown-outside effect below) still dismisses it.
  useEffect(() => {
    if (inspectedObjectId == null || previewSticky || altHeld) return;

    let skipFirst = true;

    const id = setInterval(() => {
      if (skipFirst) {
        skipFirst = false;
        return;
      }
      // Ask the browser for the element it currently considers hovered rather
      // than sampling elementFromPoint() against a JS-tracked pointer. That
      // tracked coordinate is only as fresh as the last `pointermove`, and over
      // sparse/coalesced pointer streams (remote-desktop / RDP webviews) it goes
      // stale a few px off the card while the OS cursor is genuinely still on
      // it — so a single elementFromPoint sample spuriously dismissed the
      // just-shown preview. `:hover` is driven by the engine's own continuous
      // hit-test and stays correct with no pointermove event at all. It matches
      // the interactive info/parse panel too (also `[data-card-hover]`), and
      // pointer-events-none surfaces (the preview image) never match it — the
      // same "over a card or its info panel" test the old sample performed.
      const stillOverTarget = document.querySelector("[data-card-hover]:hover");
      if (stillOverTarget == null) {
        dismissPreview();
        hoverObject(null);
      }
    }, 300);

    return () => clearInterval(id);
  }, [inspectedObjectId, previewSticky, altHeld, dismissPreview, hoverObject]);

  // Any later pointer interaction outside a card or the preview dismisses it.
  // Registering after the current event lets the long-press/click that opened
  // the preview complete first. Capture phase keeps this working for controls
  // that stop bubbling, and `elementFromPoint` sees through the desktop
  // preview's pointer-events-none surface.
  useEffect(() => {
    if (inspectedObjectId == null) return;

    function isOverPreview(x: number, y: number): boolean {
      return Array.from(document.querySelectorAll<HTMLElement>("[data-card-preview]")).some((preview) => {
        const rect = preview.getBoundingClientRect();
        return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
      });
    }

    function onPointerDown(event: PointerEvent) {
      const el = document.elementFromPoint(event.clientX, event.clientY);
      const isOverCard = el?.closest("[data-card-hover]") != null;
      if (!isOverCard && !isOverPreview(event.clientX, event.clientY)) {
        dismissPreview();
        hoverObject(null);
      }
    }

    // Defer one event turn so the pointerdown that caused the current preview
    // is never reinterpreted as an outside dismissal.
    const timer = setTimeout(() => {
      document.addEventListener("pointerdown", onPointerDown, true);
    }, 0);

    return () => {
      clearTimeout(timer);
      document.removeEventListener("pointerdown", onPointerDown, true);
    };
  }, [inspectedObjectId, dismissPreview, hoverObject]);
}
