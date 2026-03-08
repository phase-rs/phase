import { useCallback, useEffect } from "react";

import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { LifeTotal } from "../components/controls/LifeTotal.tsx";
import { PassButton } from "../components/controls/PassButton.tsx";
import { PhaseTracker } from "../components/controls/PhaseTracker.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLog } from "../components/log/GameLog.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { WasmAdapter } from "../adapter/wasm-adapter.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";

export function GamePage() {
  const initGame = useGameStore((s) => s.initGame);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();
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

  const handleMulliganChoice = useCallback(
    (id: string) => {
      dispatch({
        type: "MulliganDecision",
        data: { keep: id === "keep" },
      });
    },
    [dispatch],
  );

  const handleBottomCards = useCallback(
    (id: string) => {
      const cards = id.split(",").map(Number).filter(Boolean);
      dispatch({ type: "SelectCards", data: { cards } });
    },
    [dispatch],
  );

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

      {/* Animation overlay (above board, below modals) */}
      <AnimationOverlay />

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} />

      {/* WaitingFor-driven prompt overlays */}
      {waitingFor?.type === "TargetSelection" && <TargetingOverlay />}
      {waitingFor?.type === "ManaPayment" && <ManaPaymentUI />}
      {waitingFor?.type === "ReplacementChoice" && <ReplacementModal />}

      {waitingFor?.type === "MulliganDecision" && (
        <ChoiceModal
          title={`Mulligan (${waitingFor.data.mulligan_count} cards)`}
          options={[
            { id: "keep", label: "Keep Hand" },
            {
              id: "mulligan",
              label: "Mulligan",
              description: `Draw ${7 - waitingFor.data.mulligan_count - 1} cards`,
            },
          ]}
          onChoose={handleMulliganChoice}
        />
      )}

      {waitingFor?.type === "MulliganBottomCards" && (
        <MulliganBottomCardsPrompt
          playerId={waitingFor.data.player}
          count={waitingFor.data.count}
          onChoose={handleBottomCards}
        />
      )}

      {waitingFor?.type === "GameOver" && (
        <GameOverScreen winner={waitingFor.data.winner} />
      )}

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

// ── Mulligan Bottom Cards ─────────────────────────────────────────────────

interface MulliganBottomCardsPromptProps {
  playerId: number;
  count: number;
  onChoose: (id: string) => void;
}

function MulliganBottomCardsPrompt({
  playerId,
  count,
  onChoose,
}: MulliganBottomCardsPromptProps) {
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  const selectedTargets = useUiStore((s) => s.selectedTargets);
  const addTarget = useUiStore((s) => s.addTarget);

  if (!player || !objects) return null;

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  const isReady = selectedTargets.length === count;

  const handleConfirm = () => {
    onChoose(selectedTargets.join(","));
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" />
      <div className="relative z-10 w-full max-w-lg rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-2 text-center text-lg font-bold text-white">
          Put {count} card{count > 1 ? "s" : ""} on bottom
        </h2>
        <p className="mb-4 text-center text-sm text-gray-400">
          Select {count} card{count > 1 ? "s" : ""} to put on the bottom of
          your library
        </p>

        <div className="mb-4 flex flex-wrap justify-center gap-2">
          {handObjects.map((obj) => {
            const isSelected = selectedTargets.includes(obj.id);
            return (
              <button
                key={obj.id}
                onClick={() => {
                  if (!isSelected && selectedTargets.length < count) {
                    addTarget(obj.id);
                  }
                }}
                className={`rounded-lg px-3 py-2 text-sm transition ${
                  isSelected
                    ? "bg-cyan-600 text-white ring-2 ring-cyan-400"
                    : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                }`}
              >
                {obj.name}
              </button>
            );
          })}
        </div>

        <div className="flex justify-center">
          <button
            onClick={handleConfirm}
            disabled={!isReady}
            className={`rounded-lg px-6 py-2 font-semibold transition ${
              isReady
                ? "bg-cyan-600 text-white hover:bg-cyan-500"
                : "cursor-not-allowed bg-gray-700 text-gray-500"
            }`}
          >
            Confirm ({selectedTargets.length}/{count})
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Game Over Screen ──────────────────────────────────────────────────────

function GameOverScreen({ winner }: { winner: number | null }) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" />
      <div className="relative z-10 rounded-xl bg-gray-900 p-8 text-center shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-2 text-2xl font-bold text-white">Game Over</h2>
        <p className="text-lg text-gray-300">
          {winner != null
            ? winner === 0
              ? "You Win!"
              : "Opponent Wins"
            : "Draw"}
        </p>
      </div>
    </div>
  );
}
