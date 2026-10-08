import { useCallback } from "react";
import type { PanInfo } from "framer-motion";

import type { GameAction } from "../adapter/types.ts";
import { dispatchAction } from "../game/dispatch.ts";

/**
 * Upward-drag threshold (pixels) at which a drag gesture counts as "play this
 * card." Negative because Framer Motion's y-axis grows downward.
 */
export const DRAG_PLAY_THRESHOLD = -20;

/**
 * Hand cards need a larger upward commitment than other draggable cast
 * surfaces. This leaves room for lateral hand reordering without a small
 * upward component accidentally becoming a cast.
 */
export const HAND_DRAG_PLAY_THRESHOLD = -64;

interface UseDragToCastOptions {
  hasPriority: boolean;
  isInSourceZone?: (info: PanInfo) => boolean;
  castAction?: GameAction | null;
  onPlay?: () => void;
  /** Use Euclidean distance instead of y-only for the drag threshold. */
  useDistanceThreshold?: boolean;
}

/**
 * Returns an onDragEnd handler that plays the card when the user drags
 * upward past `DRAG_PLAY_THRESHOLD` while holding priority. Exactly one of
 * `castAction` or `onPlay` should be supplied. Returns a boolean indicating
 * whether the drag triggered a play — callers may use this to gate their
 * own post-drag cleanup (e.g. suppressing the subsequent click).
 */
export function useDragToCast({
  castAction,
  onPlay,
  hasPriority,
  isInSourceZone,
  useDistanceThreshold,
}: UseDragToCastOptions) {
  return useCallback(
    (_event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo): boolean => {
      if (!hasPriority) return false;
      if (isInSourceZone?.(info)) return false;
      const pastThreshold = useDistanceThreshold
        ? Math.hypot(info.offset.x, info.offset.y) >= Math.abs(DRAG_PLAY_THRESHOLD)
        : info.offset.y < DRAG_PLAY_THRESHOLD;
      if (!pastThreshold) return false;
      if (onPlay) {
        onPlay();
        return true;
      }
      if (castAction) {
        dispatchAction(castAction);
        return true;
      }
      return false;
    },
    [castAction, onPlay, hasPriority, isInSourceZone, useDistanceThreshold],
  );
}
