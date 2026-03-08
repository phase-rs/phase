import { useCallback } from "react";

import type { GameAction } from "../adapter/types.ts";
import { useAnimationStore } from "../stores/animationStore.ts";
import { useGameStore } from "../stores/gameStore.ts";

/**
 * Wraps gameStore.dispatch with animation queue integration.
 * Flow: dispatch action -> enqueue animation effects -> wait for drain -> UI renders final state.
 */
export function useGameDispatch(): (action: GameAction) => Promise<void> {
  const dispatch = useGameStore((s) => s.dispatch);
  const enqueueEffects = useAnimationStore((s) => s.enqueueEffects);

  return useCallback(
    async (action: GameAction) => {
      const events = await dispatch(action);

      if (events.length > 0) {
        enqueueEffects(events);
      }
    },
    [dispatch, enqueueEffects],
  );
}
