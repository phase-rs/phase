import { useCallback, useEffect, useRef } from "react";
import type { PointerEvent as ReactPointerEvent, RefObject } from "react";

import { HAND_DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";
import { useUiStore } from "../../stores/uiStore.ts";

const HOLD_DELAY_MS = 400;
const PRE_HOLD_MOVE_THRESHOLD_PX = 12;
// A held finger naturally drifts a few CSS pixels. Require a deliberate lift
// before replacing the large inspection preview with the movable hand card.
const HELD_CARD_DRAG_START_PX = 16;
const VERTICAL_SCRUB_TOLERANCE_PX = 28;
const RELEASE_CLICK_SUPPRESSION_MS = 750;

function cardAtPoint(container: HTMLElement, x: number, y: number): HTMLElement | null {
  const candidates = Array.from(
    container.querySelectorAll<HTMLElement>("[data-hand-card][data-object-id]"),
  )
    .map((element) => ({ element, rect: element.getBoundingClientRect() }))
    .filter(({ rect }) =>
      x >= rect.left
      && x <= rect.right
      && y >= rect.top - VERTICAL_SCRUB_TOLERANCE_PX
      && y <= rect.bottom + VERTICAL_SCRUB_TOLERANCE_PX
    );

  if (candidates.length === 0) return null;

  candidates.sort((a, b) => {
    const aDistance = Math.abs(x - (a.rect.left + a.rect.right) / 2);
    const bDistance = Math.abs(x - (b.rect.left + b.rect.right) / 2);
    return aDistance - bDistance;
  });
  return candidates[0].element;
}

/**
 * Mobile hand interaction matching Arena's gesture split:
 *
 * - a short tap remains available to open the full hand drawer;
 * - holding activates a non-blocking preview;
 * - horizontal movement while held scrubs across adjacent fanned cards;
 * - dragging a directly castable card above the hand arms release-to-cast;
 * - release elsewhere dismisses the preview without casting.
 *
 * The fan cards stay pointer-events-none on mobile, so this hook owns one stable
 * pointer-captured surface instead of moving the gesture target as cards animate.
 */
export function useHandScrubPreview(
  containerRef: RefObject<HTMLElement | null>,
  enabled: boolean,
  options: {
    isPlayable?: (objectId: number) => boolean;
    canReleaseToCast?: (objectId: number) => boolean;
    onReleaseToCast?: (objectId: number) => void;
  } = {},
) {
  const { isPlayable, canReleaseToCast, onReleaseToCast } = options;
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const dismissPreview = useUiStore((s) => s.dismissPreview);
  const setMobileHandGesture = useUiStore((s) => s.setMobileHandGesture);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pointerIdRef = useRef<number | null>(null);
  const startRef = useRef({ x: 0, y: 0 });
  const scrubbingRef = useRef(false);
  const activeCardRef = useRef<HTMLElement | null>(null);
  const activeObjectIdRef = useRef<number | null>(null);
  const cardGrabOffsetXRef = useRef(0);
  const sourceOriginRef = useRef({
    bottom: 0,
    centerX: 0,
    height: 0,
    rotation: 0,
    top: 0,
    width: 0,
  });
  const draggingCardRef = useRef(false);
  const castReadyRef = useRef(false);
  const suppressClickRef = useRef(false);
  const suppressResetRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearClickSuppression = useCallback(() => {
    suppressClickRef.current = false;
    if (suppressResetRef.current != null) {
      clearTimeout(suppressResetRef.current);
      suppressResetRef.current = null;
    }
  }, []);

  const clearHoldTimer = useCallback(() => {
    if (timerRef.current != null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const clearActiveCard = useCallback(() => {
    activeCardRef.current?.removeAttribute("data-hand-touch-active");
    activeCardRef.current = null;
    activeObjectIdRef.current = null;
    castReadyRef.current = false;
    draggingCardRef.current = false;
  }, []);

  const inspectAtPoint = useCallback(
    (x: number, y: number) => {
      const container = containerRef.current;
      if (!container) return null;
      const card = cardAtPoint(container, x, y);
      if (!card) return activeObjectIdRef.current;
      if (card === activeCardRef.current) return activeObjectIdRef.current;

      const objectId = Number(card.dataset.objectId);
      if (!Number.isFinite(objectId)) return activeObjectIdRef.current;

      clearActiveCard();
      card.dataset.handTouchActive = "true";
      activeCardRef.current = card;
      activeObjectIdRef.current = objectId;
      const rect = card.getBoundingClientRect();
      const centerX = (rect.left + rect.right) / 2;
      const rotation = Number(card.dataset.handRotation);
      const height = card.offsetHeight || rect.height;
      sourceOriginRef.current = {
        bottom: rect.bottom,
        centerX,
        height,
        rotation: Number.isFinite(rotation) ? rotation : 0,
        top: rect.bottom - height,
        width: card.offsetWidth || rect.width,
      };
      cardGrabOffsetXRef.current = x - centerX;
      inspectObject(objectId, undefined, "immediate");
      setPreviewSticky(true);
      return objectId;
    },
    [clearActiveCard, containerRef, inspectObject, setPreviewSticky],
  );

  const updateGesture = useCallback(
    (objectId: number, x: number, y: number, phase: "preview" | "drag") => {
      const container = containerRef.current;
      if (!activeCardRef.current || !container) return;
      const bounds = container.getBoundingClientRect();
      const playable = isPlayable?.(objectId) ?? false;
      const offsetY = Math.min(0, y - startRef.current.y);
      const castReady = Boolean(
        phase === "drag"
        && playable
        && canReleaseToCast?.(objectId)
        && offsetY < HAND_DRAG_PLAY_THRESHOLD
        && y < bounds.top,
      );
      castReadyRef.current = castReady;
      setMobileHandGesture({
        objectId,
        phase,
        sourceOrigin: sourceOriginRef.current,
        offsetX: x - sourceOriginRef.current.centerX - cardGrabOffsetXRef.current,
        offsetY,
        playable,
        castReady,
      });
    },
    [canReleaseToCast, containerRef, isPlayable, setMobileHandGesture],
  );

  const finishScrub = useCallback((allowCast = false) => {
    const wasScrubbing = scrubbingRef.current;
    const castObjectId =
      allowCast && castReadyRef.current ? activeObjectIdRef.current : null;
    clearHoldTimer();
    scrubbingRef.current = false;
    pointerIdRef.current = null;
    clearActiveCard();
    setMobileHandGesture(null);
    if (wasScrubbing) {
      dismissPreview();
      suppressClickRef.current = true;
      if (suppressResetRef.current != null) clearTimeout(suppressResetRef.current);
      // WKWebView can dispatch the compatibility click well after pointerup.
      // Keep the guard alive long enough to consume it; a new pointer gesture
      // clears the guard immediately so a deliberate follow-up tap still works.
      suppressResetRef.current = setTimeout(
        clearClickSuppression,
        RELEASE_CLICK_SUPPRESSION_MS,
      );
    }
    if (castObjectId != null) onReleaseToCast?.(castObjectId);
  }, [clearActiveCard, clearClickSuppression, clearHoldTimer, dismissPreview, onReleaseToCast, setMobileHandGesture]);

  useEffect(() => {
    return () => {
      clearHoldTimer();
      clearActiveCard();
      clearClickSuppression();
      setMobileHandGesture(null);
      if (scrubbingRef.current) dismissPreview();
    };
  }, [clearActiveCard, clearClickSuppression, clearHoldTimer, dismissPreview, setMobileHandGesture]);

  useEffect(() => {
    const container = containerRef.current;
    if (!enabled || !container) return;

    // `touch-action: none` is the declarative guard, but iOS Safari/WKWebView
    // can still begin rubber-banding while the long-press timer is pending.
    // Claim touch movement that started on the hand with a non-passive native
    // listener so the browser cannot move the viewport before scrubbing starts.
    const preventNativeHandPan = (event: TouchEvent) => {
      if (pointerIdRef.current == null || !event.cancelable) return;
      event.preventDefault();
    };

    container.addEventListener("touchmove", preventNativeHandPan, { passive: false });
    return () => container.removeEventListener("touchmove", preventNativeHandPan);
  }, [containerRef, enabled]);

  const onPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      if (!enabled || event.pointerType === "mouse" || !event.isPrimary || event.button !== 0) {
        return;
      }

      clearClickSuppression();
      clearHoldTimer();
      setMobileHandGesture(null);
      pointerIdRef.current = event.pointerId;
      startRef.current = { x: event.clientX, y: event.clientY };
      try {
        event.currentTarget.setPointerCapture(event.pointerId);
      } catch {
        // Pointer capture is best-effort on older WKWebView versions.
      }

      const { x, y } = startRef.current;
      timerRef.current = setTimeout(() => {
        timerRef.current = null;
        const objectId = inspectAtPoint(x, y);
        scrubbingRef.current = objectId != null;
        if (objectId != null) updateGesture(objectId, x, y, "preview");
      }, HOLD_DELAY_MS);
    },
    [clearClickSuppression, clearHoldTimer, enabled, inspectAtPoint, setMobileHandGesture, updateGesture],
  );

  const onPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      if (pointerIdRef.current !== event.pointerId) return;

      // Claim the gesture throughout the hold delay as well as during active
      // scrubbing. Waiting until the preview opens lets WebKit start a native
      // pan that pointer capture cannot take back.
      event.preventDefault();

      if (!scrubbingRef.current) {
        const dx = event.clientX - startRef.current.x;
        const dy = event.clientY - startRef.current.y;
        if (
          dx * dx + dy * dy
          > PRE_HOLD_MOVE_THRESHOLD_PX * PRE_HOLD_MOVE_THRESHOLD_PX
        ) {
          clearHoldTimer();
        }
        return;
      }

      const offsetY = event.clientY - startRef.current.y;
      if (offsetY <= -HELD_CARD_DRAG_START_PX) draggingCardRef.current = true;
      const objectId = draggingCardRef.current
        ? activeObjectIdRef.current
        : inspectAtPoint(event.clientX, event.clientY);
      if (objectId != null) {
        updateGesture(
          objectId,
          event.clientX,
          event.clientY,
          draggingCardRef.current ? "drag" : "preview",
        );
      }
    },
    [clearHoldTimer, inspectAtPoint, updateGesture],
  );

  const onPointerUp = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      if (scrubbingRef.current) event.preventDefault();
      // WebKit may deliver the finger's final position only with pointerup.
      // Recompute from that authoritative coordinate so returning below the
      // cast boundary always disarms release-to-cast, even when no final
      // pointermove was emitted.
      const objectId = activeObjectIdRef.current;
      if (scrubbingRef.current && draggingCardRef.current && objectId != null) {
        updateGesture(objectId, event.clientX, event.clientY, "drag");
      }
      try {
        event.currentTarget.releasePointerCapture(event.pointerId);
      } catch {
        // Ignore capture-release mismatches from WebKit and test harnesses.
      }
      finishScrub(true);
    },
    [finishScrub, updateGesture],
  );

  const onPointerCancel = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      if (pointerIdRef.current !== event.pointerId) return;
      finishScrub(false);
    },
    [finishScrub],
  );

  const consumeClick = useCallback(() => {
    if (!suppressClickRef.current) return false;
    clearClickSuppression();
    return true;
  }, [clearClickSuppression]);

  return {
    handlers: { onPointerDown, onPointerMove, onPointerUp, onPointerCancel },
    consumeClick,
  };
}
