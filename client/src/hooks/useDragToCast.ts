import { useCallback } from "react";
import type { PanInfo } from "framer-motion";

import type { GameAction } from "../adapter/types.ts";
import { dispatchAction } from "../game/dispatch.ts";

/**
 * Upward-drag threshold (pixels) at which a drag gesture counts as "play this
 * card." Negative because Framer Motion's y-axis grows downward.
 */
export const DRAG_PLAY_THRESHOLD = -20;

interface UseDragToCastOptions {
  /**
   * Whether the local player currently holds priority. When false, no drag
   * ever triggers a play.
   */
  hasPriority: boolean;
  /**
   * Optional predicate that suppresses the cast when the pointer ends inside
   * the source zone's bounds (hand drops back into hand, for example). Omit
   * when the source zone has no "release without playing" gesture — e.g. the
   * command zone, where any upward drag past the threshold always casts.
   */
  isInSourceZone?: (info: PanInfo) => boolean;
  /**
   * Direct CastSpell action for a single-cast card (Commander zone path).
   * When provided, drag-end dispatches this action. Mutually exclusive with
   * `onPlay` — if both are set, `onPlay` takes precedence since it can
   * encode richer play choices (flashback, adventure, channel, etc.).
   */
  castAction?: GameAction | null;
  /**
   * Custom play callback for source zones that resolve to more than a single
   * CastSpell (e.g. hand cards that may have a choice modal between cast and
   * an activated ability). Invoked only when the gesture passes all gates.
   */
  onPlay?: () => void;
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
}: UseDragToCastOptions) {
  return useCallback(
    (_event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo): boolean => {
      if (!hasPriority) return false;
      if (isInSourceZone?.(info)) return false;
      if (info.offset.y >= DRAG_PLAY_THRESHOLD) return false;
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
    [castAction, onPlay, hasPriority, isInSourceZone],
  );
}
