import { createContext, useContext, useEffect, useRef, type ReactNode } from "react";

import type { GameAction } from "../adapter/types";
import { P2PHostAdapter, P2PGuestAdapter } from "../adapter/p2p-adapter";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter";
import { WasmAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import { audioManager } from "../audio/AudioManager";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction } from "../game/dispatch";
import { hostRoom, joinRoom } from "../network/connection";
import { createPeerSession } from "../network/peer";
import type { ParsedDeck } from "../services/deckParser";
import { detectServerUrl } from "../services/serverDetection";
import { useGameStore, loadGame } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";

function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + activeName);
  if (!raw) return null;
  return JSON.parse(raw) as ParsedDeck;
}

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

function resolveEntries(
  deckCards: Array<{ name: string; count: number }>,
  cardDb: Record<string, CardFace>,
): Array<{ card: CardFace; count: number }> {
  const entries: Array<{ card: CardFace; count: number }> = [];
  for (const entry of deckCards) {
    const card = cardDb[entry.name];
    if (card) {
      entries.push({ card, count: entry.count });
    } else {
      console.warn(`Card not found in card-data.json: ${entry.name}`);
    }
  }
  return entries;
}

function pickOpponentDeck(playerDeck: ParsedDeck): Array<{ name: string; count: number }> {
  const playerNames = new Set(playerDeck.main.map((e) => e.name));
  const candidates = STARTER_DECKS.filter(
    (s) => !s.cards.every((c) => playerNames.has(c.name)),
  );
  const pick = candidates.length > 0
    ? candidates[Math.floor(Math.random() * candidates.length)]
    : STARTER_DECKS[Math.floor(Math.random() * STARTER_DECKS.length)];
  return pick.cards;
}

async function buildDeckPayload(deck: ParsedDeck): Promise<DeckPayload | null> {
  try {
    const resp = await fetch("/card-data.json");
    if (!resp.ok) {
      console.warn("card-data.json not available (HTTP", resp.status, "). Starting with empty game.");
      return null;
    }
    const cardDb = (await resp.json()) as Record<string, CardFace>;

    const playerEntries = resolveEntries(deck.main, cardDb);
    const opponentCards = pickOpponentDeck(deck);
    const opponentEntries = resolveEntries(opponentCards, cardDb);

    return { player_deck: playerEntries, opponent_deck: opponentEntries };
  } catch (err) {
    console.warn("Failed to load card-data.json:", err);
    return null;
  }
}

const GameDispatchContext = createContext<(action: GameAction) => Promise<void>>(
  () => {
    throw new Error("No GameProvider found in component tree");
  },
);

export interface GameProviderProps {
  gameId: string;
  mode: "ai" | "online" | "local" | "p2p-host" | "p2p-join";
  difficulty?: string;
  joinCode?: string;
  onWsEvent?: (event: WsAdapterEvent) => void;
  onP2PEvent?: (event: P2PAdapterEvent) => void;
  onReady?: () => void;
  onCardDataMissing?: () => void;
  onNoDeck?: () => void;
  children: ReactNode;
}

