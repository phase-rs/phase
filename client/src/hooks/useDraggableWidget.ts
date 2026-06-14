import { useCallback, useLayoutEffect, useRef } from "react";
import { type MotionStyle, type PanInfo, useMotionValue } from "framer-motion";

import { useUiStore } from "../stores/uiStore.ts";
import {
  usePreferencesStore,
  type FlexTableSize,
  type FlexWidgetKey,
  type WidgetOffset,
} from "../stores/preferencesStore.ts";

/** Keep at least this many pixels of a widget on-screen when clamping a
 *  cross-monitor offset back into view. */
const VIEWPORT_MARGIN = 24;

/** What a draggable wrapper repositions. A shared-global widget, or the opponent
 *  HUD whose offset is keyed by table size (1v1 vs multiplayer). */
export type DraggableTarget =
  | { kind: "widget"; key: FlexWidgetKey }
  | { kind: "opponentHud"; tableSize: FlexTableSize };

/** Props to spread onto the inner `motion.div` that wraps a widget's content.
 *  `x`/`y` apply the persisted offset at all times (so a customized layout
 *  survives normal play); `drag` is enabled only in Flex Layout edit mode. */
export interface DraggableWidgetProps {
  ref: React.RefObject<HTMLDivElement | null>;
  style: MotionStyle;
  drag: boolean;
  dragMomentum: false;
  dragElastic: 0;
  onDragEnd: () => void;
  onClickCapture?: (e: React.MouseEvent) => void;
}

function useTargetOffset(target: DraggableTarget): WidgetOffset | undefined {
  // Scoped selector: returns the same reference until THIS target's offset
  // changes, so dragging one widget never re-renders the others.
  return usePreferencesStore((s) =>
    target.kind === "widget"
      ? s.flexLayout.widgets[target.key]
      : s.flexLayout.opponentHudByTableSize[target.tableSize],
  );
}

/**
 * Makes a board widget drag-repositionable in Flex Layout edit mode, persisting
 * its offset to `preferencesStore`. Net-new infrastructure built directly on
 * Framer Motion's `drag` (it is NOT a wrapper over `useDragToCast`, which only
 * exposes a threshold `onDragEnd`). Returns props for an inner `motion.div`;
 * the call site decides which node to wrap so existing transforms (e.g. a HUD's
 * `-translate-x-1/2`) on the outer node are never clobbered.
 */
export function useDraggableWidget(target: DraggableTarget): DraggableWidgetProps {
  const flexEditMode = useUiStore((s) => s.flexEditMode);
  const offset = useTargetOffset(target);
  const ref = useRef<HTMLDivElement>(null);
  const x = useMotionValue(offset?.dx ?? 0);
  const y = useMotionValue(offset?.dy ?? 0);

  // Seed/re-sync the motion values from the persisted offset (a preset apply or
  // reset returns the widget home), THEN visually clamp into the viewport so a
  // cloud-synced offset from a larger monitor can't strand it off-screen. Both
  // steps must live in ONE layout effect: a separate passive re-sync would run
  // after this and overwrite the clamp. The clamp adjusts the motion values
  // only — it must NOT persist, or it would wrongly flip activePreset to
  // "custom" on load.
  useLayoutEffect(() => {
    x.set(offset?.dx ?? 0);
    y.set(offset?.dy ?? 0);
    const el = ref.current;
    if (!el || offset == null) return;
    const rect = el.getBoundingClientRect();
    let cx = x.get();
    let cy = y.get();
    if (rect.left > window.innerWidth - VIEWPORT_MARGIN) {
      cx -= rect.left - (window.innerWidth - VIEWPORT_MARGIN);
    }
    if (rect.top > window.innerHeight - VIEWPORT_MARGIN) {
      cy -= rect.top - (window.innerHeight - VIEWPORT_MARGIN);
    }
    if (rect.right < VIEWPORT_MARGIN) cx += VIEWPORT_MARGIN - rect.right;
    if (rect.bottom < VIEWPORT_MARGIN) cy += VIEWPORT_MARGIN - rect.bottom;
    if (cx !== x.get()) x.set(cx);
    if (cy !== y.get()) y.set(cy);
  }, [offset?.dx, offset?.dy, offset, x, y]);

  const persist = useCallback(
    (next: WidgetOffset) => {
      const store = usePreferencesStore.getState();
      if (target.kind === "widget") {
        store.setFlexWidgetOffset(target.key, next);
      } else {
        store.setFlexOpponentHudOffset(target.tableSize, next);
      }
    },
    [target],
  );

  const onDragEnd = useCallback(() => {
    persist({ dx: Math.round(x.get()), dy: Math.round(y.get()) });
  }, [persist, x, y]);

  // In edit mode, a functional control (rail button, pile, stack/log) is
  // drag-only — swallow the click so a reposition tap can't also fire its action.
  const onClickCapture = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
  }, []);

  return {
    ref,
    style: { x, y },
    drag: flexEditMode,
    dragMomentum: false,
    dragElastic: 0,
    onDragEnd,
    onClickCapture: flexEditMode ? onClickCapture : undefined,
  };
}

/** Re-export for call sites that wire `onDragEnd`-style handlers themselves. */
export type { PanInfo };
