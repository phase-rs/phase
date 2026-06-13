import { useCallback, useRef } from "react";
import type React from "react";

import { useUiStore } from "../stores/uiStore.ts";
import { useCanHover } from "./useCanHover.ts";
import { useIsMobile } from "./useIsMobile.ts";
import { useLongPress } from "./useLongPress.ts";
import type { ObjectId } from "../adapter/types.ts";

/**
 * Returns a hover-props factory for list-render sites where useCardHover cannot
 * be called per-item (cards rendered in a `.map()`).
 *
 * Desktop: onMouseEnter/onMouseLeave drive inspectObject for hover preview.
 *
 * Touch: the synthesized mouseenter is skipped (it would open the dismiss-looping
 * MobilePreviewOverlay and block card selection), and long-press → sticky preview
 * is wired instead, matching useCardHover on the board so modal/zone card lists
 * get the same gesture. Because hooks can't run per-item, a single shared
 * long-press timer serves the whole list: only one pointer is active at a time,
 * so each card's onPointerDown records its id in pressedIdRef and the timer reads
 * it when it fires. The click a browser synthesizes after a long press is
 * swallowed in the CAPTURE phase (which runs before the caller's bubble-phase
 * onClick), so the preview gesture never also toggles selection — callers keep a
 * plain onClick and need no firedRef plumbing.
 *
 *   const hoverProps = useInspectHoverProps();
 *   <button {...hoverProps(id)} onClick={() => select(id)} />
 *
 * For per-card components (where useCardHover is callable), prefer useCardHover.
 */
export function useInspectHoverProps() {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const isMobile = useIsMobile();
  const canHover = useCanHover();

  // Which card most recently began a press — lets the single shared long-press
  // timer resolve the correct id on fire (one active pointer at a time).
  const pressedIdRef = useRef<ObjectId | null>(null);
  const { handlers: longPressHandlers, firedRef } = useLongPress(
    useCallback(() => {
      if (pressedIdRef.current != null) {
        // Long-press is explicit intent (a hold past the timer), so bypass hover
        // latency and show the sticky preview immediately, mirroring useCardHover.
        inspectObject(pressedIdRef.current, undefined, "immediate");
        setPreviewSticky(true);
      }
    }, [inspectObject, setPreviewSticky]),
  );

  return useCallback(
    (id: ObjectId) => {
      // Touch-only devices: skip mouse handlers (synthesized mouseenter loops the
      // preview) and wire long-press → preview instead.
      if (isMobile || !canHover) {
        return {
          ...longPressHandlers,
          onPointerDown: (e: React.PointerEvent) => {
            pressedIdRef.current = id;
            longPressHandlers.onPointerDown(e);
          },
          // Capture phase runs before the caller's bubble-phase onClick, so a
          // stopPropagation here swallows the post-long-press click without the
          // caller needing to guard onClick with firedRef.
          onClickCapture: (e: React.MouseEvent) => {
            if (firedRef.current) {
              e.stopPropagation();
              firedRef.current = false;
            }
          },
          // Required for usePreviewDismiss's elementFromPoint poll.
          "data-card-hover": true,
        };
      }
      return {
        onMouseEnter: () => inspectObject(id),
        onMouseLeave: () => inspectObject(null),
        // Required for usePreviewDismiss's elementFromPoint poll — without this
        // attribute the 300ms dismiss loop clears the preview while the cursor is
        // still over the card (choice modals, zone lists).
        "data-card-hover": true,
      };
    },
    [isMobile, canHover, inspectObject, longPressHandlers, firedRef],
  );
}
