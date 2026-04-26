import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { FormatConfig, GameFormat, LobbyGame, MatchType, PlayerId } from "../adapter/types";
import { FORMAT_REGISTRY } from "../data/formatRegistry";
import { PROTOCOL_VERSION, type ServerInfo } from "../adapter/ws-adapter";
import {
  clearWsSession,
  loadWsSession,
  saveWsSession,
} from "../services/multiplayerSession";
import {
  lookupJoinTargetOver,
  openBrokerClient,
  resolveGuestOver,
  subscribeLobbyOver,
  type BrokerClient,
  type LookupJoinTargetResult,
  type RegisterHostRequest,
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
import type { P2PHostAdapter } from "../adapter/p2p-adapter";
import type {
  PlayerSlot,
  SeatMutation,
} from "../multiplayer/seatTypes";
export type { DeckChoice, PlayerSlot, SeatKind, SeatMutation } from "../multiplayer/seatTypes";

type ConnectionStatus = "disconnected" | "connecting" | "connected";
type HostingStatus = "idle" | "connecting" | "waiting";

// Module-level WebSocket ref (non-serializable, lives outside store)
let hostWs: WebSocket | null = null;
// Module-level broker client for P2P LobbyOnly hosting. Survives page
// navigations so the lobby entry stays alive while the tile is showing.
let activeBroker: BrokerClient | null = null;
let activeBrokerGameCode: string | null = null;
let activeP2PHostAdapter: P2PHostAdapter | null = null;
let activeP2PHostGameId: string | null = null;

function asDeckPayload(deck: HostingDeck): { main_deck: string[]; sideboard: string[]; commander: string[] } {
  return {
    main_deck: deck.main_deck,
    sideboard: deck.sideboard,
    commander: deck.commander,
  };
}
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
 * AbortControllers for in-flight join-adjacent RPCs (`resolveGuest`,
 * `lookupJoinTarget`). On the socket's `reconnecting` transition we abort
 * every pending call so the caller gets a `connection_lost` result
 * immediately rather than waiting for its own timeout. New calls after
 * reconnect use fresh controllers.
 */
const pendingJoinRpcAborts: Set<AbortController> = new Set();

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
  commander: string[];
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

/** Snapshot of the host's session config, captured at startHosting time.
 *  Immutable after creation — format lock prevents mid-wait changes. */
export interface HostSession {
  formatConfig: FormatConfig;
  timerSeconds: number | null;
  matchType: MatchType;
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
  // PlayerId → display name, captured from playerSlots at game start (ephemeral — not persisted)
  playerNames: Map<number, string>;
  // Per-player connection tracking (ephemeral — not persisted)
  disconnectedPlayers: Set<number>;
  // Action round-trip tracking (ephemeral — not persisted)
  actionPending: boolean;
  latencyMs: number | null;
  // Hosting session (ephemeral — not persisted)
  hostGameCode: string | null;
  hostIsPublic: boolean;
  hostingStatus: HostingStatus;
  hostSession: HostSession | null;
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
  openBroker: (req: RegisterHostRequest) => Promise<{ broker: BrokerClient; gameCode: string } | null>;
  closeBroker: () => void;
  getBroker: () => { broker: BrokerClient; gameCode: string } | null;
  startP2PHostingSession: (
    settings: HostingSettings,
    deck: HostingDeck,
    opts: { useBroker: boolean; roomName?: string | null },
  ) => Promise<boolean>;
  getActiveP2PHost: () => { adapter: P2PHostAdapter; gameId: string } | null;
  seatMutate: (mutation: SeatMutation) => void;
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
   * Read-only typed-code lookup. Returns format/routing metadata without
   * consuming a seat.
   */
  lookupJoinTarget: (
    code: string,
    password?: string,
  ) => Promise<LookupJoinTargetResult>;
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

// Build the FORMAT_DEFAULTS map from the engine-authored FORMAT_REGISTRY.
// Adding a user-selectable format only needs a registry entry; its default
// config flows here automatically. TwoHeadedGiant isn't in the registry
// (not user-selectable yet) but the enum variant is still valid and callers
// may look it up, so it's appended explicitly.
const TWO_HEADED_GIANT_DEFAULT: FormatConfig = {
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
};

export const FORMAT_DEFAULTS: Record<GameFormat, FormatConfig> = {
  ...(Object.fromEntries(
    FORMAT_REGISTRY.map((m) => [m.format, m.default_config]),
  ) as Record<Exclude<GameFormat, "TwoHeadedGiant">, FormatConfig>),
  TwoHeadedGiant: TWO_HEADED_GIANT_DEFAULT,
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
      playerNames: new Map(),
      disconnectedPlayers: new Set(),
      actionPending: false,
      latencyMs: null,
      hostGameCode: null,
      hostIsPublic: false,
      hostingStatus: "idle" as HostingStatus,
      hostSession: null,
      pendingGameRoute: null,
      serverInfo: null,

      setServerInfo: (info) => set({ serverInfo: info }),
      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (address) => set({ serverAddress: address }),
      setConnectionStatus: (status) => set({ connectionStatus: status }),
      setActivePlayerId: (id) => set({ activePlayerId: id }),
      setOpponentDisplayName: (name) => {
        const activeId = get().activePlayerId;
        const oppId = activeId != null ? (activeId === 0 ? 1 : 0) : null;
        const next = new Map(get().playerNames);
        if (name && oppId != null) next.set(oppId, name);
        const selfName = get().displayName;
        if (selfName && activeId != null) next.set(activeId, selfName);
        set({ opponentDisplayName: name, playerNames: next });
      },
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
          hostSession: {
            formatConfig: settings.formatConfig,
            timerSeconds: settings.timerSeconds,
            matchType: settings.matchType,
          },
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
            const names = new Map<number, string>();
            for (const slot of get().playerSlots) {
              if (slot.name) names.set(slot.playerId, slot.name);
            }
            // Reset hosting state FIRST so tile hides, then set route
            set({
              hostGameCode: null,
              hostingStatus: "idle",
              hostSession: null,
              playerNames: names,
              playerSlots: [],
              pendingGameRoute: `/game/${gameId}?mode=host`,
            });
          } else if (msg.type === "PlayerSlotsUpdate") {
            const data = msg.data as { slots: PlayerSlot[] };
            // Toast newly-arrived human guests so the host gets per-joiner
            // feedback in 3+ player lobbies. Without this, only the first
            // joiner is signaled (via the `gameCreated` → `GameStarted`
            // boundary in ws-adapter); subsequent guests appear silently in
            // the slot list. Diff against the prior `playerSlots` snapshot:
            // any slot whose seat transitioned from non-human-occupied to
            // `JoinedHuman` is a fresh guest.
            const prior = get().playerSlots;
            const newJoiners = data.slots.filter((slot) => {
              if (slot.kind.type !== "JoinedHuman") return false;
              const before = prior.find((p) => p.playerId === slot.playerId);
              return !before || before.kind.type !== "JoinedHuman";
            });
            set({ playerSlots: data.slots });
            for (const joiner of newJoiners) {
              get().showToast(`${joiner.name} joined the game.`);
            }
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
              hostSession: null,
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
              hostSession: null,
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
              deck: asDeckPayload(deck),
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
        if (activeP2PHostAdapter) {
          activeP2PHostAdapter.dispose();
          activeP2PHostAdapter = null;
          activeP2PHostGameId = null;
        }
        if (activeBroker) {
          if (activeBrokerGameCode) {
            void activeBroker.unregister(activeBrokerGameCode).catch(() => {});
          }
          activeBroker.close();
          activeBroker = null;
          activeBrokerGameCode = null;
        }
        gameStartedFired = false;
        hostReconnectAttempt = 0;
        clearWsSession();
        set({
          hostGameCode: null,
          hostIsPublic: false,
          hostingStatus: "idle",
          hostSession: null,
          playerSlots: [],
          pendingGameRoute: null,
        });
      },

      clearPendingGameRoute: () => set({ pendingGameRoute: null }),

      openBroker: async (req) => {
        if (activeBroker) {
          activeBroker.close();
          activeBroker = null;
          activeBrokerGameCode = null;
        }
        try {
          const broker = await openBrokerClient(get().serverAddress);
          const registered = await broker.registerHost(req);
          activeBroker = broker;
          activeBrokerGameCode = registered.gameCode;
          return { broker, gameCode: registered.gameCode };
        } catch (err) {
          console.error("[openBroker] failed:", err);
          return null;
        }
      },

      closeBroker: () => {
        activeBroker?.close();
        activeBroker = null;
        activeBrokerGameCode = null;
      },

      getBroker: () => {
        if (activeBroker && activeBrokerGameCode) {
          return { broker: activeBroker, gameCode: activeBrokerGameCode };
        }
        return null;
      },

      startP2PHostingSession: async (settings, deck, opts) => {
        const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
          import("../network/connection"),
          import("../adapter/p2p-adapter"),
        ]);

        if (activeP2PHostAdapter) {
          activeP2PHostAdapter.dispose();
          activeP2PHostAdapter = null;
          activeP2PHostGameId = null;
        }

        let broker: BrokerClient | null = null;
        let brokerGameCode: string | null = null;
        const host = await hostRoom(undefined, {});
        if (opts.useBroker) {
          try {
            broker = await openBrokerClient(get().serverAddress);
            const registered = await broker.registerHost({
              hostPeerId: host.peer.id,
              deck: asDeckPayload(deck),
              displayName: get().displayName || "Host",
              public: true,
              password: settings.password || null,
              timerSeconds: null,
              playerCount: settings.formatConfig.max_players,
              matchConfig: { match_type: settings.matchType },
              formatConfig: settings.formatConfig,
              aiSeats: [],
              roomName: opts.roomName ?? null,
            });
            brokerGameCode = registered.gameCode;
            activeBroker = broker;
            activeBrokerGameCode = registered.gameCode;
          } catch (err) {
            host.destroy();
            console.error("[startP2PHostingSession] broker registration failed:", err);
            return false;
          }
        }

        const gameId = crypto.randomUUID();
        const adapter = new P2PHostAdapter(
          {
            player: asDeckPayload(deck),
            opponent: { main_deck: [], sideboard: [], commander: [] },
            ai_decks: [],
          },
          host.peer,
          host.onGuestConnected,
          settings.formatConfig.max_players,
          settings.formatConfig,
          { match_type: settings.matchType },
          undefined,
          broker ?? undefined,
          false,
          brokerGameCode ?? undefined,
          {
            gameId,
            roomCode: host.roomCode,
            hostDisplayName: get().displayName || undefined,
          },
        );

        adapter.onEvent((event) => {
          if (event.type === "playerSlotsUpdated" || event.type === "lobbyProgress") {
            set({ playerSlots: adapter.getPlayerSlots() });
          } else if (event.type === "roomFull") {
            get().showToast("Room full — ready to start!");
          } else if (event.type === "error") {
            get().showToast(event.message);
          }
        });

        await adapter.initialize();
        activeP2PHostAdapter = adapter;
        activeP2PHostGameId = gameId;

        set({
          hostIsPublic: opts.useBroker,
          hostingStatus: "waiting",
          hostGameCode: host.roomCode,
          hostSession: {
            formatConfig: settings.formatConfig,
            timerSeconds: settings.timerSeconds,
            matchType: settings.matchType,
          },
          playerSlots: adapter.getPlayerSlots(),
        });
        return true;
      },

      getActiveP2PHost: () => {
        if (activeP2PHostAdapter && activeP2PHostGameId) {
          return { adapter: activeP2PHostAdapter, gameId: activeP2PHostGameId };
        }
        return null;
      },

      seatMutate: (mutation) => {
        if (activeP2PHostAdapter) {
          void (async () => {
            if (mutation.type === "Start") {
              await activeP2PHostAdapter.startPregameGame();
              const gameId = activeP2PHostGameId ?? crypto.randomUUID();
              saveActiveGame({ id: gameId, mode: "p2p-host", difficulty: "" });
              useGameStore.setState({ gameId });
              set({
                pendingGameRoute: `/game/${gameId}?mode=p2p-host`,
                hostGameCode: null,
                hostingStatus: "idle",
              });
            } else {
              await activeP2PHostAdapter.applySeatMutation(mutation);
              set({ playerSlots: activeP2PHostAdapter.getPlayerSlots() });
            }
          })().catch((err) => {
            get().showToast(err instanceof Error ? err.message : String(err));
          });
          return;
        }
        if (!hostWs || hostWs.readyState !== WebSocket.OPEN) {
          get().showToast("Host connection is not active.");
          return;
        }
        hostWs.send(JSON.stringify({
          type: "SeatMutate",
          data: { mutation },
        }));
      },

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
              // One retry on the initial open (~500ms to "offline") so the
              // user sees the `ServerOfflinePrompt` quickly when the server
              // is down, rather than after 6.5s of exponential backoff. The
              // prompt's "Keep trying" button remounts `LobbyView` and
              // starts a fresh retry cycle — recovery stays available.
              attempts: 1,
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
                  for (const ac of pendingJoinRpcAborts) ac.abort();
                  pendingJoinRpcAborts.clear();
                  // Drop the handle to the old socket's listener; it
                  // will be re-bound on the next "open".
                  lobbyAttachDetach = null;
                } else if (state === "offline") {
                  // Reconnect exhausted. Caller's `ensureSubscriptionSocket`
                  // resolves `null` so fallback UI renders. Also drain any
                  // stragglers that joined between reconnecting and offline.
                  for (const ac of pendingJoinRpcAborts) ac.abort();
                  pendingJoinRpcAborts.clear();
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
        for (const ac of pendingJoinRpcAborts) ac.abort();
        pendingJoinRpcAborts.clear();
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
        pendingJoinRpcAborts.add(ac);
        try {
          return await resolveGuestOver(socket, code, password, {
            signal: ac.signal,
          });
        } finally {
          pendingJoinRpcAborts.delete(ac);
        }
      },

      lookupJoinTarget: async (code, password) => {
        const socket = await get().ensureSubscriptionSocket();
        if (!socket) {
          return {
            ok: false,
            reason: "connection_lost",
            message: "Lobby connection unavailable. Check your server address.",
          };
        }
        const ac = new AbortController();
        pendingJoinRpcAborts.add(ac);
        try {
          return await lookupJoinTargetOver(socket, code, password, {
            signal: ac.signal,
          });
        } finally {
          pendingJoinRpcAborts.delete(ac);
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

export function getPlayerDisplayName(playerId: number, myId?: number): string {
  if (playerId === myId) return "You";
  const name = useMultiplayerStore.getState().playerNames.get(playerId);
  if (name) return name;
  return `Player ${playerId + 1}`;
}

export function getOpponentDisplayName(playerId: number): string {
  const name = useMultiplayerStore.getState().playerNames.get(playerId);
  if (name) return name;
  return `Opp ${playerId + 1}`;
}
