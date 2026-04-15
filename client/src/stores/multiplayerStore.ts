import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { FormatConfig, GameFormat, LobbyGame, MatchType, PlayerId } from "../adapter/types";
import { PROTOCOL_VERSION, type ServerInfo } from "../adapter/ws-adapter";
import {
  clearWsSession,
  loadWsSession,
  saveWsSession,
} from "../services/multiplayerSession";
import {
  resolveGuestOver,
  subscribeLobbyOver,
  type ResolveResult,
} from "../services/brokerClient";
import {
  HandshakeError,
  openPhaseSocket,
  withReconnect,
  type PhaseSocket,
  type ReconnectHandle,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl } from "../services/serverDetection";
import { saveActiveGame, useGameStore } from "./gameStore";

type ConnectionStatus = "disconnected" | "connecting" | "connected";
type HostingStatus = "idle" | "connecting" | "waiting";

// Module-level WebSocket ref (non-serializable, lives outside store)
let hostWs: WebSocket | null = null;
// Prevents onclose from clearing session token after GameStarted
let gameStartedFired = false;
// Reconnection state for the hosting WebSocket
let hostReconnectAttempt = 0;
let hostReconnectTimer: ReturnType<typeof setTimeout> | null = null;
const HOST_MAX_RECONNECT_ATTEMPTS = 3;

/**
 * Long-lived, reconnecting subscription channel. Opened on first
 * multiplayer-home entry via `ensureSubscriptionSocket`, not at app boot:
 * users who never touch multiplayer don't pay for a WS. Shared between
 * the lobby subscribe path (SubscribeLobby / LobbyUpdate traffic) and the
 * P2P guest resolve path (JoinGameWithPassword → PeerInfo). The
 * `withReconnect` wrapper re-handshakes up to 3 times on unexpected
 * drops; `onStateChange` drives pending-RPC rejection and re-subscribe.
 */
let subscriptionReconnect: ReconnectHandle | null = null;
/** Awaiters of the first open — resolves once the handshake lands, or with
 * `null` if the factory exhausts all retries without ever connecting. */
let subscriptionFirstOpen: Promise<PhaseSocket | null> | null = null;

/**
 * AbortControllers for in-flight `resolveGuest` calls. On the socket's
 * `reconnecting` transition we abort every pending call so the caller
 * gets a `connection_lost` result immediately rather than waiting for
 * its own timeout. New calls after reconnect use fresh controllers.
 */
const pendingResolveAborts: Set<AbortController> = new Set();

/**
 * Registered lobby subscribers. The store multiplexes one
 * `subscribeLobbyOver` attachment across all of them: the first
 * subscriber sends `SubscribeLobby` to the server, subsequent
 * subscribers are fanned-out snapshots from the cached `lobbySnapshot`,
 * and only the *last* subscriber leaving sends `UnsubscribeLobby`. This
 * prevents the ref-counting bug where one caller's unsubscribe would
 * silence every other caller on the same shared socket.
 */
const lobbySubscribers: Set<(games: LobbyGame[]) => void> = new Set();
/** Most recent `LobbyUpdate` snapshot, used to seed new subscribers. */
let lobbySnapshot: LobbyGame[] | null = null;
/** Per-socket detach returned by `subscribeLobbyOver`. Re-bound on
 * reconnect; `null` when no socket is attached. */
let lobbyAttachDetach: (() => void) | null = null;

export interface AiSeatConfig {
  seatIndex: number;
  difficulty: string;
  deckName: string | null;
}

export interface HostingDeck {
  main_deck: string[];
  sideboard: string[];
  commander?: string[];
}

export interface HostingSettings {
  displayName: string;
  public: boolean;
  password: string;
  timerSeconds: number | null;
  formatConfig: FormatConfig;
  matchType: MatchType;
  aiSeats: AiSeatConfig[];
  /** Optional per-match label shown in the lobby, distinct from `displayName`
   * (the player's global identity). `null` means "use the player's name". */
  roomName: string | null;
}

export interface PlayerSlot {
  playerId: string;
  name: string;
  isReady: boolean;
  isAi: boolean;
  aiDifficulty: string;
  deckName: string | null;
}

/** Single toast entry keyed by caller.
 *
 * `expiresAt` is always set (absolute wall-clock ms) — both plain and
 * countdown toasts auto-dismiss by comparing `expiresAt <= Date.now()`,
 * which is immune to Map-mutation re-renders that would otherwise reset a
 * relative `setTimeout`. Plain toasts use a fixed 5s window; countdown
 * toasts use `countdownSeconds` from the caller.
 *
 * `showCountdown` controls the "Ns to forfeit" suffix in the UI, keeping
 * the visual treatment (amber banner at top vs. red at bottom) orthogonal
 * to the dismissal mechanism.
 */
