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
        // Long-press is an explicit-intent gesture (already a 400ms hold), so it
        // bypasses the configurable hover latency and shows the sticky preview now
        // — and setting it synchronously avoids orphaning previewSticky.
        inspectObject(objectId, undefined, "immediate");
        setPreviewSticky(true);
      }
    }, [inspectObject, setPreviewSticky, objectId]),
  );

  // Gate on the event's own `pointerType`, not on a `(any-hover: hover)` media
  // query: a touch-synthesized enter is the actual hazard (it fires the preview
  // on every tap, creating an un-dismissable loop), and `pointerType` reports it
  // per-event. Capability metadata lies on hosts that deliver honest pointer
  // events anyway — a remote-desktop session advertises no hover-capable input
  // while sending `pointerType: "mouse"`.
  //
  // Deliberately a denylist on "touch" rather than the allowlist idiom used by
  // useHorizontalScroll and the deck builder's hoverPreview: those fail closed
  // on an unrecognized pointerType, which is precisely the failure this fix
  // exists to remove. Chromium reports "" for an unknown pointer, and such a
  // device should hover rather than be silently locked out again.
  const onPointerEnter = useCallback((e: React.PointerEvent) => {
    if (e.pointerType === "touch") return;
    if (objectId != null) inspectObject(objectId);
  }, [inspectObject, objectId]);

  // `useLongPress` owns an `onPointerLeave` of its own, so this must compose
  // with it rather than replace it — otherwise the long-press timer never
  // cancels when the pointer slides off the card.
  const cancelLongPress = longPressHandlers.onPointerLeave;
  const onPointerLeave = useCallback((e: React.PointerEvent) => {
    cancelLongPress(e);
    // The touch guard matters most here: on a tablet the finger-lift fires a
    // leave, which would wipe the sticky preview the long-press just opened.
    if (e.pointerType === "touch") return;
    inspectObject(null);
  }, [cancelLongPress, inspectObject]);

  // `data-card-hover` is required for usePreviewDismiss's `:hover` poll —
  // without this attribute the 300ms dismiss loop clears the preview while the
  // cursor is still over the card. Injecting it here ensures every
  // useCardHover consumer is tagged by construction, so new callsites can't
  // silently regress the invariant by forgetting the manual annotation.
  //
  // The long-press spread comes FIRST so the composed handlers above
  // deterministically win the `onPointerLeave` key collision.
  return {
    handlers: isMobile
      ? { ...longPressHandlers, "data-card-hover": true }
      : { ...longPressHandlers, onPointerEnter, onPointerLeave, "data-card-hover": true },
    firedRef,
  };
}
