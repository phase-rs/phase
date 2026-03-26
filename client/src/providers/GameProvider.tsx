import { createContext, useEffect, useRef, type ReactNode } from "react";

import type { FormatConfig, GameAction, MatchConfig } from "../adapter/types";
import { P2PHostAdapter, P2PGuestAdapter } from "../adapter/p2p-adapter";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter";
import { WasmAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import { audioManager } from "../audio/AudioManager";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { STORAGE_KEY_PREFIX, loadActiveDeck } from "../constants/storage";
import { getCachedFeed, listSubscriptions } from "../services/feedService";
import type { FeedDeck } from "../types/feed";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction, processRemoteUpdate } from "../game/dispatch";
import { hostRoom, joinRoom } from "../network/connection";
import { createPeerSession } from "../network/peer";
import type { ParsedDeck } from "../services/deckParser";
import { consumeRecentAutoUpdateMarker } from "../pwa/updateMarker";
import { ensureCardDatabase } from "../services/cardData";
import { detectServerUrl } from "../services/serverDetection";
import { useGameStore, loadGame, clearGame, clearActiveGame } from "../stores/gameStore";
import { useMultiplayerStore } from "../stores/multiplayerStore";

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
  return { main_deck: names, sideboard: sbNames, commander: deck.commander ?? [] };
}

function pickOpponentDeck(playerDeck: ParsedDeck, formatConfig?: FormatConfig): Array<{ name: string; count: number }> {
  // 1. Try format-specific feeds first (e.g., mtggoldfish-standard for Standard)
  if (formatConfig) {
    const formatKey = formatConfig.format.toLowerCase();
    const formatDecks: FeedDeck[] = [];
    for (const sub of listSubscriptions()) {
      const feed = getCachedFeed(sub.sourceId);
      if (feed?.format === formatKey) {
        formatDecks.push(...feed.decks);
      }
    }
    if (formatDecks.length > 0) {
      const playerNames = new Set(playerDeck.main.map((e) => e.name));
      const candidates = formatDecks.filter(
        (d) => !d.main.every((c) => playerNames.has(c.name)),
      );
      const pick = candidates.length > 0
        ? candidates[Math.floor(Math.random() * candidates.length)]
        : formatDecks[Math.floor(Math.random() * formatDecks.length)];
      return pick.main;
    }
  }

  // 2. Fall back to starter-decks feed
  const feed = getCachedFeed("starter-decks");
  const feedDecks = feed?.decks ?? [];

  if (feedDecks.length > 0) {
    const playerNames = new Set(playerDeck.main.map((e) => e.name));
    const candidates = feedDecks.filter(
      (d) => !d.main.every((c) => playerNames.has(c.name)),
    );
    const pick = candidates.length > 0
      ? candidates[Math.floor(Math.random() * candidates.length)]
      : feedDecks[Math.floor(Math.random() * feedDecks.length)];
    return pick.main;
  }

  // 3. Fallback: pick a random 60-card deck from localStorage
  const candidates: ParsedDeck[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key?.startsWith(STORAGE_KEY_PREFIX)) {
      try {
        const deck = JSON.parse(localStorage.getItem(key)!) as ParsedDeck;
        const cardCount = deck.main.reduce((s, e) => s + e.count, 0);
        if (cardCount >= 40 && cardCount <= 80 && !deck.commander?.length) {
          candidates.push(deck);
        }
      } catch { /* skip malformed */ }
    }
  }
  if (candidates.length > 0) {
    return candidates[Math.floor(Math.random() * candidates.length)].main;
  }

  // 4. Last resort: mirror the player's deck
  return playerDeck.main;
}

