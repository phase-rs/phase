import { useCallback } from "react";

import { useLongPress } from "./useLongPress.ts";
import { useIsMobile } from "./useIsMobile.ts";
import { useUiStore } from "../stores/uiStore.ts";

/**
 * Combined mouse hover + touch long-press handlers for card preview.
 *
 * Spread `handlers` onto the card element and use `firedRef` to suppress
 * click events that follow a long press.
 *
 * Usage:
 *   const { handlers, firedRef } = useCardHover(objectId);
 *   <div {...handlers} onClick={() => { if (!firedRef.current) doClick(); }} />
 */
export function useCardHover(objectId: number | null) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const isMobile = useIsMobile();

  const { handlers: longPressHandlers, firedRef } = useLongPress(
    useCallback(() => {
      if (objectId != null) {
        inspectObject(objectId);
        setPreviewSticky(true);
      }
    }, [inspectObject, setPreviewSticky, objectId]),
  );

  const onMouseEnter = useCallback(() => {
    if (objectId != null) inspectObject(objectId);
  }, [inspectObject, objectId]);

  const onMouseLeave = useCallback(() => {
    inspectObject(null);
  }, [inspectObject]);

  // On mobile, skip mouse events — synthesized mouseenter from touch fires
  // the preview every time the user touches a card, creating an
  // un-dismissable loop. Long-press is the only mobile preview trigger.
  //
  // `data-card-hover` is required for usePreviewDismiss's elementFromPoint
  // poll — without this attribute the 300ms dismiss loop clears the preview
  // while the cursor is still over the card. Injecting it here ensures every
  // useCardHover consumer is tagged by construction, so new callsites can't
  // silently regress the invariant by forgetting the manual annotation.
  return {
    handlers: isMobile
      ? { ...longPressHandlers, "data-card-hover": true }
      : { onMouseEnter, onMouseLeave, ...longPressHandlers, "data-card-hover": true },
    firedRef,
  };
}
