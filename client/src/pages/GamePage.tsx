import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";

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
import { CardDataMissingModal } from "../components/modal/CardDataMissingModal.tsx";
import { ChoiceModal } from "../components/modal/ChoiceModal.tsx";
import { ReplacementModal } from "../components/modal/ReplacementModal.tsx";
import { StackDisplay } from "../components/stack/StackDisplay.tsx";
import { TargetingOverlay } from "../components/targeting/TargetingOverlay.tsx";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage.ts";
import { WasmAdapter } from "../adapter/wasm-adapter.ts";
import { WebSocketAdapter } from "../adapter/ws-adapter.ts";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter.ts";
import type { ParsedDeck } from "../services/deckParser.ts";
import { useGameDispatch } from "../hooks/useGameDispatch.ts";
import { useKeyboardShortcuts } from "../hooks/useKeyboardShortcuts.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";
import { createAIController } from "../game/controllers/aiController.ts";
import type { AIController } from "../game/controllers/aiController.ts";

const DEFAULT_WS_URL = "ws://localhost:8080/ws";

function getWsUrl(): string {
  return import.meta.env.VITE_WS_URL ?? DEFAULT_WS_URL;
}

/** Load active deck from localStorage using shared storage constants. */
function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + activeName);
  if (!raw) return null;
  return JSON.parse(raw) as ParsedDeck;
}

/** Convert ParsedDeck to DeckData format for WebSocket adapter. */
function parsedDeckToDeckData(deck: ParsedDeck): DeckData {
  const names: string[] = [];
  for (const entry of deck.main) {
    for (let i = 0; i < entry.count; i++) {
      names.push(entry.name);
    }
  }
  const sbNames: string[] = [];
  for (const entry of deck.sideboard) {
    for (let i = 0; i < entry.count; i++) {
      sbNames.push(entry.name);
    }
  }
  return { main_deck: names, sideboard: sbNames };
}

interface CardFace {
  name: string;
  [key: string]: unknown;
}

interface DeckPayload {
  player_deck: Array<{ card: CardFace; count: number }>;
  opponent_deck: Array<{ card: CardFace; count: number }>;
}

/** Fetch card-data.json and build DeckPayload for WASM. */
async function buildDeckPayload(deck: ParsedDeck): Promise<DeckPayload | null> {
  try {
    const resp = await fetch("/card-data.json");
    if (!resp.ok) {
      console.warn("card-data.json not available (HTTP", resp.status, "). Starting with empty game.");
      return null;
    }
    const cardDb = (await resp.json()) as Record<string, CardFace>;

    const entries: Array<{ card: CardFace; count: number }> = [];
    for (const entry of deck.main) {
      const card = cardDb[entry.name];
      if (card) {
        entries.push({ card, count: entry.count });
      } else {
        console.warn(`Card not found in card-data.json: ${entry.name}`);
      }
    }

    // Mirror match: opponent uses the same deck
    return { player_deck: entries, opponent_deck: entries };
  } catch (err) {
    console.warn("Failed to load card-data.json:", err);
    return null;
  }
}

export function GamePage() {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const mode = searchParams.get("mode");
  const difficulty = searchParams.get("difficulty") ?? "Medium";
  const joinCode = searchParams.get("code") ?? "";

  const initGame = useGameStore((s) => s.initGame);
  const gameState = useGameStore((s) => s.gameState);
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();
  const reset = useGameStore((s) => s.reset);
  const inspectedObjectId = useUiStore((s) => s.inspectedObjectId);
  const objects = gameState?.objects;
  const aiControllerRef = useRef<AIController | null>(null);
  const [showAiHand, setShowAiHand] = useState(false);
  const [showCardDataMissing, setShowCardDataMissing] = useState(false);

  // Online multiplayer state
  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [waitingForOpponent, setWaitingForOpponent] = useState(false);
  const [opponentDisconnected, setOpponentDisconnected] = useState(false);
  const [disconnectGrace, setDisconnectGrace] = useState(0);

  const inspectedCardName =
    inspectedObjectId != null && objects
      ? (objects[inspectedObjectId]?.name ?? null)
      : null;

  useKeyboardShortcuts();

  useEffect(() => {
    const isOnline = mode === "host" || mode === "join";

    if (isOnline) {
      const parsedDeck = loadActiveDeck();
      const deck = parsedDeck
        ? parsedDeckToDeckData(parsedDeck)
        : { main_deck: [], sideboard: [] };
      const wsAdapter = new WebSocketAdapter(
        getWsUrl(),
        mode as "host" | "join",
        deck,
        mode === "join" ? joinCode : undefined,
      );

      const unsubscribe = wsAdapter.onEvent((event: WsAdapterEvent) => {
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
        }
      });

      initGame(wsAdapter).then(() => {
        setWaitingForOpponent(false);
      });

      return () => {
        unsubscribe();
        reset();
      };
    }

    // AI or default mode: require active deck
    const parsedDeck = loadActiveDeck();
    if (!parsedDeck) {
      navigate("/");
      return;
    }

    let cancelled = false;
    const adapter = new WasmAdapter();

    buildDeckPayload(parsedDeck).then((deckPayload) => {
      if (cancelled) return;

      if (deckPayload === null) {
        setShowCardDataMissing(true);
      }

      initGame(adapter, deckPayload).then(() => {
        if (cancelled) return;

        if (mode === "ai") {
          const controller = createAIController(
            () => useGameStore.getState().gameState,
            async (action) => {
              await useGameStore.getState().dispatch(action);
            },
            { difficulty },
          );
          aiControllerRef.current = controller;
          controller.start();
        }
      });
    });

    return () => {
      cancelled = true;
      if (aiControllerRef.current) {
        aiControllerRef.current.dispose();
        aiControllerRef.current = null;
      }
      reset();
    };
  }, [initGame, reset, mode, difficulty, joinCode, navigate]);

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
              Connecting to game {joinCode}
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
        <CardDataMissingModal onContinue={() => setShowCardDataMissing(false)} />
      )}

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
