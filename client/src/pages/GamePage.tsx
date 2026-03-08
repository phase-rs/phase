import { useEffect } from "react";

import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { LifeTotal } from "../components/controls/LifeTotal.tsx";
import { PassButton } from "../components/controls/PassButton.tsx";
import { PhaseTracker } from "../components/controls/PhaseTracker.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLog } from "../components/log/GameLog.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { WasmAdapter } from "../adapter/wasm-adapter.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";

export function GamePage() {
  const initGame = useGameStore((s) => s.initGame);
  const gameState = useGameStore((s) => s.gameState);
  const reset = useGameStore((s) => s.reset);
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const objects = gameState?.objects;

  const inspectedCardName =
    inspectedObjectId != null && objects
      ? (objects[inspectedObjectId]?.name ?? null)
      : null;

  useEffect(() => {
    const adapter = new WasmAdapter();
    initGame(adapter);
    return () => {
      reset();
    };
  }, [initGame, reset]);

  return (
    <div className="flex h-screen bg-gray-950">
      {/* Main board area */}
      <div className="flex flex-1 flex-col overflow-hidden">
        <OpponentHand />
        <GameBoard />
        <PlayerHand />
      </div>

      {/* Right side panel */}
      <div className="flex w-64 flex-col gap-3 border-l border-gray-800 bg-gray-900/50 p-3 lg:w-72">
        {/* Opponent life */}
        <LifeTotal playerId={1} />

        {/* Phase tracker */}
        <PhaseTracker />

        {/* Stack */}
        <StackDisplay />

        {/* Game log (fills remaining space) */}
        <GameLog />

        {/* Player life */}
        <LifeTotal playerId={0} />

        {/* Controls */}
        <div className="flex items-center gap-2">
          <PassButton />
          <FullControlToggle />
        </div>
      </div>

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} />

      {/* Responsive: side panel collapses to bottom drawer on small screens */}
      <style>{`
        @media (max-width: 768px) {
          .flex.h-screen {
            flex-direction: column;
          }
          .flex.h-screen > .w-64,
          .flex.h-screen > .lg\\:w-72 {
            width: 100%;
            max-height: 40vh;
            border-left: none;
            border-top: 1px solid rgb(31 41 55);
          }
        }
      `}</style>
    </div>
  );
}
