import { useCallback, useEffect, useRef } from "react";

const MOVE_THRESHOLD = 10;

/** The default hold, in milliseconds, after which a press is a long press rather than a tap. */
export const LONG_PRESS_DELAY_MS = 500;

/**
 * Marks an element that handles its own taps, such as a tooltip trigger that
 * opens on a tap (`GameplayTooltip`'s `touch="toggle"`). A long press that
 * starts inside one leaves the pointer uncaptured (see `onPointerDown`).
 */
export const OWNS_TAP_ATTRIBUTE = "data-owns-tap";

interface UseLongPressOptions {
  delay?: number;
}

/**
 * Long-press hook for pointer devices. Returns pointer event handlers and a
 * `firedRef` so callers can suppress click events that follow a long press.
 *
 * Usage:
 *   const { handlers, firedRef } = useLongPress(() => inspect(id));
 *   <div {...handlers} onClick={() => { if (!firedRef.current) handleClick(); }} />
 */
export function useLongPress(
  callback: () => void,
  options?: UseLongPressOptions,
) {
  const { delay = LONG_PRESS_DELAY_MS } = options ?? {};
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const firedRef = useRef(false);
  const startPos = useRef<{ x: number; y: number } | null>(null);
  const pointerIdRef = useRef<number | null>(null);

  const clear = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    pointerIdRef.current = null;
  }, []);

  useEffect(() => clear, [clear]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!e.isPrimary || e.button !== 0) return;
      firedRef.current = false;
      pointerIdRef.current = e.pointerId;
      startPos.current = { x: e.clientX, y: e.clientY };
      // Capture keeps the rest of the press on this element, but it also sends
      // the press's `pointerup` and `click` here rather than to the element
      // under the finger. When the press starts inside a descendant that
      // handles its own taps, leave the pointer where it is so the tap reaches
      // it. A touch pointer is captured to the element under the finger
      // anyway, and its move, up and cancel events bubble here, so the long
      // press and its cancellation work the same.
      const tapOwner = e.target instanceof Element ? e.target.closest(`[${OWNS_TAP_ATTRIBUTE}]`) : null;
      const descendantOwnsTap =
        tapOwner !== null && tapOwner !== e.currentTarget && e.currentTarget.contains(tapOwner);
      if (!descendantOwnsTap) {
        try {
          e.currentTarget.setPointerCapture?.(e.pointerId);
        } catch {
          // Pointer capture is best-effort; long-press still works without it.
        }
      }
      timerRef.current = setTimeout(() => {
        firedRef.current = true;
        callback();
      }, delay);
    },
    [callback, delay],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (pointerIdRef.current !== e.pointerId) return;
      if (!startPos.current || !timerRef.current) return;
      const dx = e.clientX - startPos.current.x;
      const dy = e.clientY - startPos.current.y;
      if (dx * dx + dy * dy > MOVE_THRESHOLD * MOVE_THRESHOLD) {
        clear();
      }
    },
    [clear],
  );

  const onPointerUp = useCallback((e: React.PointerEvent) => {
    if (pointerIdRef.current !== e.pointerId) return;
    try {
      e.currentTarget.releasePointerCapture?.(e.pointerId);
    } catch {
      // Ignore capture-release mismatches from browsers/test harnesses.
    }
    clear();
  }, [clear]);

  const onPointerCancel = useCallback((e: React.PointerEvent) => {
    if (pointerIdRef.current !== e.pointerId) return;
    try {
      e.currentTarget.releasePointerCapture?.(e.pointerId);
    } catch {
      // Ignore capture-release mismatches from browsers/test harnesses.
    }
    clear();
    firedRef.current = false;
  }, [clear]);

  const onPointerLeave = useCallback((e: React.PointerEvent) => {
    if (pointerIdRef.current !== e.pointerId) return;
    clear();
  }, [clear]);

  // Prevent the native context menu after a long press, but allow desktop right-click.
  const onContextMenu = useCallback((e: React.MouseEvent) => {
    if (timerRef.current || firedRef.current) {
      e.preventDefault();
    }
  }, []);

  return {
    handlers: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel,
      onPointerLeave,
      onContextMenu,
    },
    firedRef,
  };
}
