import { useCallback, useRef } from "react";
import type React from "react";

import { useUiStore } from "../stores/uiStore.ts";
import { useLongPress } from "./useLongPress.ts";
import type { ObjectId } from "../adapter/types.ts";

/**
 * Returns a hover-props factory for list-render sites where useCardHover cannot
 * be called per-item (cards rendered in a `.map()`).
 *
 * Both gestures are always wired, each gated on the event's own `pointerType`:
 * hover preview for anything that isn't a finger, long-press → sticky preview
 * for fingers. Gating per-event instead of on a device-capability media query
 * is what makes this work on hosts that misreport their input — a remote-desktop
 * session advertises no hover-capable input while still sending
 * `pointerType: "mouse"`. See useCardHover, which splits the same way.
 *
 * The touch-synthesized enter stays suppressed: it would open the
 * dismiss-looping MobilePreviewOverlay and block card selection.
 *
 * Because hooks can't run per-item, a single shared long-press timer serves the
 * whole list: only one pointer is active at a time, so each card's onPointerDown
 * records its id in pressedIdRef and the timer reads it when it fires. The click
 * a browser synthesizes after a long press is
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

  // `useLongPress` returns a fresh object literal every render, so capture the
  // two methods this composes with rather than depending on the object.
  const armLongPress = longPressHandlers.onPointerDown;
  const cancelLongPress = longPressHandlers.onPointerLeave;

  return useCallback(
    (id: ObjectId) => ({
      // Spread first so the composed handlers below deterministically win the
      // `onPointerDown` / `onPointerLeave` key collisions.
      ...longPressHandlers,
      // Long-press exists to stand in for hover, so arm it only for the pointer
      // that cannot hover. Arming it for a mouse would open a sticky preview on
      // any 500ms hold and then — via `onClickCapture` below — swallow the click
      // that ended it, which on a choice modal reads as a dead card. This is an
      // allowlist where the hover gates are denylists, and the two are
      // complementary: an unrecognized pointerType still hovers, and only a
      // known finger gets the long-press substitute.
      onPointerDown: (e: React.PointerEvent) => {
        if (e.pointerType !== "touch") return;
        pressedIdRef.current = id;
        armLongPress(e);
      },
      onPointerEnter: (e: React.PointerEvent) => {
        if (e.pointerType === "touch") return;
        inspectObject(id);
      },
      onPointerLeave: (e: React.PointerEvent) => {
        cancelLongPress(e);
        // On touch the finger-lift fires a leave, which would wipe the sticky
        // preview the long-press just opened.
        if (e.pointerType === "touch") return;
        inspectObject(null);
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
      // Required for usePreviewDismiss's elementFromPoint poll — without this
      // attribute the 300ms dismiss loop clears the preview while the cursor is
      // still over the card (choice modals, zone lists).
      "data-card-hover": true,
    }),
    [armLongPress, cancelLongPress, firedRef, inspectObject, longPressHandlers],
  );
}
