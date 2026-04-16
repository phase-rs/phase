import { createContext, useEffect, useRef, type ReactNode } from "react";

import type { FormatConfig, GameAction, MatchConfig } from "../adapter/types";
import { P2PHostAdapter, P2PGuestAdapter } from "../adapter/p2p-adapter";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter";
import { WasmAdapter, getSharedAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import { audioManager } from "../audio/AudioManager";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { STORAGE_KEY_PREFIX, loadActiveDeck } from "../constants/storage";
import { getCachedFeed, listSubscriptions } from "../services/feedService";
import type { FeedDeck } from "../types/feed";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction, processRemoteUpdate } from "../game/dispatch";
import { hostRoom, joinRoom } from "../network/connection";
import type { BrokerClient } from "../services/brokerClient";
import { loadP2PSession } from "../services/p2pSession";
import type { ParsedDeck } from "../services/deckParser";
import { consumeRecentAutoUpdateMarker } from "../pwa/updateMarker";
import { ensureCardDatabase } from "../services/cardData";
import { clearWsSession, loadWsSession, saveWsSession } from "../services/multiplayerSession";
import { detectServerUrl } from "../services/serverDetection";
import {
  clearGame,
  clearActiveGame,
  clearP2PHostSession,
  loadGame,
  loadP2PHostSession,
  saveActiveGame,
  useGameStore,
} from "../stores/gameStore";
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

// Deferred store reset: cleanup schedules the store clear on a macrotask so that
// an immediate remount (StrictMode double-mount, or any dep-change re-run) can
// cancel it before it fires. Without this, every cleanup briefly sets
// gameState to null and GameBoard flashes "Waiting for game..." before the
// next initGame/resumeGame repopulates the store.
let pendingStoreReset: ReturnType<typeof setTimeout> | null = null;

function cancelPendingStoreReset(): void {
  if (pendingStoreReset !== null) {
    clearTimeout(pendingStoreReset);
    pendingStoreReset = null;
  }
}

function scheduleStoreReset(reset: () => void): void {
  cancelPendingStoreReset();
  pendingStoreReset = setTimeout(() => {
    pendingStoreReset = null;
    reset();
  }, 0);
}

export interface GameProviderProps {
  gameId: string;
  mode: "ai" | "online" | "local" | "p2p-host" | "p2p-join";
  difficulty?: string;
  joinCode?: string;
  formatConfig?: FormatConfig;
  playerCount?: number;
  matchConfig?: MatchConfig;
  /** CR 103.1: 0 = human plays first, 1 = opponent plays first, undefined = random. */
  firstPlayer?: number;
  /**
   * When `mode === "p2p-host"`, whether to register the room with a
   * lobby-only broker so it appears in the public listing. `false` hosts
   * a pure-PeerJS room (room code shared out-of-band). Ignored outside
   * the P2P host flow.
   */
  useBroker?: boolean;
  roomName?: string;
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
  firstPlayer,
  useBroker = false,
  roomName,
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
    // A prior cleanup may have deferred a store reset. Cancel it — this mount
    // is about to populate the store via initGame/resumeGame, and a fire from
    // the previous cleanup would null out the state we just wrote.
    cancelPendingStoreReset();

    const { initGame, resumeGame, resumeP2PHost, reset, setGameMode } = useGameStore.getState();
    setGameMode(mode);

    const isOnline = mode === "online";
    const isP2P = mode === "p2p-host" || mode === "p2p-join";
    if (!isOnline && !isP2P) {
      useMultiplayerStore.setState({ playerNames: new Map() });
    }
    const hasSession = loadWsSession() !== null;
    const isReconnect = isOnline && !joinCode && hasSession;

    // AbortController threaded through the P2P setup pipeline (below).
    // Component unmount calls `ac.abort()` in the cleanup; each `await`
    // inside `setupP2P` rechecks via `signal.throwIfAborted()`, so
    // teardown converges on a single `catch` regardless of which step was
    // in flight when the user navigated away.
    //
    // The non-P2P branches (AI, online, local) retain the `cancelled`
    // flag pattern — migrating them to AbortController is out of scope
    // for this change and carries regression risk in flows that work.
    // `cancelled` is declared inside those branches; the P2P branch uses
    // `signal.aborted` exclusively.
    const ac = new AbortController();
    const { signal } = ac;

