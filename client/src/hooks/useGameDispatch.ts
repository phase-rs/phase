import { useCallback } from "react";

import type { GameAction } from "../adapter/types";
import { currentSnapshot, dispatchAction } from "../game/dispatch";

/**
 * Backward-compatible hook delegating to the standalone dispatch.
 * New code should prefer useDispatch() from GameProvider context.
 */
export function useGameDispatch(): (action: GameAction) => Promise<void> {
  return useCallback((action: GameAction) => dispatchAction(action), []);
}

export { currentSnapshot };
