import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { type MotionStyle, type MotionValue, type PanInfo, useMotionValue } from "framer-motion";

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

/** Release within this many pixels of the docked home (offset 0,0) and the
 *  widget magnetically snaps back home. Matches the home-ghost affordance. */
const SNAP_HOME_PX = 28;

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
  onDragStart: () => void;
  onDragEnd: () => void;
  onClickCapture?: (e: React.MouseEvent) => void;
  /** True while a reposition drag is in progress — gates the home-ghost. */
  dragging: boolean;
  /** Live offset motion values, so the call site can counter-translate a
   *  home-ghost marker to the docked position (the "snap zone"). */
  x: MotionValue<number>;
  y: MotionValue<number>;
  /** Box-scale applied to this widget (1 unless box-scalable + scaled). The
   *  home-ghost must divide its counter-translate by this, since the scale
   *  composes with `x`/`y` in the same transform. */
  scale: number;
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

/** Widgets that support a whole-box `transform: scale()` (the ones with a scale
 *  stepper in the edit toolbar). Their keys are both `FlexWidgetKey` and
 *  `FlexScaleKey`, so the lookup below narrows safely. */
const BOX_SCALABLE_WIDGETS = new Set<FlexWidgetKey>(["actionRail", "playerPiles"]);

function useTargetScale(target: DraggableTarget): number {
  return usePreferencesStore((s) => {
    if (target.kind !== "widget" || !BOX_SCALABLE_WIDGETS.has(target.key)) return 1;
    return s.flexLayout.scales?.[target.key as "actionRail" | "playerPiles"] ?? 1;
  });
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
  const scale = useTargetScale(target);
  const ref = useRef<HTMLDivElement>(null);
  const x = useMotionValue(offset?.dx ?? 0);
  const y = useMotionValue(offset?.dy ?? 0);
  const [dragging, setDragging] = useState(false);

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

  const onDragStart = useCallback(() => setDragging(true), []);

  const onDragEnd = useCallback(() => {
    setDragging(false);
    const dx = Math.round(x.get());
    const dy = Math.round(y.get());
    // Magnetic snap: released near the docked home → snap back to it exactly.
    if (Math.abs(dx) <= SNAP_HOME_PX && Math.abs(dy) <= SNAP_HOME_PX) {
      x.set(0);
      y.set(0);
      persist({ dx: 0, dy: 0 });
      return;
    }
    persist({ dx, dy });
  }, [persist, x, y]);

  // In edit mode, a functional control (rail button, pile, stack/log) is
  // drag-only — swallow the click so a reposition tap can't also fire its action.
  const onClickCapture = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
  }, []);

  return {
    ref,
    // `scale` composes with the drag translate; only emitted when non-default so
    // an unscaled widget's transform is untouched. transform-origin is set at the
    // call site (each widget anchors its scale to its docked corner).
    style: scale !== 1 ? { x, y, scale } : { x, y },
    drag: flexEditMode,
    dragMomentum: false,
    dragElastic: 0,
    onDragStart,
    onDragEnd,
    onClickCapture: flexEditMode ? onClickCapture : undefined,
    dragging,
    x,
    y,
    scale,
  };
}

/** Re-export for call sites that wire `onDragEnd`-style handlers themselves. */
export type { PanInfo };
