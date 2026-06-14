import { motion } from "framer-motion";

import {
  useDraggableWidget,
  type DraggableTarget,
} from "../../hooks/useDraggableWidget.ts";

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
  children,
}: DraggableWidgetProps) {
  const {
    ref,
    style: motionStyle,
    drag,
    dragMomentum,
    dragElastic,
    onDragEnd,
    onClickCapture,
  } = useDraggableWidget(target);
  return (
    <motion.div
      ref={ref}
      data-flex-zone={flexZone}
      drag={drag}
      dragMomentum={dragMomentum}
      dragElastic={dragElastic}
      onDragEnd={onDragEnd}
      onClickCapture={onClickCapture}
      // In edit mode force the node grabbable even if its normal className is
      // `pointer-events-none` (e.g. a zone rail whose dead space must not block
      // the board during play). Outside edit mode, defer to the className.
      style={{ ...style, ...motionStyle, pointerEvents: drag ? "auto" : undefined }}
      className={className}
    >
      {children}
    </motion.div>
  );
}
