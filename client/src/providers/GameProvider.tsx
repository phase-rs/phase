import { createContext, useContext, useEffect, useRef, type ReactNode } from "react";

import type { GameAction } from "../adapter/types";
import { WasmAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import { audioManager } from "../audio/AudioManager";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction } from "../game/dispatch";
import type { ParsedDeck } from "../services/deckParser";
import { useGameStore, loadGame } from "../stores/gameStore";

const DEFAULT_WS_URL = "ws://localhost:8080/ws";

function getWsUrl(): string {
  return import.meta.env.VITE_WS_URL ?? DEFAULT_WS_URL;
}

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
  mode: "ai" | "online" | "local";
  difficulty?: string;
  joinCode?: string;
  onWsEvent?: (event: WsAdapterEvent) => void;
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
  onReady,
  onCardDataMissing,
  onNoDeck,
  children,
}: GameProviderProps) {
  // Refs for callback props — these are notifications that should never
  // cause the game setup effect to re-run.
  const onWsEventRef = useRef(onWsEvent);
  const onReadyRef = useRef(onReady);
  const onCardDataMissingRef = useRef(onCardDataMissing);
  const onNoDeckRef = useRef(onNoDeck);
  onWsEventRef.current = onWsEvent;
  onReadyRef.current = onReady;
  onCardDataMissingRef.current = onCardDataMissing;
  onNoDeckRef.current = onNoDeck;

  useEffect(() => {
    const { initGame, resumeGame, reset } = useGameStore.getState();

    const isOnline = mode === "online";
    const hasSession = sessionStorage.getItem("phase-ws-session") !== null;
    const isReconnect = isOnline && !joinCode && hasSession;

    let cancelled = false;
    let wsUnsubscribe: (() => void) | null = null;
    let controller: ReturnType<typeof createGameLoopController> | null = null;

    if (isOnline || isReconnect) {
      const parsedDeck = loadActiveDeck();
      const deck = parsedDeck
        ? parsedDeckToDeckData(parsedDeck)
        : { main_deck: [], sideboard: [] };

      const wsMode = joinCode ? "join" : "host";
      const wsAdapter = new WebSocketAdapter(
        getWsUrl(),
        wsMode,
        deck,
        wsMode === "join" ? joinCode : undefined,
      );

      wsUnsubscribe = wsAdapter.onEvent((event) => {
        if (event.type === "stateChanged") {
          const store = useGameStore.getState();
          store.setGameState(event.state);
          store.setWaitingFor(event.state.waiting_for);
        }
        onWsEventRef.current?.(event);
      });

      if (isReconnect) {
        wsAdapter.tryReconnect();
      } else {
        initGame(gameId, wsAdapter).then(() => {
          if (cancelled) return;
          onReadyRef.current?.();
          audioManager.startMusic();
        });
      }

      return () => {
        cancelled = true;
        if (wsUnsubscribe) wsUnsubscribe();
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
