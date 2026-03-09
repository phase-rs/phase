import { useCallback, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";

import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardImage } from "../components/card/CardImage.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { PassButton } from "../components/controls/PassButton.tsx";
import { PhaseStopBar } from "../components/controls/PhaseStopBar.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLogPanel } from "../components/log/GameLogPanel.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { CombatOverlay } from "../components/combat/CombatOverlay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { PlayerHud } from "../components/hud/PlayerHud.tsx";
import { OpponentHud } from "../components/hud/OpponentHud.tsx";
import { ZoneIndicator } from "../components/zone/ZoneIndicator.tsx";
import { ZoneViewer } from "../components/zone/ZoneViewer.tsx";
import { PreferencesModal } from "../components/settings/PreferencesModal.tsx";
import type { WsAdapterEvent } from "../adapter/ws-adapter.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { GameProvider } from "../providers/GameProvider.tsx";
import { PLAYER_ID } from "../constants/game.ts";

export function GamePage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const rawMode = searchParams.get("mode");
  const difficulty = searchParams.get("difficulty") ?? "Medium";
  const joinCode = searchParams.get("code") ?? "";

  // Map URL modes to GameProvider modes
  const mode: "ai" | "online" | "local" =
    rawMode === "host" || rawMode === "join" ? "online" : rawMode === "ai" ? "ai" : "local";

  const [showCardDataMissing, setShowCardDataMissing] = useState(false);

  // Online multiplayer state
  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [waitingForOpponent, setWaitingForOpponent] = useState(false);
  const [opponentDisconnected, setOpponentDisconnected] = useState(false);
  const [disconnectGrace, setDisconnectGrace] = useState(0);
  const [reconnectState, setReconnectState] = useState<
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" }
  >({ status: "idle" });

  const handleWsEvent = useCallback((event: WsAdapterEvent) => {
    switch (event.type) {
      case "gameCreated":
        setHostGameCode(event.gameCode);
        break;
      case "waitingForOpponent":
        setWaitingForOpponent(true);
        break;
      case "opponentDisconnected":
        setOpponentDisconnected(true);
        setDisconnectGrace(event.graceSeconds);
        break;
      case "opponentReconnected":
        setOpponentDisconnected(false);
        break;
      case "reconnecting":
        setReconnectState({
          status: "reconnecting",
          attempt: event.attempt,
          maxAttempts: event.maxAttempts,
        });
        break;
      case "reconnected":
        setReconnectState({ status: "idle" });
        break;
      case "reconnectFailed":
        setReconnectState({ status: "failed" });
        break;
    }
  }, []);

  const handleReady = useCallback(() => {
    setWaitingForOpponent(false);
  }, []);

  const handleNoDeck = useCallback(() => {
    navigate("/");
  }, [navigate]);

  const handleCardDataMissing = useCallback(() => {
    setShowCardDataMissing(true);
  }, []);

  return (
    <GameProvider
      mode={mode}
      difficulty={difficulty}
      joinCode={joinCode || undefined}
      onWsEvent={mode === "online" ? handleWsEvent : undefined}
      onReady={mode === "online" ? handleReady : undefined}
      onCardDataMissing={handleCardDataMissing}
      onNoDeck={handleNoDeck}
    >
      <GamePageContent
        mode={rawMode}
        hostGameCode={hostGameCode}
        waitingForOpponent={waitingForOpponent}
        opponentDisconnected={opponentDisconnected}
        disconnectGrace={disconnectGrace}
        reconnectState={reconnectState}
        showCardDataMissing={showCardDataMissing}
        onDismissCardDataMissing={() => setShowCardDataMissing(false)}
      />
    </GameProvider>
  );
}

interface GamePageContentProps {
  mode: string | null;
  hostGameCode: string | null;
  waitingForOpponent: boolean;
  opponentDisconnected: boolean;
  disconnectGrace: number;
  reconnectState:
    | { status: "idle" }
    | { status: "reconnecting"; attempt: number; maxAttempts: number }
    | { status: "failed" };
  showCardDataMissing: boolean;
  onDismissCardDataMissing: () => void;
}