/** Build a DeckList (name-only) for the WASM engine to resolve. */
function buildDeckList(deck: ParsedDeck, formatConfig?: FormatConfig): {
  player: { main_deck: string[]; sideboard: string[]; commander: string[] };
  opponent: { main_deck: string[]; sideboard: string[]; commander: string[] };
  ai_decks: Array<{ main_deck: string[]; sideboard: string[]; commander: string[] }>;
} {
  const playerNames: string[] = [];
  for (const entry of deck.main) {
    for (let i = 0; i < entry.count; i++) {
      playerNames.push(entry.name);
    }
  }
  const playerSideboard: string[] = [];
  for (const entry of deck.sideboard) {
    for (let i = 0; i < entry.count; i++) {
      playerSideboard.push(entry.name);
    }
  }
  const opponentCards = pickOpponentDeck(deck, formatConfig);
  const opponentNames: string[] = [];
  for (const entry of opponentCards) {
    for (let i = 0; i < entry.count; i++) {
      opponentNames.push(entry.name);
    }
  }
  return {
    player: { main_deck: playerNames, sideboard: playerSideboard, commander: deck.commander ?? [] },
    opponent: { main_deck: opponentNames, sideboard: [], commander: [] },
    ai_decks: [],
  };
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
  formatConfig?: FormatConfig;
  playerCount?: number;
  matchConfig?: MatchConfig;
  onWsEvent?: (event: WsAdapterEvent) => void;
  onP2PEvent?: (event: P2PAdapterEvent) => void;
  onReady?: () => void;
  onCardDataMissing?: () => void;
  onNoDeck?: () => void;
  /** Called when a saved game could not be resumed and a fresh game was started instead. */
  onResumeReset?: (reason: string) => void;
  children: ReactNode;
}

