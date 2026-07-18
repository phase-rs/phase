import { useEffect, useRef } from "react";

import { useUiStore } from "../stores/uiStore.ts";

/**
 * Safety mechanism to dismiss the card preview when the pointer is no longer
 * over an inspectable element. This handles the case where framer-motion
 * animations (tap rotation, attack slide, layout transitions) move elements
 * out from under the cursor without firing onMouseLeave.
 *
 * Uses `document.elementFromPoint()` on a 300ms interval to verify the pointer
 * is still over an element with `[data-card-hover]`.
 *
 * When `previewSticky` is true (set by long-press on touch devices), the
 * interval-based dismiss is skipped. A delayed capture-phase pointer listener
 * instead dismisses only a later interaction outside both a card and preview.
 */
export function usePreviewDismiss() {
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const previewSticky = useUiStore((s) => s.previewSticky);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const hoverObject = useUiStore((s) => s.hoverObject);
  const pointerRef = useRef({ x: 0, y: 0 });

  // Track pointer position (only while preview is active, mouse only)
  useEffect(() => {
    if (inspectedObjectId == null) return;

    function onMove(e: PointerEvent) {
      // Only track mouse pointer, not touch — touch uses sticky dismiss
      if (e.pointerType === "touch") return;
      pointerRef.current = { x: e.clientX, y: e.clientY };
    }

    document.addEventListener("pointermove", onMove, { passive: true });
    return () => document.removeEventListener("pointermove", onMove);
  }, [inspectedObjectId]);

  // Mouse: periodically verify the pointer is still over a card-hover element
  // Skipped when preview is sticky (touch-initiated)
  useEffect(() => {
    if (inspectedObjectId == null || previewSticky) return;

    let skipFirst = true;

    const id = setInterval(() => {
      if (skipFirst) {
        skipFirst = false;
        return;
      }
      const { x, y } = pointerRef.current;
      if (x === 0 && y === 0) return;

      const el = document.elementFromPoint(x, y);
      if (!el) return;

      const isOverCard = el.closest("[data-card-hover]") !== null;
      if (!isOverCard) {
        dismissPreview();
        hoverObject(null);
      }
    }, 300);

    return () => clearInterval(id);
  }, [inspectedObjectId, previewSticky, dismissPreview, hoverObject]);

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