    let wsUnsubscribe: (() => void) | null = null;
    let p2pUnsubscribe: (() => void) | null = null;
    // Per plan §4 "Peer ownership": the adapter's `dispose()` is the SOLE
    // caller of `hostPeer.destroy()` / guest `peer.destroy()`. GameProvider
    // holds only the adapter reference and calls `dispose()` on unmount;
    // direct `peer.destroy()` calls would double-destroy and also skip the
    // per-session cleanup that `dispose()` performs.
    let p2pAdapter: P2PHostAdapter | P2PGuestAdapter | null = null;
    let controller: ReturnType<typeof createGameLoopController> | null = null;

    if (isP2P) {
      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        onNoDeckRef.current?.();
        return;
      }

      const deckList = buildDeckList(parsedDeck, formatConfig);

      const wireP2PEvents = (adapter: P2PHostAdapter | P2PGuestAdapter) => {
        p2pUnsubscribe = adapter.onEvent((event) => {
          if (event.type === "playerIdentity") {
            useMultiplayerStore.getState().setActivePlayerId(event.playerId);
            if (event.playerNames) {
              const names = new Map(Object.entries(event.playerNames).map(
                ([k, v]) => [Number(k), v] as [number, string],
              ));
              useMultiplayerStore.setState({ playerNames: names });
            }
          }
          if (event.type === "stateChanged") {
            processRemoteUpdate(event.state, event.events, event.legalResult);
          }
          onP2PEventRef.current?.(event);
        });
      };

