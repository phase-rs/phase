import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { FormatConfig, GameFormat, MatchType, PlayerId } from "../adapter/types";
import { PROTOCOL_VERSION, type ServerInfo } from "../adapter/ws-adapter";
import {
  clearWsSession,
  loadWsSession,
  saveWsSession,
} from "../services/multiplayerSession";
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
 * The first frame the hosting WS would have sent (CreateGameWithSettings or
 * Reconnect) is deferred until the server's ServerHello is received, so a
 * version-mismatched connection never registers a lobby or claims a session.
 */
let hostHandshakeContinuation: (() => void) | null = null;

function sendClientHello(ws: WebSocket): void {
  ws.send(
    JSON.stringify({
      type: "ClientHello",
      data: {
        client_version: __APP_VERSION__,
        build_commit: __BUILD_HASH__,
        protocol_version: PROTOCOL_VERSION,
      },
    }),
  );
}

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
}

/**
 * Checks whether a lobby entry's host is running a compatible build with the
 * given server snapshot. Used by the lobby list to disable incompatible rows.
 * A missing `hostBuildCommit` (restored session, legacy entry) is treated as
 * unknown-but-allowed, matching the server's behavior at the join gate.
 */
export function isLobbyEntryCompatible(
  hostBuildCommit: string | undefined,
  serverInfo: ServerInfo | null,
): boolean {
  if (!serverInfo) return true;
  if (!hostBuildCommit) return true;
  return hostBuildCommit === serverInfo.buildCommit;
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

        // Shared message handler for both initial and reconnect WebSockets
        const handleHostMessage = (ws: WebSocket, msg: { type: string; data?: unknown }) => {
          if (msg.type === "ServerHello") {
            const data = msg.data as {
              server_version: string;
              build_commit: string;
              protocol_version: number;
              mode: "Full" | "LobbyOnly";
            };
            const info: ServerInfo = {
              version: data.server_version,
              buildCommit: data.build_commit,
              protocolVersion: data.protocol_version,
              mode: data.mode,
            };
            set({ serverInfo: info });
            if (info.protocolVersion !== PROTOCOL_VERSION) {
              hostHandshakeContinuation = null;
              ws.close();
              get().showToast(
                `Server protocol version ${info.protocolVersion} does not match client ${PROTOCOL_VERSION}. Please refresh.`,
              );
              get().cancelHosting();
              return;
            }
            const cont = hostHandshakeContinuation;
            hostHandshakeContinuation = null;
            cont?.();
          } else if (msg.type === "GameCreated") {
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
            const rws = new WebSocket(get().serverAddress);
            hostWs = rws;

            rws.onopen = () => {
              hostHandshakeContinuation = () => {
                rws.send(
                  JSON.stringify({
                    type: "Reconnect",
                    data: {
                      game_code: session.gameCode,
                      player_token: session.playerToken,
                    },
                  }),
                );
              };
              sendClientHello(rws);
            };

            rws.onmessage = (event) => {
              const msg = JSON.parse(event.data as string) as {
                type: string;
                data?: unknown;
              };
              handleHostMessage(rws, msg);
            };

            rws.onerror = () => {
              if (!gameStartedFired) {
                hostWs = null;
                attemptHostReconnect();
              }
            };

            rws.onclose = () => {
              if (!gameStartedFired && hostWs === rws) {
                hostWs = null;
                attemptHostReconnect();
              }
            };
          }, delay);
        };

        // Wire up the initial WebSocket handlers
        if (!isValidWebSocketUrl(get().serverAddress)) {
          set({
            hostGameCode: null,
            hostIsPublic: false,
            hostingStatus: "idle",
            playerSlots: [],
          });
          get().showToast("Invalid server address. Update it in Settings.");
          return;
        }
        const ws = new WebSocket(get().serverAddress);
        hostWs = ws;

        ws.onopen = () => {
          hostHandshakeContinuation = () => {
            ws.send(
              JSON.stringify({
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
            );
          };
          sendClientHello(ws);
        };

        ws.onmessage = (event) => {
          const msg = JSON.parse(event.data as string) as {
            type: string;
            data?: unknown;
          };
          handleHostMessage(ws, msg);
        };

        ws.onerror = () => {
          if (!gameStartedFired) {
            hostWs = null;
            attemptHostReconnect();
          }
        };

        ws.onclose = () => {
          if (!gameStartedFired && hostWs === ws) {
            hostWs = null;
            attemptHostReconnect();
          }
        };
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
        hostHandshakeContinuation = null;
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
