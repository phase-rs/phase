import { createContext, useEffect, useRef, type ReactNode } from "react";

import type { FormatConfig, GameAction, MatchConfig, MatchType } from "../adapter/types";
import { P2PHostAdapter, P2PGuestAdapter } from "../adapter/p2p-adapter";
import type { P2PAdapterEvent } from "../adapter/p2p-adapter";
import { WasmAdapter, getSharedAdapter } from "../adapter/wasm-adapter";
import { WebSocketAdapter } from "../adapter/ws-adapter";
import { audioManager } from "../audio/AudioManager";
import type { DeckData, WsAdapterEvent } from "../adapter/ws-adapter";
import { loadActiveDeck } from "../constants/storage";
import type { AiDeckCandidate } from "../services/aiDeckCatalog";
import { buildLegalAiDeckCatalog } from "../services/aiDeckCatalog";
import { AI_DECK_RANDOM, usePreferencesStore } from "../stores/preferencesStore";
import { createGameLoopController } from "../game/controllers/gameLoopController";
import { dispatchAction, processRemoteUpdate } from "../game/dispatch";
import { usePhaseStopsSync } from "../hooks/usePhaseStopsSync";
import { hostRoom, joinRoom } from "../network/connection";
import type { BrokerClient } from "../services/brokerClient";
import { loadP2PSession } from "../services/p2pSession";
import { expandParsedDeck, type ParsedDeck } from "../services/deckParser";
import { consumeRecentAutoUpdateMarker } from "../pwa/updateMarker";
import { ensureCardDatabase } from "../services/cardData";
import { loadDraftRun } from "../services/quickDraftPersistence";
import { clearWsSession, loadWsSession, saveWsSession } from "../services/multiplayerSession";
import { detectServerUrl } from "../services/serverDetection";
import {
  clearGame,
  clearActiveGame,
  clearP2PHostSession,
  loadActiveGame,
  loadGame,
  loadP2PHostSession,
  saveActiveGame,
  useGameStore,
} from "../stores/gameStore";
import type { AISeatBinding } from "../game/controllers/aiController";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import { assignRandomAvatars, fetchAvatarArtUrl } from "../services/playerAvatars";

/** Build per-seat AI controller bindings for a game about to start. Reads
 *  the session-scoped `aiSeats` snapshot from `ActiveGameMeta` (written at
 *  game start by the setup page); falls back to a flat `difficulty` applied
 *  to every seat when no snapshot exists (e.g. resuming a pre-multi-AI save). */
function resolveAiSeatBindings(
  gameId: string,
  playerCount: number | undefined,
  fallbackDifficulty: string | undefined,
): AISeatBinding[] | undefined {
  const count = playerCount ?? 2;
  const opponentCount = Math.max(0, count - 1);
  if (opponentCount === 0) return undefined;
  const meta = loadActiveGame();
  const snapshot = meta?.id === gameId ? meta.aiSeats : undefined;
  const fallback = fallbackDifficulty ?? "Medium";
  return Array.from({ length: opponentCount }, (_, i) => ({
    playerId: i + 1,
    difficulty: snapshot?.[i]?.difficulty ?? fallback,
  }));
}

let avatarGeneration = 0;

function setupRandomAvatars(playerCount: number) {
  const generation = ++avatarGeneration;
  const avatars = assignRandomAvatars(playerCount);
  const names = new Map<number, string>();
  names.set(0, "You");
  for (let i = 1; i < avatars.length; i++) {
    names.set(i, avatars[i].name);
  }
  useMultiplayerStore.setState({ playerNames: names, playerAvatars: new Map() });
  for (let i = 0; i < avatars.length; i++) {
    fetchAvatarArtUrl(avatars[i].cardName).then((url) => {
      if (!url || avatarGeneration !== generation) return;
      const next = new Map(useMultiplayerStore.getState().playerAvatars);
      next.set(i, url);
      useMultiplayerStore.setState({ playerAvatars: next });
    });
  }
}

