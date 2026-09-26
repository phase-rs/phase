import { useCallback, useEffect, useRef } from "react";
import type { ThreeEvent } from "@react-three/fiber";

import { CARD_PREVIEW_LONG_PRESS_DELAY_MS } from "../../hooks/useLongPress.ts";

export const TABLETOP_CARD_HOLD_DELAY_MS = CARD_PREVIEW_LONG_PRESS_DELAY_MS;
export const TABLETOP_CARD_HOLD_MOVE_THRESHOLD_PX = 10;

interface TabletopCardHoldOptions {
  delayMs?: number;
  onHold: () => void;
}

interface PointerCaptureHandle {
  releasePointerCapture?: (pointerId: number) => void;
  setPointerCapture?: (pointerId: number) => void;
}

function captureHandle(
  event: ThreeEvent<PointerEvent>,
): PointerCaptureHandle | null {
  return event.target as PointerCaptureHandle | null;
}

function isPrimaryCardPress(event: ThreeEvent<PointerEvent>): boolean {
  // R3F normally copies PointerEvent fields onto ThreeEvent, but WebKit-backed
  // canvases and test hosts may expose them only on nativeEvent. Touch/pen
  // pointerdown is a primary press even when `button` is omitted; the strict
  // mouse check still prevents right-click from opening inspection.
  const nativeEvent = event.nativeEvent;
  const isPrimary = event.isPrimary ?? nativeEvent.isPrimary;
  const pointerType = event.pointerType || nativeEvent.pointerType;
  const button = event.button ?? nativeEvent.button;
  if (isPrimary === false) return false;
  if (pointerType === "touch" || pointerType === "pen") return true;
  return button === 0;
}

/**
 * Pointer gesture controller for cards rendered inside the React Three Fiber
 * canvas. A completed hold consumes the synthetic click that follows it, while
 * a short press remains available to the card's normal game interaction.
 */
export function useTabletopCardHold({
  delayMs = TABLETOP_CARD_HOLD_DELAY_MS,
  onHold,
}: TabletopCardHoldOptions) {
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pointerIdRef = useRef<number | null>(null);
  const startPositionRef = useRef<{ x: number; y: number } | null>(null);
  const heldRef = useRef(false);

  const clearTimer = useCallback(() => {
    if (timerRef.current != null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const resetPointer = useCallback(() => {
    clearTimer();
    pointerIdRef.current = null;
    startPositionRef.current = null;
  }, [clearTimer]);

  useEffect(() => resetPointer, [resetPointer]);

  const onPointerDown = useCallback(
    (event: ThreeEvent<PointerEvent>) => {
      if (!isPrimaryCardPress(event)) return;

      event.stopPropagation();
      resetPointer();
      heldRef.current = false;
      pointerIdRef.current = event.pointerId;
      startPositionRef.current = {
        x: event.clientX,
        y: event.clientY,
      };
      captureHandle(event)?.setPointerCapture?.(event.pointerId);
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        heldRef.current = true;
        onHold();
      }, delayMs);
    },
    [delayMs, onHold, resetPointer],
  );

  const onPointerMove = useCallback(
    (event: ThreeEvent<PointerEvent>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      const start = startPositionRef.current;
      if (!start || timerRef.current == null) return;

      const deltaX = event.clientX - start.x;
      const deltaY = event.clientY - start.y;
      const threshold = TABLETOP_CARD_HOLD_MOVE_THRESHOLD_PX;
      if (deltaX * deltaX + deltaY * deltaY > threshold * threshold) {
        captureHandle(event)?.releasePointerCapture?.(event.pointerId);
        resetPointer();
      }
    },
    [resetPointer],
  );

  const finishPointer = useCallback(
    (event: ThreeEvent<PointerEvent>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      captureHandle(event)?.releasePointerCapture?.(event.pointerId);
      resetPointer();
    },
    [resetPointer],
  );

  const cancelPointer = useCallback(
    (event: ThreeEvent<PointerEvent>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      captureHandle(event)?.releasePointerCapture?.(event.pointerId);
      resetPointer();
      heldRef.current = false;
    },
    [resetPointer],
  );

  const losePointerCapture = useCallback(
    (event: ThreeEvent<PointerEvent>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      // Releasing capture is also part of the successful pointer-up path. Keep
      // heldRef intact so the browser's subsequent click remains consumable.
      resetPointer();
    },
    [resetPointer],
  );

  const onContextMenu = useCallback((event: ThreeEvent<MouseEvent>) => {
    if (timerRef.current == null && !heldRef.current) return;
    event.stopPropagation();
    event.nativeEvent.preventDefault();
  }, []);

  const consumeClick = useCallback(() => {
    const held = heldRef.current;
    heldRef.current = false;
    return held;
  }, []);

  return {
    consumeClick,
    handlers: {
      onContextMenu,
      onLostPointerCapture: losePointerCapture,
      onPointerCancel: cancelPointer,
      onPointerDown,
      onPointerMove,
      onPointerUp: finishPointer,
    },
  };
}
