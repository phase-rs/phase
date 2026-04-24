import { useCallback } from "react";

import { useUiStore } from "../stores/uiStore.ts";
import { useIsMobile } from "./useIsMobile.ts";
import type { ObjectId } from "../adapter/types.ts";

/**
 * Returns a hover-props factory that skips onMouseEnter/onMouseLeave on mobile.
 *
 * On touch devices, a tap synthesizes mouseenter which would call inspectObject
 * and open the full-screen MobilePreviewOverlay (CardPreview.tsx). The overlay's
 * onPointerDown={dismissPreview} then intercepts the next tap, blocking card
 * selection, targeting, and play actions.
 *
 * Use this hook in list-render sites where useCardHover cannot be called
 * per-item. Callers spread the returned props onto their interactive element:
 *
 *   const hoverProps = useInspectHoverProps();
 *   <button {...hoverProps(id)} onClick={…} />
 *
 * For per-card components (where useCardHover is callable), prefer useCardHover
 * — it also provides long-press preview on mobile.
 */
export function useInspectHoverProps() {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const isMobile = useIsMobile();
  return useCallback(
    (id: ObjectId) =>
      isMobile
        ? undefined
        : {
            onMouseEnter: () => inspectObject(id),
            onMouseLeave: () => inspectObject(null),
            // Required for usePreviewDismiss's elementFromPoint poll — without
            // this attribute the 300ms dismiss loop clears the preview while
            // the cursor is still over the card (choice modals, zone lists).
            "data-card-hover": true,
          },
    [isMobile, inspectObject],
  );
}
