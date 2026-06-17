import { motion, useTransform } from "framer-motion";

import {
  useDraggableWidget,
  type DraggableTarget,
} from "../../hooks/useDraggableWidget.ts";
import { type FlexScaleKey } from "../../stores/preferencesStore.ts";
import { ResizeHandle } from "./ResizeHandle.tsx";

interface DraggableWidgetProps {
  /** What this wrapper repositions (a shared widget, or the table-size-keyed
   *  opponent HUD). */
  target: DraggableTarget;
  /** `data-flex-zone` value so {@link FlexEditOverlay} can anchor its outline. */
  flexZone: string;
  className?: string;
  /** Positioning style carried from the original node (e.g. a zone rail's
   *  CSS-var style). Merged under the motion `x`/`y` so the drag offset wins. */
  style?: React.CSSProperties;
  /** If set, the widget shows a corner resize grip in edit mode that scales
   *  `scales[scaleKey]`. */
  scaleKey?: FlexScaleKey;
  /** Corner the resize grip sits at (default bottom-right). */
  resizeCorner?: "br" | "bl";
  children: React.ReactNode;
}

/**
 * Wraps a board widget's content in a Framer Motion node that applies its
 * persisted offset at all times and becomes draggable in Flex Layout edit mode.
 * This is the single integration point every call site uses — wrap the widget's
 * CONTENT (not its positioned outer node), so an existing transform on the outer
 * node (e.g. a HUD's `-translate-x-1/2`) is never clobbered.
 */
export function DraggableWidget({
  target,
  flexZone,
  className,
  style,
  scaleKey,
  resizeCorner,
  children,
}: DraggableWidgetProps) {
  const {
    ref,
    style: motionStyle,
    drag,
    dragMomentum,
    dragElastic,
    onDragStart,
    onDragEnd,
    onClickCapture,
    dragging,
    x,
    y,
    scale,
  } = useDraggableWidget(target);
  // Counter-translate a ghost outline to the docked home position: the node is
  // transformed by `translate(x,y) scale(s)`, so the ghost's own translate lands
  // in scaled space — dividing by `s` cancels it, placing the ghost exactly on
  // the dock (a live "snap zone" marker shown only mid-drag). s=1 ⇒ plain -x/-y.
  const ghostX = useTransform(x, (v) => -v / scale);
  const ghostY = useTransform(y, (v) => -v / scale);
  return (
    <motion.div
      ref={ref}
      data-flex-zone={flexZone}
      drag={drag}
      dragMomentum={dragMomentum}
      dragElastic={dragElastic}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onClickCapture={onClickCapture}
      // In edit mode force the node grabbable even if its normal className is
      // `pointer-events-none` (e.g. a zone rail whose dead space must not block
      // the board during play). Outside edit mode, defer to the className.
      style={{ ...style, ...motionStyle, pointerEvents: drag ? "auto" : undefined }}
      // Grab cursor in edit mode signals the whole widget is draggable.
      className={drag ? `${className ?? ""} cursor-grab active:cursor-grabbing` : className}
    >
      {dragging && (
        <motion.div
          aria-hidden
          style={{ x: ghostX, y: ghostY }}
          className="pointer-events-none absolute inset-0 z-40 rounded-lg border-2 border-dashed border-sky-300/70 bg-sky-400/10"
        />
      )}
      {children}
      {drag && scaleKey && <ResizeHandle scaleKey={scaleKey} corner={resizeCorner} />}
    </motion.div>
  );
}