export interface Toast {
  message: string;
  expiresAt: number;
  showCountdown: boolean;
}

/** Default auto-dismiss window for plain toasts. */
const PLAIN_TOAST_DURATION_MS = 5000;

/** Stable key for opponent-disconnect toasts so multiple concurrent
 * disconnects in a 3+ player game stack instead of stomping each other. */
export function playerToastKey(playerId: number): string {
  return `player:${playerId}`;
}

/** Default slot for toasts that don't care about coexisting with others
 * (generic errors, own-reconnect banners). Matches the pre-map single-slot
 * behavior: repeated generic toasts replace each other. */
const GENERIC_TOAST_KEY = "generic";

interface MultiplayerState {
  playerId: string;
  displayName: string;
  serverAddress: string;
  connectionStatus: ConnectionStatus;
  activePlayerId: PlayerId | null;
  opponentDisplayName: string | null;
  /** Keyed toast stack. Iteration order = insertion order (Map guarantee),
   * so the UI renders them top-down in the order they were raised. */
  toasts: Map<string, Toast>;
  formatConfig: FormatConfig | null;
  playerSlots: PlayerSlot[];
  spectators: string[];
  isSpectator: boolean;
  // Per-player connection tracking (ephemeral — not persisted)
  disconnectedPlayers: Set<number>;
  // Action round-trip tracking (ephemeral — not persisted)
  actionPending: boolean;
  latencyMs: number | null;
  // Hosting session (ephemeral — not persisted)
  hostGameCode: string | null;
  hostIsPublic: boolean;
  hostingStatus: HostingStatus;
  pendingGameRoute: string | null;
  // Server identity from the most recent ServerHello (ephemeral — not persisted).
  // null before the first hello; updated when the hosting WS or the game WS
  // completes its handshake.
  serverInfo: ServerInfo | null;
}

interface MultiplayerActions {
  setDisplayName: (name: string) => void;
  setServerAddress: (address: string) => void;
  setConnectionStatus: (status: ConnectionStatus) => void;
  setActivePlayerId: (id: PlayerId | null) => void;
  setOpponentDisplayName: (name: string | null) => void;
  /**
   * Show a transient toast. When `opts.countdownSeconds` is provided, the
   * toast renders a live countdown and persists until it reaches zero or
   * is explicitly cleared; otherwise it auto-dismisses after 5 seconds.
   * `opts.key` lets concurrent toasts coexist (e.g. `playerToastKey(pid)`);
   * omitted keys all share the "generic" slot (old behavior).
   */
  showToast: (
    message: string,
    opts?: { countdownSeconds?: number; key?: string },
  ) => void;
  /** Clear one toast. No key → clear the generic slot only. */
  clearToast: (key?: string) => void;
  /** Clear only player-disconnect toasts (`player:*` keys). Leaves generic
   * toasts like connection errors intact. Use on `gameResumed`. */
  clearPlayerToasts: () => void;
  /** Clear every toast. Rarely needed — prefer `clearPlayerToasts()` or
   * keyed `clearToast()`. Retained for full-reset paths. */
  clearAllToasts: () => void;
  setFormatConfig: (config: FormatConfig | null) => void;
  setPlayerSlots: (slots: PlayerSlot[]) => void;
  toggleReady: (playerId: string) => void;
  setSpectators: (names: string[]) => void;
  setIsSpectator: (value: boolean) => void;
  setPlayerDisconnected: (playerId: number) => void;
  setPlayerReconnected: (playerId: number) => void;
  setActionPending: (pending: boolean) => void;
  setLatency: (ms: number | null) => void;
  // Hosting session actions
  startHosting: (settings: HostingSettings, deck: HostingDeck) => void;
  cancelHosting: () => void;
  clearPendingGameRoute: () => void;
  setServerInfo: (info: ServerInfo | null) => void;
  /**
   * Lazily open the long-lived subscription socket and return the
   * `PhaseSocket`. Idempotent: a second call while an open is in flight
   * returns the same promise. Resolves `null` if the handshake fails so
   * callers can fall back rather than crash.
   */
  ensureSubscriptionSocket: () => Promise<PhaseSocket | null>;
  /** Close and discard the subscription socket. Called on store teardown. */
  closeSubscriptionSocket: () => void;
  /**
   * Send `JoinGameWithPassword` over the subscription socket and return a
   * discriminated `ResolveResult`. Opens the socket lazily if it's not yet
   * alive. Does NOT navigate — the caller inspects the result and handles
   * password retry, build mismatch, etc. before navigation.
   */
  resolveGuest: (code: string, password?: string) => Promise<ResolveResult>;
  /**
   * Subscribe to lobby-list updates over the subscription socket. Returns
   * a cleanup function that detaches listeners and sends `UnsubscribeLobby`.
   * Callers should not await; `onUpdate` fires asynchronously once the
   * first `LobbyUpdate` snapshot arrives. Returns `null` when the socket
   * could not be opened so the caller can render a fallback.
   */
  subscribeLobby: (
    onUpdate: (games: LobbyGame[]) => void,
  ) => Promise<(() => void) | null>;
}

