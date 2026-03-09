import { createContext, useContext, useEffect, type ReactNode } from "react";

import type { GameAction } from "../adapter/types";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction } from "../game/dispatch";

const GameDispatchContext = createContext<(action: GameAction) => Promise<void>>(
  () => {
    throw new Error("No GameProvider found in component tree");
  },
);

interface GameProviderProps {
  mode: "ai" | "online" | "local";
  difficulty?: string;
  children: ReactNode;
}

export function GameProvider({ mode, difficulty, children }: GameProviderProps) {
  useEffect(() => {
    const controller = createGameLoopController({ mode, difficulty });
    controller.start();
    return () => controller.dispose();
  }, [mode, difficulty]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}

export function useDispatch(): (action: GameAction) => Promise<void> {
  return useContext(GameDispatchContext);
}
