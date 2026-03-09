import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";

import { AnimationOverlay } from "../components/animation/AnimationOverlay.tsx";
import { BattlefieldBackground } from "../components/board/BattlefieldBackground.tsx";
import { BlockAssignmentLines } from "../components/board/BlockAssignmentLines.tsx";
import { GameBoard } from "../components/board/GameBoard.tsx";
import { CardImage } from "../components/card/CardImage.tsx";
import { CardPreview } from "../components/card/CardPreview.tsx";
import { ActionButton } from "../components/board/ActionButton.tsx";
import { FullControlToggle } from "../components/controls/FullControlToggle.tsx";
import { CombatPhaseIndicator } from "../components/controls/PhaseStopBar.tsx";
import { OpponentHand } from "../components/hand/OpponentHand.tsx";
import { PlayerHand } from "../components/hand/PlayerHand.tsx";
import { GameLogPanel } from "../components/log/GameLogPanel.tsx";
import { DamageAssignmentModal } from "../components/combat/DamageAssignmentModal.tsx";
import { ManaPaymentUI } from "../components/mana/ManaPaymentUI.tsx";
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { PlayerHud } from "../components/hud/PlayerHud.tsx";
import { OpponentHud } from "../components/hud/OpponentHud.tsx";
import { GraveyardPile } from "../components/zone/GraveyardPile.tsx";
import { LibraryPile } from "../components/zone/LibraryPile.tsx";
import { ZoneIndicator } from "../components/zone/ZoneIndicator.tsx";
import { ZoneViewer } from "../components/zone/ZoneViewer.tsx";
import { PreferencesModal } from "../components/settings/PreferencesModal.tsx";
import type { WsAdapterEvent } from "../adapter/ws-adapter.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { useGameStore, clearGame } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";
import { GameProvider } from "../providers/GameProvider.tsx";
import { PLAYER_ID } from "../constants/game.ts";

