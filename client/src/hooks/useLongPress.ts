import { useCallback, useEffect, useRef } from "react";

const MOVE_THRESHOLD = 10;

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
  const { delay = 500 } = options ?? {};
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
      try {
        e.currentTarget.setPointerCapture?.(e.pointerId);
      } catch {
        // Pointer capture is best-effort; long-press still works without it.
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