function setupCommanderAvatars(gameState: { objects: Record<number, { name: string; owner: number; is_commander?: boolean }> }) {
  const generation = ++avatarGeneration;
  const names = new Map<number, string>();
  const commanderNames = new Map<number, string>();

  for (const obj of Object.values(gameState.objects)) {
    if (!obj?.is_commander) continue;
    if (commanderNames.has(obj.owner)) continue;
    commanderNames.set(obj.owner, obj.name);
  }

  for (const [playerId, cardName] of commanderNames) {
    names.set(playerId, cardName.split(",")[0].split(" //")[0]);
  }

  useMultiplayerStore.setState({ playerNames: names, playerAvatars: new Map() });

  for (const [playerId, cardName] of commanderNames) {
    fetchAvatarArtUrl(cardName).then((url) => {
      if (!url || avatarGeneration !== generation) return;
      const next = new Map(useMultiplayerStore.getState().playerAvatars);
      next.set(playerId, url);
      useMultiplayerStore.setState({ playerAvatars: next });
    });
  }
}

function parsedDeckToDeckData(deck: ParsedDeck): DeckData {
  return expandParsedDeck(deck);
}

type ExpandedDeck = { main_deck: string[]; sideboard: string[]; commander: string[] };
type DeckListPayload = {
  player: ExpandedDeck;
  opponent: ExpandedDeck;
  ai_decks: ExpandedDeck[];
};

function candidatePassesFilters(
  candidate: AiDeckCandidate,
  archetypeFilter: ReturnType<typeof usePreferencesStore.getState>["aiArchetypeFilter"],
  coverageFloor: number,
): boolean {
  if (candidate.coveragePct != null && candidate.coveragePct < coverageFloor) return false;
  return archetypeFilter === "Any" || !candidate.archetype || candidate.archetype === archetypeFilter;
}

function randomPickDistinct(pool: AiDeckCandidate[], excludeIds: Set<string>): AiDeckCandidate {
  const fresh = pool.filter((d) => !excludeIds.has(d.id));
  const source = fresh.length > 0 ? fresh : pool;
  return source[Math.floor(Math.random() * source.length)];
}

function pickOpponentDeck(
  catalog: AiDeckCandidate[],
  requestedDeckId: string,
  excludeIds: Set<string>,
  archetypeFilter: ReturnType<typeof usePreferencesStore.getState>["aiArchetypeFilter"],
  coverageFloor: number,
): AiDeckCandidate {
  if (requestedDeckId !== AI_DECK_RANDOM) {
    const pinned = catalog.find((candidate) => candidate.id === requestedDeckId);
    if (pinned) return pinned;
  }

  const filtered = catalog.filter((candidate) =>
    candidatePassesFilters(candidate, archetypeFilter, coverageFloor)
  );
  return randomPickDistinct(filtered.length > 0 ? filtered : catalog, excludeIds);
}

function buildPlayerOnlyDeckList(deck: ParsedDeck): DeckListPayload {
  const player = expandParsedDeck(deck);
  return {
    player,
    opponent: { main_deck: [], sideboard: [], commander: [] },
    ai_decks: [],
  };
}