function GamePageContent({
  mode,
  hostGameCode,
  waitingForOpponent,
  opponentDisconnected,
  disconnectGrace,
  reconnectState,
  showCardDataMissing,
  onDismissCardDataMissing,
}: GamePageContentProps) {
  const navigate = useNavigate();
  const containerRef = useRef<HTMLDivElement>(null);

  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const objects = gameState?.objects;
  const [showAiHand, setShowAiHand] = useState(false);
  const [viewingZone, setViewingZone] = useState<{
    zone: "graveyard" | "exile";
    playerId: number;
  } | null>(null);
  const [showPreferences, setShowPreferences] = useState(false);

  const isDragging = useUiStore((s) => s.isDragging);
  const inspectedCardName =
    !isDragging && inspectedObjectId != null && objects
      ? (objects[inspectedObjectId]?.name ?? null)
      : null;

  useKeyboardShortcuts();

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

  const isReconnecting = reconnectState.status !== "idle";

  return (
    <div ref={containerRef} className="relative h-screen w-screen overflow-hidden bg-gray-950">
      <StackDisplay />

      {/* Reconnecting banner */}
      {reconnectState.status === "reconnecting" && (
        <div className="fixed left-0 right-0 top-0 z-40 bg-amber-600 px-4 py-2 text-center text-sm font-semibold text-white">
          Reconnecting... (attempt {reconnectState.attempt}/{reconnectState.maxAttempts})
        </div>
      )}

      {/* Connection lost banner */}
      {reconnectState.status === "failed" && (
        <div className="fixed left-0 right-0 top-0 z-40 flex items-center justify-center gap-4 bg-red-700 px-4 py-2 text-sm font-semibold text-white">
          <span>Connection lost</span>
          <button
            onClick={() => navigate("/")}
            className="rounded bg-white/20 px-3 py-1 text-xs font-semibold hover:bg-white/30"
          >
            Return to Menu
          </button>
        </div>
      )}

      {/* Full-screen board layout */}
      <div className={`flex h-full flex-col${isReconnecting ? " pointer-events-none" : ""}`}>
        {/* Opponent area */}
        <OpponentHud />
        <OpponentHand showCards={showAiHand} />

        {/* Opponent battlefield */}
        <GameBoard />

        {/* Center divider with phase/stack/zone indicators */}
        <div className="flex items-center justify-center gap-4 border-y border-gray-800 px-4 py-1">
          <ZoneIndicator
            zone="graveyard"
            playerId={0}
            onClick={() => setViewingZone({ zone: "graveyard", playerId: 0 })}
          />
          <div className="flex items-center gap-2">
            <span className="text-xs text-gray-500">
              T{gameState?.turn_number ?? 0}
            </span>
            {gameState && (
              <span
                className={`rounded px-1.5 py-0.5 text-[10px] font-semibold ${
                  gameState.active_player === 0
                    ? "bg-cyan-900/60 text-cyan-300"
                    : "bg-red-900/60 text-red-300"
                }`}
              >
                {gameState.active_player === 0 ? "Your Turn" : "Opp Turn"}
              </span>
            )}
            <PhaseStopBar />
          </div>
          <ZoneIndicator
            zone="exile"
            playerId={0}
            onClick={() => setViewingZone({ zone: "exile", playerId: 0 })}
          />
        </div>

        {/* Player area */}
        <div className="flex items-center gap-2 px-4 py-1">
          <PlayerHud onSettingsClick={() => setShowPreferences(true)} />
          <div className="ml-auto flex items-center gap-2">
            <PassButton />
            <FullControlToggle />
          </div>
        </div>
        <PlayerHand />
      </div>

      {/* AI debug toggle */}
      {mode === "ai" && (
        <button
          onClick={() => setShowAiHand((v) => !v)}
          className="fixed right-2 top-2 z-40 rounded bg-gray-800/80 px-2 py-1 text-xs text-gray-400 hover:text-gray-200"
        >
          {showAiHand ? "Hide AI Hand" : "Show AI Hand"}
        </button>
      )}

      {/* Host game: show game code while waiting */}
      {waitingForOpponent && hostGameCode && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/70" />
          <div className="relative z-10 rounded-xl bg-gray-900 p-8 text-center shadow-2xl ring-1 ring-gray-700">
            <h2 className="mb-2 text-xl font-bold text-white">
              Waiting for Opponent
            </h2>
            <p className="mb-4 text-sm text-gray-400">
              Share this code with your opponent:
            </p>
            <p className="mb-4 font-mono text-4xl font-bold tracking-widest text-emerald-400">
              {hostGameCode}
            </p>
            <p className="text-xs text-gray-500">
              The game will start when your opponent joins.
            </p>
          </div>
        </div>
      )}

      {/* Join game: waiting overlay */}
      {waitingForOpponent && !hostGameCode && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/70" />
          <div className="relative z-10 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-gray-700">
            <h2 className="text-lg font-bold text-white">Joining Game...</h2>
            <p className="mt-2 text-sm text-gray-400">
              Connecting to game
            </p>
          </div>
        </div>
      )}

      {/* Opponent disconnected overlay */}
      {opponentDisconnected && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/60" />
          <div className="relative z-10 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-yellow-700">
            <h2 className="mb-2 text-lg font-bold text-yellow-400">
              Opponent Disconnected
            </h2>
            <p className="text-sm text-gray-300">
              Waiting for opponent to reconnect...
            </p>
            <p className="mt-2 text-xs text-gray-500">
              Game will forfeit in {disconnectGrace}s
            </p>
          </div>
        </div>
      )}

      {/* Card data missing modal */}
      {showCardDataMissing && (
        <CardDataMissingModal onContinue={onDismissCardDataMissing} />
      )}

      {/* Overlay layers */}
      <GameLogPanel />

      {viewingZone && (
        <ZoneViewer
          zone={viewingZone.zone}
          playerId={viewingZone.playerId}
          onClose={() => setViewingZone(null)}
        />
      )}

      {showPreferences && (
        <PreferencesModal onClose={() => setShowPreferences(false)} />
      )}

      {/* Animation overlay (above board, below modals) */}
      <AnimationOverlay containerRef={containerRef} />

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} />

      {/* WaitingFor-driven prompt overlays */}
      {waitingFor?.type === "TargetSelection" && <TargetingOverlay />}
      {waitingFor?.type === "DeclareAttackers" && <CombatOverlay mode="attackers" />}
      {waitingFor?.type === "DeclareBlockers" && <CombatOverlay mode="blockers" />}
      {waitingFor?.type === "ManaPayment" && <ManaPaymentUI />}
      {waitingFor?.type === "ReplacementChoice" && <ReplacementModal />}

      {waitingFor?.type === "MulliganDecision" && waitingFor.data.player === PLAYER_ID && (
        <MulliganDecisionPrompt
          playerId={waitingFor.data.player}
          mulliganCount={waitingFor.data.mulligan_count}
          onChoose={handleMulliganChoice}
        />
      )}

      {waitingFor?.type === "MulliganBottomCards" && waitingFor.data.player === PLAYER_ID && (
        <MulliganBottomCardsPrompt
          playerId={waitingFor.data.player}
          count={waitingFor.data.count}
          onChoose={handleBottomCards}
        />
      )}

      {waitingFor?.type === "GameOver" && (
        <GameOverScreen winner={waitingFor.data.winner} />
      )}
    </div>
  );
}