/**
 * Checks whether a lobby entry's host is running a compatible build with
 * the browsing client. Used by the lobby list to disable incompatible
 * rows. A missing `hostBuildCommit` (restored session, legacy entry) is
 * treated as unknown-but-allowed, matching the server's behavior at the
 * join gate. We compare against this client's `__BUILD_HASH__` rather
 * than the server's commit because in `LobbyOnly` mode the server is a
 * P2P peer broker — its commit is independent of the host/guest engine
 * build that actually has to agree at game time. In `Full` mode the
 * protocol-version check in `isServerCompatible` covers the client-to-
 * server direction, and host/guest still need matching engine builds.
 */
export function isLobbyEntryCompatible(
  hostBuildCommit: string | undefined,
): boolean {
  if (!hostBuildCommit) return true;
  return hostBuildCommit === __BUILD_HASH__;
}

/** True when the client's wire-protocol matches the server's. */
export function isServerCompatible(info: ServerInfo | null): boolean {
  if (!info) return false;
  return info.protocolVersion === PROTOCOL_VERSION;
}

export const FORMAT_DEFAULTS: Record<GameFormat, FormatConfig> = {
  Standard: {
    format: "Standard",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  Pioneer: {
    format: "Pioneer",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  Historic: {
    format: "Historic",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  Pauper: {
    format: "Pauper",
    starting_life: 20,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  Commander: {
    format: "Commander",
    starting_life: 40,
    min_players: 2,
    max_players: 4,
    deck_size: 100,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
  },
  Brawl: {
    format: "Brawl",
    starting_life: 25,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
  },
  HistoricBrawl: {
    format: "HistoricBrawl",
    starting_life: 25,
    min_players: 2,
    max_players: 2,
    deck_size: 60,
    singleton: true,
    command_zone: true,
    commander_damage_threshold: 21,
    range_of_influence: null,
    team_based: false,
  },
  FreeForAll: {
    format: "FreeForAll",
    starting_life: 20,
    min_players: 3,
    max_players: 6,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: false,
  },
  TwoHeadedGiant: {
    format: "TwoHeadedGiant",
    starting_life: 30,
    min_players: 4,
    max_players: 4,
    deck_size: 60,
    singleton: false,
    command_zone: false,
    commander_damage_threshold: null,
    range_of_influence: null,
    team_based: true,
  },
};

export const useMultiplayerStore = create<MultiplayerState & MultiplayerActions>()(
  persist(
    (set, get) => ({
      playerId: crypto.randomUUID(),
      displayName: "",
      serverAddress: "wss://us.phase-rs.dev/ws",
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      toasts: new Map(),
      formatConfig: null,
      playerSlots: [],
      spectators: [],
      isSpectator: false,
      disconnectedPlayers: new Set(),
      actionPending: false,
      latencyMs: null,
      hostGameCode: null,
      hostIsPublic: false,
      hostingStatus: "idle" as HostingStatus,
      pendingGameRoute: null,
      serverInfo: null,

      setServerInfo: (info) => set({ serverInfo: info }),
      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (address) => set({ serverAddress: address }),
      setConnectionStatus: (status) => set({ connectionStatus: status }),
      setActivePlayerId: (id) => set({ activePlayerId: id }),
      setOpponentDisplayName: (name) => set({ opponentDisplayName: name }),
      showToast: (message, opts) =>
        set((state) => {
          const key = opts?.key ?? GENERIC_TOAST_KEY;
          const isCountdown = opts?.countdownSeconds != null;
          const expiresAt = isCountdown
            ? Date.now() + opts!.countdownSeconds! * 1000
            : Date.now() + PLAIN_TOAST_DURATION_MS;
          const next = new Map(state.toasts);
          next.set(key, { message, expiresAt, showCountdown: isCountdown });
          return { toasts: next };
        }),
      clearToast: (key) =>
        set((state) => {
          const k = key ?? GENERIC_TOAST_KEY;
          if (!state.toasts.has(k)) return {};
          const next = new Map(state.toasts);
          next.delete(k);
          return { toasts: next };
        }),
      /** Clear every player-disconnect toast. Used on `gameResumed`, which is
       * a server-wide resume — any per-player countdown is moot, but generic
       * toasts (errors, connection warnings) should survive. */
      clearPlayerToasts: () =>
        set((state) => {
          let changed = false;
          const next = new Map(state.toasts);
          for (const key of state.toasts.keys()) {
            if (key.startsWith("player:")) {
              next.delete(key);
              changed = true;
            }
          }
          return changed ? { toasts: next } : {};
        }),
      clearAllToasts: () =>
        set((state) =>
          state.toasts.size === 0 ? {} : { toasts: new Map() },
        ),
      setFormatConfig: (config) => set({ formatConfig: config }),
      setPlayerSlots: (slots) => set({ playerSlots: slots }),
      toggleReady: (playerId) =>
        set((state) => ({
          playerSlots: state.playerSlots.map((slot) =>
            slot.playerId === playerId ? { ...slot, isReady: !slot.isReady } : slot,
          ),
        })),
      setSpectators: (names) => set({ spectators: names }),
      setIsSpectator: (value) => set({ isSpectator: value }),
      setPlayerDisconnected: (pid) =>
        set((state) => {
          const next = new Set(state.disconnectedPlayers);
          next.add(pid);
          return { disconnectedPlayers: next };
        }),
      setPlayerReconnected: (pid) =>
        set((state) => {
          const next = new Set(state.disconnectedPlayers);
          next.delete(pid);
          return { disconnectedPlayers: next };
        }),
      setActionPending: (pending) => set({ actionPending: pending }),
      setLatency: (ms) => set({ latencyMs: ms }),

      startHosting: (settings, deck) => {
        // Clean up any existing hosting session
        if (hostWs) {
          hostWs.close();
          hostWs = null;
        }
        if (hostReconnectTimer) {
          clearTimeout(hostReconnectTimer);
          hostReconnectTimer = null;
        }
        clearWsSession();
        gameStartedFired = false;
        hostReconnectAttempt = 0;

        set({
          hostIsPublic: settings.public,
          hostingStatus: "connecting",
          hostGameCode: null,
          pendingGameRoute: null,
        });

        // Shared post-handshake message handler. ServerHello is handled
        // upstream by `openPhaseSocket`, so by the time we get here the
        // server's identity is already known and compatible.
        const handleHostMessage = (ws: WebSocket, msg: { type: string; data?: unknown }) => {
          if (msg.type === "GameCreated") {
            const data = msg.data as { game_code: string; player_token: string };
            saveWsSession({
              gameCode: data.game_code,
              playerToken: data.player_token,
              serverUrl: get().serverAddress,
              timestamp: Date.now(),
            });
            // Reset reconnect counter on successful (re)connection
            hostReconnectAttempt = 0;
            set({ hostGameCode: data.game_code, hostingStatus: "waiting" });
          } else if (msg.type === "GameStarted") {
            gameStartedFired = true;
            ws.close();
            hostWs = null;
            const gameId = crypto.randomUUID();
            saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
            useGameStore.setState({ gameId });
            // Reset hosting state FIRST so banner hides, then set route
            set({
              hostGameCode: null,
              hostingStatus: "idle",
              playerSlots: [],
              pendingGameRoute: `/game/${gameId}?mode=host`,
            });
          } else if (msg.type === "PlayerSlotsUpdate") {
            const data = msg.data as { slots: PlayerSlot[] };
            set({ playerSlots: data.slots });
          } else if (msg.type === "Error") {
            const data = msg.data as { message: string };
            console.error("Host error:", data.message);
            get().showToast(data.message || "Failed to create game.");
            get().cancelHosting();
          }
        };

        // Open a phase socket (handshake + version gate) then attach our
        // post-handshake message/close handlers and send `setupFrame`. All
        // callers (initial connect + reconnect) funnel through here so the
        // handshake policy lives in one place.
        const openHostSocket = async (
          setupFrame: () => unknown,
          onReopen: () => void,
        ): Promise<void> => {
          if (!isValidWebSocketUrl(get().serverAddress)) {
            clearWsSession();
            set({
              hostGameCode: null,
              hostIsPublic: false,
              hostingStatus: "idle",
              playerSlots: [],
            });
            get().showToast("Invalid server address. Update it in Settings.");
            return;
          }

          let socket;
          try {
            socket = await openPhaseSocket(get().serverAddress);
          } catch (err) {
            if (
              err instanceof HandshakeError &&
              err.kind === "protocol_mismatch"
            ) {
              get().showToast(err.message);
              get().cancelHosting();
              return;
            }
            if (!gameStartedFired) {
              hostWs = null;
              onReopen();
            }
            return;
          }

          set({ serverInfo: socket.serverInfo });
          hostWs = socket.ws;

          socket.ws.onmessage = (event) => {
            const msg = JSON.parse(event.data as string) as {
              type: string;
              data?: unknown;
            };
            handleHostMessage(socket.ws, msg);
          };
          socket.ws.onerror = () => {
            if (!gameStartedFired) {
              hostWs = null;
              onReopen();
            }
          };
          socket.ws.onclose = () => {
            if (!gameStartedFired && hostWs === socket.ws) {
              hostWs = null;
              onReopen();
            }
          };

          socket.ws.send(JSON.stringify(setupFrame()));
        };

        // Attempt to reconnect the hosting WS using stored session token
        const attemptHostReconnect = () => {
          if (gameStartedFired) return;
          const session = loadWsSession();
          if (!session || hostReconnectAttempt >= HOST_MAX_RECONNECT_ATTEMPTS) {
            // No session to reconnect or exhausted attempts — give up
            clearWsSession();
            set({
              hostGameCode: null,
              hostIsPublic: false,
              hostingStatus: "idle",
              playerSlots: [],
            });
            get().showToast("Connection to server lost.");
            return;
          }

          hostReconnectAttempt++;
          const delay = Math.pow(2, hostReconnectAttempt - 1) * 1000;
          hostReconnectTimer = setTimeout(() => {
            hostReconnectTimer = null;
            if (gameStartedFired) return;
            void openHostSocket(
              () => ({
                type: "Reconnect",
                data: {
                  game_code: session.gameCode,
                  player_token: session.playerToken,
                },
              }),
              attemptHostReconnect,
            );
          }, delay);
        };

        void openHostSocket(
          () => ({
            type: "CreateGameWithSettings",
            data: {
              deck: { main_deck: deck.main_deck, sideboard: deck.sideboard, commander: deck.commander ?? [] },
              display_name: settings.displayName,
              public: settings.public,
              password: settings.password || null,
              timer_seconds: settings.timerSeconds,
              player_count: settings.formatConfig.max_players,
              match_config: { match_type: settings.matchType },
              format_config: settings.formatConfig,
              ai_seats: settings.aiSeats,
              room_name: settings.roomName,
            },
          }),
          attemptHostReconnect,
        );
      },

      cancelHosting: () => {
        if (hostReconnectTimer) {
          clearTimeout(hostReconnectTimer);
          hostReconnectTimer = null;
        }
        if (hostWs) {
          hostWs.close();
          hostWs = null;
        }
        gameStartedFired = false;
        hostReconnectAttempt = 0;
        clearWsSession();
        set({
          hostGameCode: null,
          hostIsPublic: false,
          hostingStatus: "idle",
          playerSlots: [],
          pendingGameRoute: null,
        });
      },

      clearPendingGameRoute: () => set({ pendingGameRoute: null }),

      ensureSubscriptionSocket: async () => {
        // Fast path: handle is live and currently has a connected socket.
        const existing = subscriptionReconnect?.current();
        if (existing && existing.ws.readyState === WebSocket.OPEN) {
          return existing;
        }
        // Deduped first-open promise: concurrent callers await the same
        // `withReconnect` bootstrapping without racing handshakes.
        if (subscriptionFirstOpen) return subscriptionFirstOpen;

        const addr = get().serverAddress;
        if (!isValidWebSocketUrl(addr)) return null;

        subscriptionFirstOpen = new Promise<PhaseSocket | null>((resolve) => {
          let settled = false;
          const settle = (val: PhaseSocket | null) => {
            if (settled) return;
            settled = true;
            resolve(val);
          };

          subscriptionReconnect = withReconnect(
            () =>
              openPhaseSocket(addr).catch((err) => {
                // Protocol mismatch is not retryable — surface the toast
                // on the *first* handshake attempt, then let
                // `withReconnect` treat subsequent attempts as plain
                // errors (they'll keep rejecting until "offline" fires).
                if (
                  err instanceof HandshakeError &&
                  err.kind === "protocol_mismatch"
                ) {
                  get().showToast(err.message);
                }
                throw err;
              }),
            {
              attempts: 3,
              onStateChange: (state) => {
                if (state === "open") {
                  const socket = subscriptionReconnect?.current() ?? null;
                  if (socket) {
                    set({ serverInfo: socket.serverInfo });
                    // Re-attach the single multiplexed lobby listener if
                    // any subscribers are registered. The first snapshot
                    // from the server will overwrite `lobbySnapshot` and
                    // fan-out; stale cached data is not authoritative
                    // across a reconnect.
                    if (lobbySubscribers.size > 0) {
                      lobbyAttachDetach = subscribeLobbyOver(socket, (games) => {
                        lobbySnapshot = games;
                        for (const cb of lobbySubscribers) cb(games);
                      });
                    }
                  }
                  settle(socket);
                } else if (state === "reconnecting") {
                  // In-flight RPCs would otherwise hang until their own
                  // timeout. Abort them now so the caller can branch
                  // immediately. New RPCs registered after this point
                  // use fresh controllers and are unaffected.
                  for (const ac of pendingResolveAborts) ac.abort();
                  pendingResolveAborts.clear();
                  // Drop the handle to the old socket's listener; it
                  // will be re-bound on the next "open".
                  lobbyAttachDetach = null;
                } else if (state === "offline") {
                  // Reconnect exhausted. Caller's `ensureSubscriptionSocket`
                  // resolves `null` so fallback UI renders. Also drain any
                  // stragglers that joined between reconnecting and offline.
                  for (const ac of pendingResolveAborts) ac.abort();
                  pendingResolveAborts.clear();
                  settle(null);
                }
              },
            },
          );
        }).finally(() => {
          subscriptionFirstOpen = null;
        });

        return subscriptionFirstOpen;
      },

      closeSubscriptionSocket: () => {
        for (const ac of pendingResolveAborts) ac.abort();
        pendingResolveAborts.clear();
        lobbyAttachDetach?.();
        lobbyAttachDetach = null;
        lobbySubscribers.clear();
        lobbySnapshot = null;
        subscriptionReconnect?.close();
        subscriptionReconnect = null;
      },

      resolveGuest: async (code, password) => {
        const socket = await get().ensureSubscriptionSocket();
        if (!socket) {
          return {
            ok: false,
            reason: "connection_lost",
            message: "Lobby connection unavailable. Check your server address.",
          };
        }
        // Register an abort controller so a mid-RPC `reconnecting`
        // transition can cut short the wait with `connection_lost`
        // rather than letting the caller's own timeout fire.
        const ac = new AbortController();
        pendingResolveAborts.add(ac);
        try {
          return await resolveGuestOver(socket, code, password, {
            signal: ac.signal,
          });
        } finally {
          pendingResolveAborts.delete(ac);
        }
      },

      subscribeLobby: async (onUpdate) => {
        const socket = await get().ensureSubscriptionSocket();
        if (!socket) return null;
        const wasEmpty = lobbySubscribers.size === 0;
        lobbySubscribers.add(onUpdate);
        // First subscriber sends `SubscribeLobby`. Later subscribers ride
        // the same upstream attachment — sending the frame again per
        // subscriber, then detaching on their own cleanup, would send
        // `UnsubscribeLobby` on the shared socket and silence every
        // other subscriber (the ref-counting bug this structure fixes).
        if (wasEmpty) {
          lobbyAttachDetach = subscribeLobbyOver(socket, (games) => {
            lobbySnapshot = games;
            for (const cb of lobbySubscribers) cb(games);
          });
        } else if (lobbySnapshot) {
          // Immediate seed for late subscribers so they don't wait on
          // the next server push to render anything.
          onUpdate(lobbySnapshot);
        }
        return () => {
          lobbySubscribers.delete(onUpdate);
          if (lobbySubscribers.size === 0) {
            lobbyAttachDetach?.();
            lobbyAttachDetach = null;
            lobbySnapshot = null;
          }
        };
      },
    }),
    {
      name: "phase-multiplayer",
      partialize: (state) => ({
        playerId: state.playerId,
        displayName: state.displayName,
        serverAddress: state.serverAddress,
      }),
    },
  ),
);