async function buildLocalAiDeckList(
  deck: ParsedDeck,
  playerCount: number,
  formatConfig?: FormatConfig,
  selectedMatchType?: MatchType,
): Promise<DeckListPayload> {
  const { aiSeats, aiArchetypeFilter, aiCoverageFloor } = usePreferencesStore.getState();
  const catalog = await buildLegalAiDeckCatalog({
    selectedFormat: formatConfig?.format,
    selectedMatchType,
  });
  if (catalog.candidates.length === 0) {
    throw new Error(`No legal AI decks are available for ${formatConfig?.format ?? "the selected format"}.`);
  }

  const opponentCount = Math.max(1, playerCount - 1);
  const excludeIds = new Set<string>();
  const picks: ParsedDeck[] = [];
  for (let i = 0; i < opponentCount; i++) {
    // Unconfigured seats default to Random — NOT to `aiSeats[0]`. Falling
    // through to seat 0 would re-introduce the original bug: if the user
    // pinned one deck for a 2-player session and a 4-player resume-fallback
    // fires, every missing seat would clone that pinned deck.
    const requestedDeckId = aiSeats[i]?.deckId ?? AI_DECK_RANDOM;
    const result = pickOpponentDeck(
      catalog.candidates,
      requestedDeckId,
      excludeIds,
      aiArchetypeFilter,
      aiCoverageFloor,
    );
    picks.push(result.deck);
    excludeIds.add(result.id);
  }
  return {
    player: expandParsedDeck(deck),
    opponent: expandParsedDeck(picks[0]),
    ai_decks: picks.slice(1).map(expandParsedDeck),
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

/**
 * Fire a browser notification that an opponent joined the host's game.
 * Suppressed when the tab is focused (user already sees it) or when
 * permission is not granted. Silent on browsers that reject
 * `new Notification()` outside a ServiceWorker (Safari, some mobile).
 * Shared by the WS and P2P host-side join paths.
 */
function notifyOpponentJoined(opponentName?: string): void {
  if (
    typeof Notification === "undefined"
    || Notification.permission !== "granted"
    || typeof document === "undefined"
    || document.visibilityState === "visible"
  ) {
    return;
  }
  try {
    const body = opponentName
      ? `${opponentName} joined your game. Come back and play!`
      : "Your opponent has joined the game. Come back and play!";
    const n = new Notification("Opponent joined", { body });
    n.onclick = () => {
      window.focus();
      n.close();
    };
  } catch {
    // Silent fallback.
  }
}

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
  source?: string;
  draftId?: string;
  onWsEvent?: (event: WsAdapterEvent) => void;
  onP2PEvent?: (event: P2PAdapterEvent) => void;
  onReady?: () => void;
  onCardDataMissing?: () => void;
  onNoDeck?: (reason?: string) => void;
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
  source,
  draftId,
  onWsEvent,
  onP2PEvent,
  onReady,
  onCardDataMissing,
  onNoDeck,
  onResumeReset,
  children,
}: GameProviderProps) {
  // Sync the persistent phaseStops preference into engine-owned state so the
  // engine remains the single authority for auto-pass / empty-blocker decisions.
  usePhaseStopsSync();

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
    if (mode !== "ai") return;
    let applied = false;
    const unsub = useGameStore.subscribe((state) => {
      if (applied || !state.gameState?.command_zone?.length) return;
      applied = true;
      setupCommanderAvatars(state.gameState);
      unsub();
    });
    const state = useGameStore.getState();
    if (!applied && state.gameState?.command_zone?.length) {
      applied = true;
      setupCommanderAvatars(state.gameState);
      unsub();
    }
    return unsub;
  }, [mode, gameId]);

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
      if (mode === "ai") {
        setupRandomAvatars(playerCount ?? 2);
      } else {
        useMultiplayerStore.setState({ playerNames: new Map(), playerAvatars: new Map() });
      }
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

      const wireP2PEvents = (adapter: P2PHostAdapter | P2PGuestAdapter) => {
        // Host-only: proactively request notification permission while the
        // user is at the "waiting for opponent" screen so the later
        // `guestConnected` event can fire a notification (Bug 2).
        if (
          mode === "p2p-host"
          && typeof Notification !== "undefined"
          && Notification.permission === "default"
        ) {
          void Notification.requestPermission().catch(() => {});
        }
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
          if (event.type === "guestConnected") {
            notifyOpponentJoined();
          }
          onP2PEventRef.current?.(event);
        });
      };

      const setupP2P = async () => {
        const effectivePlayerCount = playerCount ?? 2;
        const deckList = buildPlayerOnlyDeckList(parsedDeck);
        signal.throwIfAborted();

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
                draftMetadata: null,
              });
              signal.throwIfAborted();
              if (result) {
                broker = result.broker;
                serverGameCode = result.gameCode;
              }
            }

            // Only show the lobby tile for fresh hosts waiting for guests.
            // Resume flows skip this — the game is already started and the
            // tile re-appearing on a live game page is confusing.
            if (!isResume) {
              onP2PEventRef.current?.({
                type: "roomCreated",
                roomCode: host.roomCode,
              });
              onP2PEventRef.current?.({ type: "waitingForGuest" });
            }

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
            const { conn, peer } = await joinRoom(code, signal, 10_000);
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
            saveActiveGame({ id: gameId, mode: "p2p-join", difficulty: "", p2pRoomCode: code });
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
          const peerErrorType = (err as { peerErrorType?: string }).peerErrorType;
          if (peerErrorType === "unavailable-id") {
            onP2PEventRef.current?.({
              type: "hostingFailed",
              reason: "room_still_claimed",
              message,
            });
          } else if (message.includes("Deck rejected:") || message.includes("Deck not legal")) {
            const sepIdx = message.indexOf("||format:");
            onP2PEventRef.current?.({
              type: "deckRejected",
              reason: sepIdx >= 0 ? message.slice(0, sepIdx) : message,
              format: sepIdx >= 0 ? message.slice(sepIdx + 9) : undefined,
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

      // Password bridging: prefer sessionStorage over URL params so the
      // password never appears in the URL bar, browser history, or
      // outbound Referer headers. Fall back to URL params for first-load
      // compatibility, and immediately strip the password from the URL
      // via history.replaceState if we find it there.
      const urlParams = new URLSearchParams(window.location.search);
      const sessionKey = `phase-join-password:${joinCode ?? ""}`;
      let password: string | undefined =
        (joinCode && window.sessionStorage.getItem(sessionKey)) || undefined;
      if (!password && urlParams.has("password")) {
        password = urlParams.get("password") ?? undefined;
        if (password && joinCode) {
          window.sessionStorage.setItem(sessionKey, password);
        }
        urlParams.delete("password");
        const stripped = urlParams.toString();
        const newPath =
          window.location.pathname
          + (stripped ? `?${stripped}` : "")
          + window.location.hash;
        window.history.replaceState(window.history.state, "", newPath);
      }

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
          if (event.type === "gameCreated") {
            // Host-side: proactively request browser notification permission
            // while the user is staring at the "waiting for opponent" screen
            // so we can fire a notification the moment a guest joins (Bug 2).
            // No-op if already granted/denied or if unsupported (mobile,
            // http: origins). Ignoring the permission result is intentional —
            // we fall back silently on `opponentJoined`.
            if (typeof Notification !== "undefined" && Notification.permission === "default") {
              void Notification.requestPermission().catch(() => {});
            }
          }
          if (event.type === "opponentJoined") {
            notifyOpponentJoined(event.opponentName);
          }
          if (event.type === "passwordRequired") {
            // Server rejected the join because the room is password-protected
            // and we sent no / wrong password. Stash the new password in
            // sessionStorage (same key `setupWs` reads from) and reload —
            // the reload re-mounts GameProvider which re-reads the stash.
            // We deliberately avoid putting the password in the URL: that
            // would land it in browser history and in outbound Referer
            // headers to any image CDN / Scryfall / analytics request.
            const entered = window.prompt("This room requires a password:");
            if (entered && joinCode) {
              window.sessionStorage.setItem(
                `phase-join-password:${joinCode}`,
                entered,
              );
              window.location.reload();
            } else {
              if (joinCode) {
                window.sessionStorage.removeItem(
                  `phase-join-password:${joinCode}`,
                );
              }
              useMultiplayerStore.getState().setConnectionStatus("disconnected");
              window.location.href = "/multiplayer";
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
          }).catch((err) => {
            if (cancelled) return;
            const msg = err instanceof Error ? err.message : String(err);
            useMultiplayerStore.getState().setConnectionStatus("disconnected");
            if (msg.includes("Deck not legal")) {
              onWsEventRef.current?.({ type: "deckRejected", reason: msg });
            } else {
              useMultiplayerStore.getState().showToast("Connection failed. Retry or change server in Settings.");
            }
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
          // Derive player count from the restored state — the URL param may be
          // absent on resume (e.g. navigating directly to a saved game URL).
          const resumedPlayerCount = savedState.players?.length ?? playerCount;
          controller = createGameLoopController({
            mode,
            difficulty,
            aiSeats: resolveAiSeatBindings(gameId, resumedPlayerCount, difficulty),
            playerCount: resumedPlayerCount,
          });
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
          let deckList: DeckListPayload;
          try {
            deckList = await buildLocalAiDeckList(
              parsedDeck,
              playerCount ?? 2,
              formatConfig,
              matchConfig?.match_type,
            );
          } catch (deckErr) {
            onNoDeckRef.current?.(deckErr instanceof Error ? deckErr.message : String(deckErr));
            return;
          }
          try {
            await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
            if (cancelled) return;
            if (!adapter.cardDbLoaded) {
              onCardDataMissingRef.current?.();
            }
            controller = createGameLoopController({
              mode,
              difficulty,
              aiSeats: resolveAiSeatBindings(gameId, playerCount, difficulty),
              playerCount,
            });
            controller.start();
            audioManager.setContext("battlefield");
          } catch (initErr) {
            console.error("Deck validation failed:", initErr);
            if (!cancelled) onNoDeckRef.current?.(initErr instanceof Error ? initErr.message : String(initErr));
          }
        }
        return;
      }

      // No saved state — start a new game.
      // Draft mode: deck data was pre-built by DraftPage and stored in
      // sessionStorage. Use it directly instead of loadActiveDeck + buildDeckList.
      const draftDeckKey = `phase:draft-deck:${gameId}`;
      const draftDeckRaw = sessionStorage.getItem(draftDeckKey);
      if (draftDeckRaw) {
        sessionStorage.removeItem(draftDeckKey);
        const deckList = JSON.parse(draftDeckRaw) as {
          player: { main_deck: string[]; sideboard: string[]; commander: string[] };
          opponent: { main_deck: string[]; sideboard: string[]; commander: string[] };
          ai_decks: Array<{ main_deck: string[]; sideboard: string[]; commander: string[] }>;
        };
        try {
          await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
          if (cancelled) return;
          controller = createGameLoopController({
            mode,
            difficulty,
            aiSeats: resolveAiSeatBindings(gameId, playerCount, difficulty),
            playerCount,
          });
          controller.start();
          audioManager.setContext("battlefield");
        } catch (err) {
          console.error("Draft deck validation failed:", err);
          if (!cancelled) onNoDeckRef.current?.();
        }
        return;
      }

      if (source === "draft" && draftId) {
        const run = await loadDraftRun(draftId);
        if (run) {
          const deckList = {
            player: { main_deck: run.playerDeck, sideboard: [] as string[], commander: [] as string[] },
            opponent: { main_deck: run.opponentDeck, sideboard: [] as string[], commander: [] as string[] },
            ai_decks: [],
          };
          try {
            await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
            if (cancelled) return;
            controller = createGameLoopController({
              mode,
              difficulty,
              aiSeats: resolveAiSeatBindings(gameId, playerCount, difficulty),
              playerCount,
            });
            controller.start();
            audioManager.setContext("battlefield");
          } catch (err) {
            console.error("Draft IDB deck fallback failed:", err);
            if (!cancelled) onNoDeckRef.current?.();
          }
          return;
        }
      }

      const parsedDeck = loadActiveDeck();
      if (!parsedDeck) {
        onNoDeckRef.current?.();
        return;
      }

      let deckList: DeckListPayload;
      try {
        deckList = await buildLocalAiDeckList(
          parsedDeck,
          playerCount ?? 2,
          formatConfig,
          matchConfig?.match_type,
        );
      } catch (deckErr) {
        onNoDeckRef.current?.(deckErr instanceof Error ? deckErr.message : String(deckErr));
        return;
      }
      try {
        await initGame(gameId, adapter, deckList, formatConfig, playerCount, matchConfig, firstPlayer);
        if (cancelled) return;
        if (!adapter.cardDbLoaded) {
          onCardDataMissingRef.current?.();
        }
        controller = createGameLoopController({
          mode,
          difficulty,
          aiSeats: resolveAiSeatBindings(gameId, playerCount, difficulty),
          playerCount,
        });
        controller.start();
        audioManager.setContext("battlefield");
      } catch (err) {
        console.error("Deck validation failed:", err);
        if (!cancelled) onNoDeckRef.current?.(err instanceof Error ? err.message : String(err));
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
  }, [gameId, mode, difficulty, joinCode, formatConfig, playerCount, matchConfig, firstPlayer, useBroker, roomName, source, draftId]);

  return (
    <GameDispatchContext.Provider value={dispatchAction}>
      {children}
    </GameDispatchContext.Provider>
  );
}
