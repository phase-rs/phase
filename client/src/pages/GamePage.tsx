import { useEffect } from "react";

import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { WasmAdapter } from "../adapter/wasm-adapter.ts";

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
    <div className="flex h-screen flex-col bg-gray-950">
      <OpponentHand />
      <GameBoard />
      <PlayerHand />

      <CardPreview cardName={inspectedCardName} />
    </div>
  );
}