// ── Mulligan Bottom Cards ─────────────────────────────────────────────────

interface MulliganBottomCardsPromptProps {
  playerId: number;
  count: number;
  onChoose: (id: string) => void;
}

interface MulliganDecisionPromptProps {
  playerId: number;
  mulliganCount: number;
  onChoose: (id: string) => void;
}

function MulliganDecisionPrompt({
  playerId,
  mulliganCount,
  onChoose,
}: MulliganDecisionPromptProps) {
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);

  if (!player || !objects) {
    return (
      <ChoiceModal
        title={`Mulligan (${mulliganCount} cards)`}
        options={[
          { id: "keep", label: "Keep Hand" },
          {
            id: "mulligan",
            label: "Mulligan",
            description: `Draw ${7 - mulliganCount - 1} cards`,
          },
        ]}
        onChoose={onChoose}
      />
    );
  }

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  const nextHandSize = 7 - mulliganCount - 1;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" />
      <div className="relative z-10 w-full max-w-5xl rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-2 text-center text-lg font-bold text-white">
          Mulligan ({mulliganCount} cards)
        </h2>
        <p className="mb-4 text-center text-sm text-gray-400">
          Review your opening hand and choose to keep or mulligan.
        </p>

        <div className="mb-6 flex flex-wrap justify-center gap-2">
          {handObjects.map((obj) => (
            <div key={obj.id} className="rounded-lg border border-gray-700 bg-gray-800/60 p-1">
              <CardImage cardName={obj.name} size="small" />
            </div>
          ))}
        </div>

        <div className="flex justify-center gap-3">
          <button
            onClick={() => onChoose("keep")}
            className="rounded-lg bg-emerald-600 px-6 py-2 font-semibold text-white transition hover:bg-emerald-500"
          >
            Keep Hand
          </button>
          <button
            onClick={() => onChoose("mulligan")}
            className="rounded-lg bg-gray-800 px-6 py-2 font-semibold text-white transition hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50"
          >
            Mulligan (Draw {nextHandSize} cards)
          </button>
        </div>
      </div>
    </div>
  );
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
                className={`rounded-lg p-1 text-sm transition ${
                  isSelected
                    ? "bg-cyan-600 text-white ring-2 ring-cyan-400"
                    : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                }`}
              >
                <CardImage cardName={obj.name} size="small" />
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