      const setupP2P = async () => {
        const effectivePlayerCount = playerCount ?? 2;

        // Resources that may need undoing on abort/error. `broker` is
        // closed unconditionally when set; `serverGameCode` gates the
        // compensating `unregister` call — we only un-do a registration
        // that actually landed.
        let broker: BrokerClient | null = null;
        let serverGameCode: string | null = null;
        let hostPeerHandle: { destroy: () => void } | null = null;

        try {
          if (mode === "p2p-host") {
            const activeHost = useMultiplayerStore.getState().getActiveP2PHost();
            if (activeHost?.gameId === gameId) {
              const adapter = activeHost.adapter;
              p2pAdapter = adapter;
              wireP2PEvents(adapter);
              await resumeP2PHost(gameId, adapter);
              signal.throwIfAborted();
            } else {
            // Resume detection: if both the engine state and the P2P
            // host session were persisted for this gameId, the host
            // crashed/reloaded mid-game and should dial back in on the
            // same room code so returning guests (whose IDB tokens are
            // keyed on `phase-<roomCode>`) still match. Partial state
            // (only one record present) is treated as inconsistent:
            // clear both and fall through to a fresh game.
            const [savedState, savedSession] = await Promise.all([
              loadGame(gameId),
              loadP2PHostSession(gameId),
            ]);
            signal.throwIfAborted();

            const isResume =
              savedState !== null && savedSession !== null && savedSession.gameStarted;
            if ((savedState !== null) !== (savedSession !== null)) {
              // Inconsistent: one record present, the other missing.
              // Drop both so the menu's Resume button doesn't re-offer.
              await clearGame(gameId);
              await clearP2PHostSession(gameId);
            }

            // Only open a fresh broker client when starting a fresh
            // game. Resume deliberately skips broker re-registration:
            // resume requires `savedSession.gameStarted`, and once the
            // game has started `handleNewGuest` rejects every new joiner
            // ("Game already in progress"). A re-registered lobby entry
            // would advertise a room that rejects its own click-throughs,
            // which is worse than letting the original entry expire via
            // the broker's 5-min TTL. Returning guests dial the host
            // directly via their cached peer-id + token; they never go
            // through the lobby list for reconnect.
            const host = await hostRoom(signal, {
              preferredRoomCode: isResume ? savedSession.roomCode : undefined,
            });
            // Before the adapter takes ownership of the Peer, `host.destroy`
            // is the only way to tear it down; once the adapter owns it,
            // `adapter.dispose()` is the sole teardown path.
            hostPeerHandle = host;
            signal.throwIfAborted();

            if (useBroker && !isResume) {
              const store = useMultiplayerStore.getState();
              const result = await store.openBroker({
                hostPeerId: host.peer.id,
                deck: deckList.player,
                displayName: store.displayName || "Host",
                public: true,
                password: null,
                timerSeconds: null,
                playerCount: effectivePlayerCount,
                matchConfig: matchConfig ?? { match_type: "Bo1" },
                formatConfig: formatConfig ?? null,
                aiSeats: [],
                roomName: roomName ?? null,
              });
              signal.throwIfAborted();
              if (result) {
                broker = result.broker;
                serverGameCode = result.gameCode;
              }
            }

            // Always display the peer `roomCode` to the host — it's the
            // 5-char code the client's `parseRoomCode` recognizes and
            // the one that works through the direct-dial path. The
            // broker's `gameCode` uses a different, wider alphabet (6
            // chars, includes 0/L) and is strictly an internal lobby
            // key — it's *not* something a guest can type into the
            // join field. Lobby-row clicks route through `resolveGuest`
            // which translates the broker key → peer id for the guest.
            onP2PEventRef.current?.({
              type: "roomCreated",
              roomCode: host.roomCode,
            });
            onP2PEventRef.current?.({ type: "waitingForGuest" });

            // The adapter owns the host Peer reference and subscribes to
            // guest connections via `hostRoom()`'s documented
            // `onGuestConnected`. `hostRoom()` buffers connections that
            // arrive before subscribe, so guests who dial during the
            // gap between `hostRoom()` returning and `initialize()`
            // subscribing are not dropped.
            const adapter = new P2PHostAdapter(
              deckList,
              host.peer,
              host.onGuestConnected,
              effectivePlayerCount,
              formatConfig,
              matchConfig,
              undefined,
              broker ?? undefined,
              false,
              serverGameCode ?? undefined,
              {
                gameId,
                roomCode: host.roomCode,
                hostDisplayName: useMultiplayerStore.getState().displayName || undefined,
                resumeData: isResume && savedState && savedSession
                  ? { state: savedState, session: savedSession }
                  : undefined,
              },
            );
            p2pAdapter = adapter;
            // Ownership of the Peer transfers to the adapter here; don't
            // double-destroy in the compensating cleanup below.
            hostPeerHandle = null;

            wireP2PEvents(adapter);

            if (isResume) {
              // Resume path: adapter.initialize() loads the saved state
              // via wasm.resumeMultiplayerHostState; resumeP2PHost
              // pulls state + legal actions into the store. Skip
              // initializeGame entirely — the engine is already live.
              await resumeP2PHost(gameId, adapter);
            } else {
              await initGame(gameId, adapter, undefined, formatConfig, effectivePlayerCount, matchConfig);
              // Mark as the active resumeable game only after setup
              // succeeds — storing the meta earlier would surface a
              // stale Resume button if construction fails mid-flight.
              saveActiveGame({ id: gameId, mode: "p2p-host", difficulty: "" });
            }
            signal.throwIfAborted();
            }
          } else {
            // p2p-join
            const code = joinCode!;
            const { conn, peer } = await joinRoom(code, signal);
            hostPeerHandle = peer;
            signal.throwIfAborted();

            // Reconstruct the same peer id `joinRoom(code)` dialed — the
            // IndexedDB session key for auto-reconnect is keyed on the full
            // prefixed id, and the guest adapter uses it on reconnect to
            // call `peer.connect(hostPeerId)`. IndexedDB (not sessionStorage)
            // means a guest whose tab crashed can reopen and rejoin with
            // their original seat.
            const hostPeerId = `phase-${code}`;
            const existing = await loadP2PSession(hostPeerId);
            signal.throwIfAborted();
            const adapter = new P2PGuestAdapter(
              deckList,
              peer,
              hostPeerId,
              conn,
              existing?.playerToken,
              useMultiplayerStore.getState().displayName || undefined,
            );
            p2pAdapter = adapter;
            hostPeerHandle = null;

            wireP2PEvents(adapter);

            await initGame(gameId, adapter, undefined, undefined, undefined, matchConfig);
            signal.throwIfAborted();
          }

          controller = createGameLoopController({ mode: "online" });
          controller.start();
          onReadyRef.current?.();
          audioManager.setContext("battlefield");
        } catch (err) {
          // Compensating teardown — fires for both aborts (unmount) and
          // real errors. Each branch is idempotent so the shape matches
          // whichever step of the pipeline failed.
          if (serverGameCode && broker) {
            // Registration landed but a later step failed; unwind the
            // server-side lobby entry. Best-effort; the server's 5-minute
            // expiry is the backstop if this itself fails.
            await broker.unregister(serverGameCode).catch(() => {
              /* best-effort */
            });
          }
          hostPeerHandle?.destroy();
          if (signal.aborted) return;
          const message = err instanceof Error ? err.message : String(err);
          // Classify the PeerJS failure mode so the UI can render an
          // actionable message. `unavailable-id` on resume means the
          // signaling server still holds the prior Peer's registration —
          // the user needs to wait, not retry immediately. Any other
          // error falls through as a generic `error` event.
          const peerErrorType = (err as { peerErrorType?: string }).peerErrorType;
          if (peerErrorType === "unavailable-id") {
            onP2PEventRef.current?.({
              type: "hostingFailed",
              reason: "room_still_claimed",
              message,
            });
          } else {
            onP2PEventRef.current?.({ type: "error", message });
          }
        }
      };