export function GameProvider({
  gameId,
  mode,
  difficulty,
  joinCode,
  formatConfig,
  playerCount,
  matchConfig,
  onWsEvent,
  onP2PEvent,
  onReady,
  onCardDataMissing,
  onNoDeck,
  onResumeReset,
  children,
}: GameProviderProps) {
  // Refs for callback props — these are notifications that should never
  // cause the game setup effect to re-run.
  const onWsEventRef = useRef(onWsEvent);
  const onP2PEventRef = useRef(onP2PEvent);
  const onReadyRef = useRef(onReady);
  const onCardDataMissingRef = useRef(onCardDataMissing);
  const onNoDeckRef = useRef(onNoDeck);
  const onResumeResetRef = useRef(onResumeReset);
  onWsEventRef.current = onWsEvent;
  onP2PEventRef.current = onP2PEvent;
  onReadyRef.current = onReady;
  onCardDataMissingRef.current = onCardDataMissing;
  onNoDeckRef.current = onNoDeck;
  onResumeResetRef.current = onResumeReset;

  useEffect(() => {
    const { initGame, resumeGame, reset } = useGameStore.getState();

    const isOnline = mode === "online";
    const isP2P = mode === "p2p-host" || mode === "p2p-join";
    const hasSession = localStorage.getItem("phase-ws-session") !== null;
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

      const deckList = buildDeckList(parsedDeck, formatConfig);

      const setupP2P = async () => {
        if (cancelled) return;

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
            const adapter = new P2PHostAdapter(deckList, session);

            p2pUnsubscribe = adapter.onEvent((event) => {
              if (event.type === "stateChanged") {
                processRemoteUpdate(event.state, event.events, event.legalActions);
              }
              onP2PEventRef.current?.(event);
            });

            await initGame(gameId, adapter, undefined, undefined, undefined, matchConfig);
            if (cancelled) return;
            controller = createGameLoopController({ mode: "online" });
            controller.start();
            onReadyRef.current?.();
            audioManager.setContext("battlefield");
          } else {
            // p2p-join
            const code = joinCode!;
            const { conn, destroyPeer } = await joinRoom(code);
            if (cancelled) { destroyPeer(); return; }

            const session = createPeerSession(conn, destroyPeer);
            const adapter = new P2PGuestAdapter(deckList, session);

            p2pUnsubscribe = adapter.onEvent((event) => {
              if (event.type === "stateChanged") {
                processRemoteUpdate(event.state, event.events, event.legalActions);
              }
              onP2PEventRef.current?.(event);
            });

            await initGame(gameId, adapter, undefined, undefined, undefined, matchConfig);
            if (cancelled) return;
            controller = createGameLoopController({ mode: "online" });
            controller.start();
            onReadyRef.current?.();
            audioManager.setContext("battlefield");
          }
        } catch (err) {
          if (cancelled) return;
          const message = err instanceof Error ? err.message : String(err);
          onP2PEventRef.current?.({ type: "error", message });
        }
      };

      setupP2P();

      return () => {
        cancelled = true;
        if (controller) controller.dispose();
        if (p2pUnsubscribe) p2pUnsubscribe();
        if (p2pHostDestroy) p2pHostDestroy();
        audioManager.setContext("menu");
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
            // Ensure adapter is set before animating so state updates land correctly
            const needAdapter = !useGameStore.getState().adapter && wsAdapter;
            if (needAdapter) {
              useGameStore.setState({ adapter: wsAdapter });
            }
            processRemoteUpdate(event.state, event.events, event.legalActions);
            useMultiplayerStore.getState().setConnectionStatus("connected");
            if (
              event.state.match_phase === "Completed"
              || (!event.state.match_phase && event.state.waiting_for.type === "GameOver")
            ) {
              clearActiveGame();
            }
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
            audioManager.setContext("battlefield");
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
          initGame(gameId, wsAdapter, undefined, undefined, undefined, matchConfig).then(() => {
            if (cancelled) return;
            useMultiplayerStore.getState().setConnectionStatus("connected");
            onReadyRef.current?.();
            audioManager.setContext("battlefield");
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
        audioManager.setContext("menu");
        reset();
      };
    }

    // AI or local mode — async setup (loadGame is async due to IndexedDB)
    const setupLocal = async () => {
      if (cancelled) return;

      const savedState = await loadGame(gameId);
      const adapter = new WasmAdapter();

      if (savedState) {
        try {
          // Load card DB before restore so the engine can rehydrate objects
          // and handle token creation / effects after resume.
          await ensureCardDatabase().catch(() => {/* card DB is best-effort */});
          if (cancelled) return;
          await resumeGame(gameId, adapter, savedState);
          if (cancelled) return;
          controller = createGameLoopController({ mode, difficulty, playerCount });
          controller.start();
          audioManager.setContext("battlefield");
        } catch (err) {
          // Saved state is incompatible (e.g. engine type changes) — clear it
          // and fall through to start a fresh game.
          if (cancelled) return;
          console.warn("Failed to resume saved game, starting fresh:", err);
          const wasAutoUpdate = consumeRecentAutoUpdateMarker();
          const reason = wasAutoUpdate
            ? "The app was updated and your saved game is incompatible with the new version."
            : `Could not restore saved game: ${err instanceof Error ? err.message : String(err)}`;
          onResumeResetRef.current?.(reason);
          clearGame(gameId);
          const freshAdapter = new WasmAdapter();
          const parsedDeck = loadActiveDeck();
          if (!parsedDeck) {
            onNoDeckRef.current?.();
            return;
          }
          const deckList = buildDeckList(parsedDeck, formatConfig);
          try {
            await initGame(gameId, freshAdapter, deckList, formatConfig, playerCount, matchConfig);
            if (cancelled) return;
            if (!freshAdapter.cardDbLoaded) {
              onCardDataMissingRef.current?.();
            }
            controller = createGameLoopController({ mode, difficulty, playerCount });
            controller.start();
            audioManager.setContext("battlefield");
          } catch (initErr) {
            console.error("Deck validation failed:", initErr);
            if (!cancelled) onNoDeckRef.current?.();
          }
        }
        return;
      }

      // No saved state — start a new game
      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        onNoDeckRef.current?.();
        return;
      }

      const deckList = buildDeckList(parsedDeck, formatConfig);
      try {
        await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig);
        if (cancelled) return;
        if (!adapter.cardDbLoaded) {
          onCardDataMissingRef.current?.();
        }
        controller = createGameLoopController({ mode, difficulty, playerCount });
        controller.start();
        audioManager.setContext("battlefield");
      } catch (err) {
        console.error("Deck validation failed:", err);
        if (!cancelled) onNoDeckRef.current?.();
      }
    };

    setupLocal();

    return () => {
      cancelled = true;
      if (controller) controller.dispose();
      audioManager.setContext("menu");
      reset();
    };
  }, [gameId, mode, difficulty, joinCode, formatConfig, playerCount, matchConfig]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}