export function GameProvider({
  gameId,
  mode,
  difficulty,
  joinCode,
  onWsEvent,
  onP2PEvent,
  onReady,
  onCardDataMissing,
  onNoDeck,
  children,
}: GameProviderProps) {
  // Refs for callback props — these are notifications that should never
  // cause the game setup effect to re-run.
  const onWsEventRef = useRef(onWsEvent);
  const onP2PEventRef = useRef(onP2PEvent);
  const onReadyRef = useRef(onReady);
  const onCardDataMissingRef = useRef(onCardDataMissing);
  const onNoDeckRef = useRef(onNoDeck);
  onWsEventRef.current = onWsEvent;
  onP2PEventRef.current = onP2PEvent;
  onReadyRef.current = onReady;
  onCardDataMissingRef.current = onCardDataMissing;
  onNoDeckRef.current = onNoDeck;

  useEffect(() => {
    const { initGame, resumeGame, reset } = useGameStore.getState();

    const isOnline = mode === "online";
    const isP2P = mode === "p2p-host" || mode === "p2p-join";
    const hasSession = sessionStorage.getItem("phase-ws-session") !== null;
    const isReconnect = isOnline && !joinCode && hasSession;

    let cancelled = false;
    let wsUnsubscribe: (() => void) | null = null;
    let p2pUnsubscribe: (() => void) | null = null;
    let p2pHostDestroy: (() => void) | null = null;
    let controller: ReturnType<typeof createGameLoopController> | null = null;

    if (isP2P) {
      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        onNoDeckRef.current?.();
        return;
      }

      buildDeckPayload(parsedDeck).then(async (deckPayload) => {
        if (cancelled) return;

        if (deckPayload === null) {
          onCardDataMissingRef.current?.();
        }

        try {
          if (mode === "p2p-host") {
            const host = hostRoom();
            p2pHostDestroy = host.destroy;

            onP2PEventRef.current?.({ type: "roomCreated", roomCode: host.roomCode });
            onP2PEventRef.current?.({ type: "waitingForGuest" });

            const { conn, destroyPeer } = await host.waitForGuest();
            if (cancelled) { destroyPeer(); return; }

            onP2PEventRef.current?.({ type: "guestConnected" });

            const session = createPeerSession(conn, destroyPeer);
            const adapter = new P2PHostAdapter(deckPayload, session);

            p2pUnsubscribe = adapter.onEvent((event) => {
              if (event.type === "stateChanged") {
                useGameStore.setState({
                  gameState: event.state,
                  waitingFor: event.state.waiting_for,
                });
              }
              onP2PEventRef.current?.(event);
            });

            await initGame(gameId, adapter);
            if (cancelled) return;
            controller = createGameLoopController({ mode: "online" });
            controller.start();
            onReadyRef.current?.();
            audioManager.startMusic();
          } else {
            // p2p-join
            const code = joinCode!;
            const { conn, destroyPeer } = await joinRoom(code);
            if (cancelled) { destroyPeer(); return; }

            const session = createPeerSession(conn, destroyPeer);
            const adapter = new P2PGuestAdapter(deckPayload, session);

            p2pUnsubscribe = adapter.onEvent((event) => {
              if (event.type === "stateChanged") {
                useGameStore.setState({
                  gameState: event.state,
                  waitingFor: event.state.waiting_for,
                });
              }
              onP2PEventRef.current?.(event);
            });

            await initGame(gameId, adapter);
            if (cancelled) return;
            controller = createGameLoopController({ mode: "online" });
            controller.start();
            onReadyRef.current?.();
            audioManager.startMusic();
          }
        } catch (err) {
          if (cancelled) return;
          const message = err instanceof Error ? err.message : String(err);
          onP2PEventRef.current?.({ type: "error", message });
        }
      });

      return () => {
        cancelled = true;
        if (controller) controller.dispose();
        if (p2pUnsubscribe) p2pUnsubscribe();
        if (p2pHostDestroy) p2pHostDestroy();
        audioManager.stopMusic(0);
        reset();
      };
    }

    if (isOnline || isReconnect) {
      const parsedDeck = loadActiveDeck();
      const deck = parsedDeck
        ? parsedDeckToDeckData(parsedDeck)
        : { main_deck: [], sideboard: [] };

      const mpStore = useMultiplayerStore.getState();
      mpStore.setConnectionStatus("connecting");

      const wsMode = joinCode ? "join" : "host";

      // Track adapter for cleanup (needed for StrictMode double-mount)
      let wsAdapter: WebSocketAdapter | null = null;

      // Extract password from URL search params
      const urlParams = new URLSearchParams(window.location.search);
      const password = urlParams.get("password") ?? undefined;

      // Use smart server detection for initial connection
      const setupWs = async () => {
        if (cancelled) return;
        const serverUrl = import.meta.env.VITE_WS_URL ?? await detectServerUrl();
        if (cancelled) return;

        wsAdapter = new WebSocketAdapter(
          serverUrl,
          wsMode,
          deck,
          wsMode === "join" ? joinCode : undefined,
          wsMode === "join" ? password : undefined,
        );

        wsUnsubscribe = wsAdapter.onEvent((event) => {
          if (event.type === "stateChanged") {
            // Batch all state updates atomically so the auto-pass controller
            // sees consistent waitingFor + legalActions in a single subscription tick.
            const needAdapter = !useGameStore.getState().adapter && wsAdapter;
            useGameStore.setState({
              gameState: event.state,
              waitingFor: event.state.waiting_for,
              legalActions: event.legalActions,
              ...(needAdapter ? { adapter: wsAdapter } : {}),
            });
            useMultiplayerStore.getState().setConnectionStatus("connected");
          }
          if (event.type === "error" || event.type === "reconnectFailed") {
            useMultiplayerStore.getState().setConnectionStatus("disconnected");
            useMultiplayerStore.getState().showToast("Connection failed. Retry or change server in Settings.");
          }
          if (event.type === "reconnecting") {
            useMultiplayerStore.getState().setConnectionStatus("connecting");
          }
          if (event.type === "reconnected") {
            useMultiplayerStore.getState().setConnectionStatus("connected");
            onReadyRef.current?.();
            audioManager.startMusic();
          }
          onWsEventRef.current?.(event);
        });

        // Start auto-pass controller for multiplayer (safe before game state
        // exists — onWaitingForChanged returns early when waitingFor is null)
        controller = createGameLoopController({ mode: "online" });
        controller.start();

        if (isReconnect) {
          wsAdapter.tryReconnect();
        } else {
          initGame(gameId, wsAdapter).then(() => {
            if (cancelled) return;
            useMultiplayerStore.getState().setConnectionStatus("connected");
            onReadyRef.current?.();
            audioManager.startMusic();
          }).catch(() => {
            useMultiplayerStore.getState().setConnectionStatus("disconnected");
            useMultiplayerStore.getState().showToast("Connection failed. Retry or change server in Settings.");
          });
        }
      };

      setupWs();

      return () => {
        cancelled = true;
        if (controller) controller.dispose();
        if (wsUnsubscribe) wsUnsubscribe();
        if (wsAdapter) wsAdapter.dispose();
        useMultiplayerStore.getState().setConnectionStatus("disconnected");
        audioManager.stopMusic(0);
        reset();
      };
    }

    // AI or local mode — check for a saved game for this ID
    const savedState = loadGame(gameId);
    const adapter = new WasmAdapter();

    if (savedState) {
      resumeGame(gameId, adapter, savedState).then(() => {
        if (cancelled) return;
        controller = createGameLoopController({ mode, difficulty });
        controller.start();
        audioManager.startMusic();
      });
      return () => {
        cancelled = true;
        if (controller) controller.dispose();
        audioManager.stopMusic(0);
        reset();
      };
    }

    // No saved state — start a new game
    const parsedDeck = loadActiveDeck();
    if (!parsedDeck) {
      onNoDeckRef.current?.();
      return;
    }

    buildDeckPayload(parsedDeck).then((deckPayload) => {
      if (cancelled) return;

      if (deckPayload === null) {
        onCardDataMissingRef.current?.();
      }

      initGame(gameId, adapter, deckPayload).then(() => {
        if (cancelled) return;

        controller = createGameLoopController({ mode, difficulty });
        controller.start();
        audioManager.startMusic();
      });
    });

    return () => {
      cancelled = true;
      if (controller) {
        controller.dispose();
      }
      audioManager.stopMusic(0);
      reset();
    };
  }, [gameId, mode, difficulty, joinCode]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}

export function useDispatch(): (action: GameAction) => Promise<void> {
  return useContext(GameDispatchContext);
}