      void setupP2P();

      return () => {
        ac.abort();
        if (controller) controller.dispose();
        if (p2pUnsubscribe) p2pUnsubscribe();
        // `adapter.dispose()` is the SOLE tear-down path for the host/guest
        // Peer (see plan §4 "Peer ownership"). It also closes per-guest
        // sessions, clears timers, and disposes the WASM engine.
        if (p2pAdapter) p2pAdapter.dispose();
        audioManager.setContext("menu");
        reset();
      };
    }

    let cancelled = false;

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
          useMultiplayerStore.getState().displayName || "Player",
        );

        wsUnsubscribe = wsAdapter.onEvent((event) => {
          if (event.type === "playerIdentity") {
            useMultiplayerStore.getState().setActivePlayerId(event.playerId);
            useMultiplayerStore.getState().setOpponentDisplayName(event.opponentName);
          }
          if (event.type === "actionPendingChanged") {
            useMultiplayerStore.getState().setActionPending(event.pending);
          }
          if (event.type === "latencyChanged") {
            useMultiplayerStore.getState().setLatency(event.latencyMs);
          }
          if (event.type === "sessionChanged") {
            if (event.session) {
              saveWsSession(event.session);
            } else {
              clearWsSession();
            }
          }
          if (event.type === "stateChanged") {
            // Ensure adapter is set before animating so state updates land correctly
            const needAdapter = !useGameStore.getState().adapter && wsAdapter;
            if (needAdapter) {
              useGameStore.setState({ adapter: wsAdapter });
            }
            processRemoteUpdate(event.state, event.events, event.legalResult);
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
          if (event.type === "playerEliminated" && event.becameSpectator) {
            useMultiplayerStore.getState().setIsSpectator(true);
            useMultiplayerStore.getState().showToast("You have been eliminated. Now spectating.");
          }
          onWsEventRef.current?.(event);
        });

        // Start auto-pass controller for multiplayer (safe before game state
        // exists — onWaitingForChanged returns early when waitingFor is null)
        controller = createGameLoopController({ mode: "online" });
        controller.start();

        if (isReconnect) {
          const session = loadWsSession();
          if (session) {
            wsAdapter.tryReconnect(session);
          }
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
        useMultiplayerStore.getState().setActionPending(false);
        useMultiplayerStore.getState().setLatency(null);
        audioManager.setContext("menu");
        reset();
      };
    }

    // AI or local mode — async setup (loadGame is async due to IndexedDB)
    //
    // Uses the shared singleton adapter so the WASM worker (and its V8 TurboFan-
    // optimized code, card database, and AI worker pool) persist across game sessions.
    // On cleanup, we clear the WASM game state but keep the worker alive.
    const setupLocal = async () => {
      if (cancelled) return;

      const savedState = await loadGame(gameId);
      const adapter = getSharedAdapter();

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
          const parsedDeck = loadActiveDeck();
          if (!parsedDeck) {
            onNoDeckRef.current?.();
            return;
          }
          const deckList = buildDeckList(parsedDeck, formatConfig);
          try {
            await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
            if (cancelled) return;
            if (!adapter.cardDbLoaded) {
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
        await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
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
      // Clear store state but keep the shared WASM worker alive — its V8
      // TurboFan-compiled code, card database, and AI pool persist for reuse.
      const adapter = useGameStore.getState().adapter;
      if (adapter instanceof WasmAdapter) {
        // Not awaited (cleanup can't be async), but safe: resetGame is posted
        // to the same worker's FIFO message queue, so it executes before any
        // subsequent initializeGame call from the next game session.
        adapter.resetGameState();
        // Defer the store clear so a StrictMode remount or dep-change re-run
        // can cancel it before it fires. On real unmount (user navigates
        // away), the timeout fires on the next macrotask and clears the store.
        scheduleStoreReset(() => {
          useGameStore.setState({
            gameId: null,
            gameState: null,
            events: [],
            eventHistory: [],
            logHistory: [],
            nextLogSeq: 0,
            adapter: null,
            waitingFor: null,
            legalActions: [],
            autoPassRecommended: false,
            spellCosts: {},
            stateHistory: [],
            turnCheckpoints: [],
          });
        });
      } else {
        scheduleStoreReset(reset);
      }
    };
  }, [gameId, mode, difficulty, joinCode, formatConfig, playerCount, matchConfig, firstPlayer, useBroker, roomName]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}