export function GamePage() {
  const navigate = useNavigate();
  const { id: gameId } = useParams<{ id: string }>();
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

  if (!gameId) return null;

  return (
    <GameProvider
      gameId={gameId}
      mode={mode}
      difficulty={difficulty}
      joinCode={joinCode || undefined}
      onWsEvent={mode === "online" ? handleWsEvent : undefined}
      onReady={mode === "online" ? handleReady : undefined}
      onCardDataMissing={handleCardDataMissing}
      onNoDeck={handleNoDeck}
    >
      <GamePageContent
        gameId={gameId}
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
  gameId: string;
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
  gameId,
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

  // Sync card size preference to CSS custom properties
  const cardSize = usePreferencesStore((s) => s.cardSize);
  useEffect(() => {
    const root = document.documentElement;
    const scale = cardSize === "small" ? 0.8 : cardSize === "large" ? 1.25 : 1;
    root.style.setProperty("--card-size-scale", String(scale));
  }, [cardSize]);

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
      <BattlefieldBackground />
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
      <div className={`relative z-10 flex h-full flex-col${isReconnecting ? " pointer-events-none" : ""}`}>
        {/* Opponent hand at top */}
        <OpponentHand showCards={showAiHand} />

        {/* Opponent avatar centered below their hand */}
        <OpponentHud />

        {/* Battlefield */}
        <GameBoard />

        {/* Player avatar centered with flanking phase indicators */}
        <PlayerHud onSettingsClick={() => setShowPreferences(true)} />

        {/* Player hand at bottom */}
        <PlayerHand />
      </div>

      {/* Player zones — bottom-left: graveyard pile, library pile, exile badge */}
      <div className="fixed bottom-4 left-4 z-30 flex items-end gap-2">
        <GraveyardPile
          playerId={0}
          onClick={() => setViewingZone({ zone: "graveyard", playerId: 0 })}
        />
        <LibraryPile playerId={0} />
        <ZoneIndicator
          zone="exile"
          playerId={0}
          onClick={() => setViewingZone({ zone: "exile", playerId: 0 })}
        />
      </div>

      {/* Opponent zones — upper-right: graveyard pile, library pile, exile badge */}
      <div className="fixed right-2 top-12 z-30 flex items-start gap-2">
        <GraveyardPile
          playerId={1}
          onClick={() => setViewingZone({ zone: "graveyard", playerId: 1 })}
        />
        <LibraryPile playerId={1} />
        <ZoneIndicator
          zone="exile"
          playerId={1}
          onClick={() => setViewingZone({ zone: "exile", playerId: 1 })}
        />
      </div>

      {/* Combat phase indicator near action button */}
      <div className="fixed bottom-36 right-4 z-30">
        <CombatPhaseIndicator />
      </div>

      {/* AI debug toggle + Concede */}
      <div className="fixed right-2 top-2 z-40 flex gap-2">
        {mode === "ai" && (
          <button
            onClick={() => setShowAiHand((v) => !v)}
            className="rounded bg-gray-800/80 px-2 py-1 text-xs text-gray-400 hover:text-gray-200"
          >
            {showAiHand ? "Hide AI Hand" : "Show AI Hand"}
          </button>
        )}
        <button
          onClick={() => {
            clearGame(gameId);
            navigate("/");
          }}
          className="rounded bg-gray-800/80 px-2 py-1 text-xs text-red-400 hover:text-red-300"
        >
          Concede
        </button>
      </div>

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

      {/* Block assignment lines (animated SVG overlay for combat) */}
      <BlockAssignmentLines />

      {/* Damage assignment review (read-only for v1.1) */}
      <DamageAssignmentModal />

      {/* Unified action button (combat + priority controls) */}
      <ActionButton />
      <div className="fixed bottom-20 right-4 z-30">
        <FullControlToggle />
      </div>

      {/* Card preview overlay */}
      <CardPreview cardName={inspectedCardName} />

      {/* WaitingFor-driven prompt overlays (only for human player) */}
      {waitingFor?.type === "TargetSelection" && waitingFor.data.player === PLAYER_ID && <TargetingOverlay />}
      {waitingFor?.type === "ManaPayment" && waitingFor.data.player === PLAYER_ID && <ManaPaymentUI />}
      {waitingFor?.type === "ReplacementChoice" && waitingFor.data.player === PLAYER_ID && <ReplacementModal />}

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
  const inspectObject = useUiStore((s) => s.inspectObject);

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
      <div className="relative z-10 w-[90vw] max-w-7xl rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-2 text-center text-lg font-bold text-white">
          Mulligan ({mulliganCount} cards)
        </h2>
        <p className="mb-4 text-center text-sm text-gray-400">
          Review your opening hand and choose to keep or mulligan.
        </p>

        <div className="mb-6 flex flex-wrap justify-center gap-3">
          {handObjects.map((obj) => (
            <div
              key={obj.id}
              className="rounded-lg border border-gray-700 bg-gray-800/60 p-1"
              onMouseEnter={() => inspectObject(obj.id)}
              onMouseLeave={() => inspectObject(null)}
            >
              <CardImage cardName={obj.name} size="normal" className="!w-[130px] !h-[182px]" />
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
  const inspectObject = useUiStore((s) => s.inspectObject);

  if (!player || !objects) return null;

  const handObjects = player.hand.map((id) => objects[id]).filter(Boolean);
  const isReady = selectedTargets.length === count;

  const handleConfirm = () => {
    onChoose(selectedTargets.join(","));
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/60" />
      <div className="relative z-10 w-[90vw] max-w-7xl rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
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
                onMouseEnter={() => inspectObject(obj.id)}
                onMouseLeave={() => inspectObject(null)}
                className={`rounded-lg p-1 text-sm transition ${
                  isSelected
                    ? "bg-cyan-600 text-white ring-2 ring-cyan-400"
                    : "bg-gray-800 text-gray-300 hover:bg-gray-700"
                }`}
              >
                <CardImage cardName={obj.name} size="normal" className="!w-[130px] !h-[182px]" />
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
  const navigate = useNavigate();
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" />
      <div className="relative z-10 rounded-xl bg-gray-900 p-8 text-center shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-2 text-2xl font-bold text-white">Game Over</h2>
        <p className="mb-4 text-lg text-gray-300">
          {winner != null
            ? winner === 0
              ? "You Win!"
              : "Opponent Wins"
            : "Draw"}
        </p>
        <button
          onClick={() => navigate("/")}
          className="rounded-lg bg-indigo-600 px-6 py-2 font-semibold text-white transition hover:bg-indigo-500"
        >
          Return to Menu
        </button>
      </div>
    </div>
  );
}
