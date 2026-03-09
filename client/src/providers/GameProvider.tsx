import { createContext, useContext, useEffect, type ReactNode } from "react";

import type { GameAction } from "../adapter/types";
import { WasmAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction } from "../game/dispatch";
import type { ParsedDeck } from "../services/deckParser";
import { useGameStore } from "../stores/gameStore";

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

    return { player_deck: entries, opponent_deck: entries };
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
  mode,
  difficulty,
  joinCode,
  onWsEvent,
  onReady,
  onCardDataMissing,
  onNoDeck,
  children,
}: GameProviderProps) {
  const initGame = useGameStore((s) => s.initGame);
  const reset = useGameStore((s) => s.reset);

  useEffect(() => {
    const isOnline = mode === "online";
    const hasSession = sessionStorage.getItem("forge-ws-session") !== null;
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

      if (onWsEvent) {
        wsUnsubscribe = wsAdapter.onEvent(onWsEvent);
      }

      if (isReconnect) {
        wsAdapter.tryReconnect();
      } else {
        initGame(wsAdapter).then(() => {
          if (cancelled) return;
          onReady?.();
        });
      }

      return () => {
        cancelled = true;
        if (wsUnsubscribe) wsUnsubscribe();
        reset();
      };
    }

    // AI or local mode
    const parsedDeck = loadActiveDeck();
    if (!parsedDeck) {
      onNoDeck?.();
      return;
    }

    const adapter = new WasmAdapter();

    buildDeckPayload(parsedDeck).then((deckPayload) => {
      if (cancelled) return;

      if (deckPayload === null) {
        onCardDataMissing?.();
      }

      initGame(adapter, deckPayload).then(() => {
        if (cancelled) return;

        controller = createGameLoopController({ mode, difficulty });
        controller.start();
      });
    });

    return () => {
      cancelled = true;
      if (controller) {
        controller.dispose();
      }
      reset();
    };
  }, [initGame, reset, mode, difficulty, joinCode, onWsEvent, onReady, onCardDataMissing, onNoDeck]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}

export function useDispatch(): (action: GameAction) => Promise<void> {
  return useContext(GameDispatchContext);
}
