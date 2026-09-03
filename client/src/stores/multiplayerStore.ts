import { create } from "zustand";
import { persist } from "zustand/middleware";

import type { PlayerAvatarIdentity } from "../services/playerAvatars.ts";

import type {
  BuiltInGameFormat,
  CustomGameFormat,
  FormatConfig,
  GameFormat,
  LobbyGame,
  LoopDetectionMode,
  MatchType,
  PairingId,
  PlayerId,
  PodOutcome,
  TournamentCreatedReply,
  TournamentJoinedReply,
  TournamentSummary,
  TournamentUpdateReply,
} from "../adapter/types";
import { AdapterError, AdapterErrorCode, isCustomGameFormat } from "../adapter/types";
import { isFormatConfigShape } from "../adapter/format-config-shape";
import { findSavedCustomFormat } from "../services/customFormats";
import { AI_DIFFICULTIES } from "../constants/ai";
import { FORMAT_REGISTRY } from "../data/formatRegistry";
import { serverProtocolRejection, type ServerInfo } from "../adapter/ws-adapter";
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
  type LookupJoinTargetOptions,
  type LookupJoinTargetResult,
  type RegisterHostRequest,
  type ResolveResult,
} from "../services/brokerClient";
import {
  createTournamentOver,
  dropFromTournamentOver,
  endTournamentOver,
  getTournamentOver,
  joinTournamentOver,
  reportMatchResultOver,
  startTournamentRoundOver,
  subscribeTournamentsOver,
  type CreateTournamentRequest,
  type TournamentRpcResult,
  type TournamentSubscriptionHandlers,
} from "../services/tournamentClient";
import {
  HandshakeError,
  openPhaseSocket,
  withReconnect,
  type PhaseSocket,
  type PhaseSocketTransport,
  type ReconnectHandle,
} from "../services/openPhaseSocket";
import { isValidWebSocketUrl } from "../services/serverDetection";
import {
  DEFAULT_MULTIPLAYER_SERVER_URL,
  isOfficialMultiplayerServerUrl,
} from "../config/multiplayerServer";
import { saveActiveGame, useGameStore } from "./gameStore";
import { usePreferencesStore } from "./preferencesStore";
import {
  canAttemptNativeEngine,
  ensureNativeEngine,
  nativeEngineKeyForCurrentOrigin,
} from "../services/nativeEngine";
import type { P2PHostAdapter } from "../adapter/p2p-adapter";
import {
  ServerDraftAdapter,
  type CreateDraftSettings,
  type DraftPhase,
} from "../adapter/server-draft-adapter";
import type { DraftPlayerView } from "../adapter/draft-adapter";
import type {
  DeckChoice,
  PlayerSlot,
  SeatMutation,
} from "../multiplayer/seatTypes";
export type { DeckChoice, PlayerSlot, SeatKind, SeatMutation } from "../multiplayer/seatTypes";

type ConnectionStatus = "disconnected" | "connecting" | "connected";
type HostingStatus = "idle" | "connecting" | "waiting";

// Module-level WebSocket ref (non-serializable, lives outside store)
let hostWs: PhaseSocketTransport | null = null;
// Module-level broker client for P2P LobbyOnly hosting. Survives page
// navigations so the lobby entry stays alive while the tile is showing.
let activeBroker: BrokerClient | null = null;
let activeBrokerGameCode: string | null = null;
let activeP2PHostAdapter: P2PHostAdapter | null = null;
let activeP2PHostGameId: string | null = null;
let p2pHostingAttempt = 0;

function asDeckPayload(deck: HostingDeck): {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
  planar_deck: string[];
  scheme_deck: string[];
} {
  return {
    main_deck: deck.main_deck,
    sideboard: deck.sideboard,
    commander: deck.commander,
    planar_deck: deck.planar_deck ?? [],
    scheme_deck: deck.scheme_deck ?? [],
  };
}

function aiSeatDeckChoice(deckName: string | null): DeckChoice {
  if (!deckName || deckName.toLowerCase() === "random") {
    return { type: "Random" };
  }
  return { type: "Named", data: deckName };
}

function effectiveAiSeats(settings: HostingSettings): AiSeatConfig[] {
  return settings.formatConfig.team_based || settings.formatConfig.format === "Planechase"
    ? []
    : settings.aiSeats;
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
 * `lookupJoinTarget`) and for every tournament RPC issued through
 * {@link runTournamentRpc} (`createTournament`, `joinTournament`,
 * `getTournament`, `startTournamentRound`, `reportMatchResult`,
 * `dropFromTournament`, `endTournament`). On the socket's `reconnecting`
 * transition we abort every pending call so the caller gets a
 * `connection_lost` / `aborted` result immediately rather than waiting for its
 * own timeout. New calls after reconnect use fresh controllers.
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
 *
 * The reference count that governs the shared `SubscribeLobby` frame spans
 * this set **and** {@link tournamentSubscribers}; see
 * {@link lobbySubscriptionRefCount}.
 */
const lobbySubscribers: Set<(games: LobbyGame[]) => void> = new Set();
/** Most recent `LobbyUpdate` snapshot, used to seed new subscribers. */
let lobbySnapshot: LobbyGame[] | null = null;

/** Lobby row for a game/draft code from the cached subscription snapshot. */
export function findLobbyGameByCode(code: string): LobbyGame | undefined {
  const normalized = code.trim().toUpperCase();
  return lobbySnapshot?.find((g) => g.game_code.toUpperCase() === normalized);
}
/** Per-socket detach returned by `subscribeLobbyOver`. Re-bound on
 * reconnect; `null` when no socket is attached. */
let lobbyAttachDetach: (() => void) | null = null;

/**
 * Registered tournament-broadcast subscribers. Handlers rather than one
 * callback because the three broadcast streams (`TournamentListUpdate`,
 * `TournamentUpdate`, `TournamentRemoved`) are independent and a caller
 * usually renders only one of them.
 *
 * This set is the SECOND half of the shared-subscription reference count —
 * see {@link lobbySubscriptionRefCount}.
 */
const tournamentSubscribers: Set<TournamentSubscriptionHandlers> = new Set();

/**
 * Most recent `TournamentListUpdate`, used to seed subscribers that attach
 * after the push has already arrived.
 *
 * A verbatim cache, never a reduction: the broker sends the whole sorted list
 * every time (`tournament_summaries()`) and there are no add/update/remove
 * delta frames, so folding anything in here would be inventing a delta
 * protocol the server does not speak. In particular `onTournamentRemoved` must
 * NOT filter this array — a removed tournament stays in the cached list until
 * the server's next `TournamentListUpdate` replaces it wholesale. Cleared
 * whenever the shared subscription is released, so a reconnect can never serve
 * a stale list.
 */
let tournamentListSnapshot: TournamentSummary[] | null = null;

/** Per-socket detach returned by `subscribeTournamentsOver`. Re-bound on
 *  reconnect; `null` when no socket is attached. Sends nothing by design —
 *  the frames belong to {@link detachSharedSubscription}. */
let tournamentAttachDetach: (() => void) | null = null;

/**
 * The single reference count governing the shared `SubscribeLobby` /
 * `UnsubscribeLobby` pair.
 *
 * Broker-side those frames are per-CONNECTION, not per-subscriber:
 * `SubscribeLobby` inserts this connection's sender into one delivery set
 * (`AddSubscriber`) and `UnsubscribeLobby` removes it (`RemoveSubscriber`),
 * so a single removal silences every stream riding this socket regardless of
 * how many subscribes preceded it. Lobby and tournament subscribers therefore
 * cannot each keep their own count — the last subscriber of EITHER kind is the
 * one that may release.
 *
 * Derived from set membership rather than an incremented integer on purpose:
 * `add`/`delete` are idempotent, so a double-subscribe cannot inflate the
 * count and a double-release cannot drive it negative and strand the
 * subscription. Callers (including React cleanups that may run twice) need no
 * discipline for that to hold.
 */
function lobbySubscriptionRefCount(): number {
  return lobbySubscribers.size + tournamentSubscribers.size;
}

/**
 * Binds both listeners to `socket` and puts `SubscribeLobby` on the wire.
 *
 * The frame is emitted as a side effect of `subscribeLobbyOver`, which owns
 * both frames (`services/brokerClient.ts`). Attaching the TOURNAMENT listener
 * here — rather than when the first tournament subscriber appears — is
 * load-bearing, not tidiness: `SubscribeLobby` triggers exactly one
 * `ToSelf(TournamentListUpdate)`, there is no request that re-fetches the
 * list, and the next list push only happens when some other actor mutates a
 * tournament. A store that attached the tournament listener later would
 * silently drop that one push whenever a lobby subscriber acquired the
 * subscription first, and the tournament page would render an empty list
 * indefinitely.
 *
 * For the same reason the two statements below are in this order and must
 * stay in it: the tournament listener is registered BEFORE the statement that
 * provokes the reply it needs to catch, because `subscribeLobbyOver`'s own
 * `ws.send` is what puts `SubscribeLobby` on the wire. Binding it afterwards
 * would happen to work only by relying on an unwritten, untested fact — that
 * a `send` cannot deliver its reply within the same synchronous execution
 * block — which a future refactor (an async `subscribeTournamentsOver`, a
 * transport that dispatches on a microtask) could quietly invalidate. This
 * ordering makes the invariant structural instead.
 */
function attachSharedSubscription(
  socket: PhaseSocket,
  set: MultiplayerSet,
  get: MultiplayerGet,
): void {
  tournamentAttachDetach = subscribeTournamentsOver(socket, {
    onListUpdate: (tournaments) => {
      tournamentListSnapshot = tournaments;
      for (const h of tournamentSubscribers) h.onListUpdate?.(tournaments);
    },
    onTournamentUpdate: (code, view) => {
      for (const h of tournamentSubscribers) h.onTournamentUpdate?.(code, view);
    },
    onTournamentRemoved: (code) => {
      // The tournament is gone server-side; its tokens can never authorize
      // anything again. Dropped here because this fan-out is the one place
      // every `TournamentRemoved` arrives, and it stays attached for the whole
      // life of the shared subscription — so the cleanup happens even when no
      // page is currently subscribed. That lifetime is also why the helper
      // must not write when it holds nothing for `code`.
      //
      // Deliberately does NOT touch `tournamentListSnapshot`: that cache is a
      // verbatim copy of the server's last list push, and filtering it here
      // would invent a delta protocol the broker does not speak.
      forgetTournamentCredential(set, get, code);
      for (const h of tournamentSubscribers) h.onTournamentRemoved?.(code);
    },
  });
  lobbyAttachDetach = subscribeLobbyOver(socket, (games) => {
    lobbySnapshot = games;
    for (const cb of lobbySubscribers) cb(games);
  });
}

/** Releases the shared subscription: detaches both listeners, sends
 *  `UnsubscribeLobby` (via `subscribeLobbyOver`'s detach, which no-ops on a
 *  socket that is no longer OPEN), and drops both cached snapshots. */
function detachSharedSubscription(): void {
  tournamentAttachDetach?.();
  tournamentAttachDetach = null;
  lobbyAttachDetach?.();
  lobbyAttachDetach = null;
  lobbySnapshot = null;
  tournamentListSnapshot = null;
}

/**
 * Acquires the shared subscription if it is not already bound to a socket.
 * Call AFTER adding the new subscriber to its set.
 *
 * The predicate is "is it attached?", not "is the count exactly 1": across a
 * reconnect the count is legitimately > 0 while the handle is null, and
 * `onStateChange`'s "open" branch re-acquires through this same function.
 */
function acquireLobbySubscription(
  socket: PhaseSocket,
  set: MultiplayerSet,
  get: MultiplayerGet,
): void {
  if (lobbyAttachDetach !== null) return;
  attachSharedSubscription(socket, set, get);
}

/**
 * Releases the shared subscription once no subscriber of EITHER kind remains.
 * Call AFTER removing the departing subscriber from its set.
 */
function releaseLobbySubscription(): void {
  if (lobbySubscriptionRefCount() > 0) return;
  detachSharedSubscription();
}

/**
 * Drops a tournament's credentials, and genuinely no-ops when nothing is held
 * for `code` — no `set` call, therefore no `persist` write.
 *
 * The presence test reads through `get` rather than being made inside the
 * updater. Returning `{}` from a zustand updater does leave the state
 * reference unchanged (so credential consumers do not re-render), but the
 * `set` still runs and `persist` still serializes the whole partition to
 * `localStorage`. This fan-out is attached for the entire life of the shared
 * subscription and fires for every `TournamentRemoved` on the server —
 * including the overwhelming majority this browser holds no credential for —
 * so the miss path has to be free.
 */
function forgetTournamentCredential(
  set: MultiplayerSet,
  get: MultiplayerGet,
  code: string,
): void {
  if (!(code in get().tournamentCredentials)) return;
  set((state) => {
    const next = { ...state.tournamentCredentials };
    delete next[code];
    return { tournamentCredentials: next };
  });
}

/**
 * Which authority a gated tournament RPC requires. A closed union naming the
 * domain concept, not the storage field: adding a third authority later is a
 * new member plus one compile error at the switch below, not a third runner.
 */
export type TournamentRole = "organizer" | "player";

/**
 * A gated action refused by THIS STORE, before any frame existed.
 *
 * Deliberately not `reason: "rejected"`. `TournamentRpcFailureReason` is the
 * WIRE vocabulary — each of its four members documents something the transport
 * or the broker did, and `"rejected"` specifically means "the broker answered
 * `Error`; `message` is its text verbatim" (`services/tournamentClient.ts`).
 * A local refusal contacted no broker and carries client-authored copy, so
 * filing it under `"rejected"` would both falsify that contract and leave a
 * consumer no way to tell the two apart except by matching English message
 * text. `role` is carried so a consumer can pick its copy (and later, its
 * i18n key) from a typed field rather than from the message.
 *
 * It lives here rather than as a fifth `TournamentRpcFailureReason` member
 * because `tournamentClient.ts` is the wire layer and this is a store-level
 * fact — and because that file is frozen by the time this store is written.
 */
export interface TournamentNotAuthorized {
  ok: false;
  reason: "not_authorized";
  /** The authority that was required and not held. */
  role: TournamentRole;
  /** Human-readable fallback. Phase 3/5 replace this with a `t()` lookup keyed
   *  off {@link TournamentNotAuthorized.role}; see the i18n boundary note. */
  message: string;
}

/**
 * What a token-gated tournament action resolves to: the wire result, widened
 * by exactly one locally-produced failure member. Every failure member keeps
 * the same `{ ok: false; reason; message }` skeleton, so `if (!r.ok)`
 * narrowing works uniformly and `r.reason === "not_authorized"` narrows
 * further to the member carrying `role`.
 */
export type GatedTournamentRpcResult<T> =
  | TournamentRpcResult<T>
  | TournamentNotAuthorized;

/**
 * Single authority for giving a tournament RPC its socket and its abort
 * registration. Follows `resolveGuest` exactly: acquire lazily, register an
 * `AbortController` so a `reconnecting` transition or a teardown cuts the wait
 * short, and remove it in `finally`. It never closes a socket — the socket
 * belongs to `ensureSubscriptionSocket` / `closeSubscriptionSocket`.
 */
async function runTournamentRpc<T>(
  get: MultiplayerGet,
  send: (
    socket: PhaseSocket,
    signal: AbortSignal,
  ) => Promise<TournamentRpcResult<T>>,
): Promise<TournamentRpcResult<T>> {
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
    return await send(socket, ac.signal);
  } finally {
    pendingJoinRpcAborts.delete(ac);
  }
}

/**
 * Single authority for token-gated tournament RPCs. Resolves the required
 * authority for `code` and refuses locally when it is absent — before any
 * socket is opened, so a call with no credential costs nothing and puts
 * nothing on the wire.
 *
 * Call sites never read `tournamentCredentials` themselves: a caller that
 * inspects which token an action needs is one refactor away from sending the
 * wrong tournament's token, and a caller that re-reads the map to explain a
 * failure has become a second authority that can disagree with this one (the
 * fan-out deletes entries asynchronously).
 *
 * Two distinguishable failure shapes, deliberately:
 *  - `{reason: "not_authorized", role}` — decided HERE, from this store's own
 *    map, with certainty. Nothing was sent.
 *  - any `TournamentRpcFailureReason` — decided by the transport or the
 *    broker. Note in particular that `"rejected"` inherits the caution below
 *    and is NOT a reliable "the server refused me" signal.
 *
 * Caution for consumers (`services/tournamentClient.ts`, module header part 4):
 * the four gated RPCs settle on a `TournamentUpdate` BROADCAST, which carries
 * no request-vs-broadcast discriminator, so a wire-level `{ok:false}` here is
 * not a reliable "the server rejected me" signal. Nothing in this store mutates
 * state on a gated failure for exactly that reason.
 */
async function runGatedTournamentRpc<T>(
  get: MultiplayerGet,
  code: string,
  role: TournamentRole,
  send: (
    socket: PhaseSocket,
    token: string,
    signal: AbortSignal,
  ) => Promise<TournamentRpcResult<T>>,
): Promise<GatedTournamentRpcResult<T>> {
  const held = get().tournamentCredentials[code];
  let token: string | undefined;
  switch (role) {
    case "organizer":
      token = held?.organizerToken;
      break;
    case "player":
      token = held?.playerToken;
      break;
  }
  if (token === undefined) {
    return {
      ok: false,
      reason: "not_authorized",
      role,
      message:
        role === "organizer"
          ? "You are not the organizer of this tournament."
          : "You are not entered in this tournament.",
    };
  }
  const heldToken = token;
  return runTournamentRpc(get, (socket, signal) =>
    send(socket, heldToken, signal),
  );
}

export interface AiSeatConfig {
  seatIndex: number;
  difficulty: string;
  deckName: string | null;
  deck?: DeckChoice;
}

export interface HostingDeck {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
  planar_deck?: string[];
  scheme_deck?: string[];
}

/** Persisted snapshot of the host-setup form so the lobby remembers the
 *  player's last choices across sessions instead of resetting to defaults.
 *  Deliberately excludes per-match / sensitive fields (room name, password):
 *  those are re-entered each time the player hosts. */
export interface RememberedHostConfig {
  format: GameFormat;
  formatConfig: FormatConfig;
  /**
   * WHICH saved custom-format definition `format` refers to, or `null` for a
   * built-in format.
   *
   * Not redundant with `format`/`formatConfig.custom_rules.id`: every Axis-A
   * lobby save carries the engine's reserved sentinel
   * `LOBBY_SAVE_CUSTOM_FORMAT_ID` (`CustomFormatId(0)`) by design, so the
   * engine id is `0` — and the format string `"Custom:0"` — for ALL of them and
   * can never distinguish two saved formats from each other. Only the
   * client-generated id from `services/customFormats.ts` can.
   */
  savedCustomFormatId: string | null;
  playerCount: number;
  matchType: MatchType;
  /** CR 732.2a: combo (infinite-loop) detector opt-in, chosen at match creation. */
  loopDetection: LoopDetectionMode;
  isPublic: boolean;
  startWhenFull: boolean;
  ranked: boolean;
  /** AI seat layout (seat index + difficulty). Deck choices are resolved fresh
   *  from the catalog at host time, so only the picker-level config persists. */
  aiSeats: AiSeatConfig[];
}

export interface HostingSettings {
  displayName: string;
  public: boolean;
  password: string;
  timerSeconds: number | null;
  formatConfig: FormatConfig;
  matchType: MatchType;
  /** CR 732.2a: combo (infinite-loop) detector opt-in, chosen at match creation. */
  loopDetection: LoopDetectionMode;
  aiSeats: AiSeatConfig[];
  startWhenFull: boolean;
  /** Optional per-match label shown in the lobby, distinct from `displayName`
   * (the player's global identity). `null` means "use the player's name". */
  roomName: string | null;
  /** Enable ranked rating updates for the room. */
  ranked: boolean;
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
  /** Last host-setup form choices, persisted across sessions. `null` until the
   *  player has hosted at least once. See {@link RememberedHostConfig}. */
  lastHostConfig: RememberedHostConfig | null;
  /**
   * Tournament code → bearer credentials this browser holds. Persisted:
   * `organizer_token` and `player_token` are minted once in a point reply and
   * never re-sent, so losing them is unrecoverable. A plain object, not a
   * `Map` — `partialize` runs through JSON, where a `Map` serializes to `{}`.
   * Bounded by {@link MAX_TOURNAMENT_CREDENTIALS}; entries are dropped on
   * `TournamentRemoved`.
   */
  tournamentCredentials: Record<string, TournamentCredential>;
  playerSlots: PlayerSlot[];
  spectators: string[];
  isSpectator: boolean;
  // PlayerId → display name, captured from playerSlots at game start (ephemeral — not persisted)
  playerNames: Map<number, string>;
  // PlayerId → semantic avatar identity (ephemeral — assigned at game start)
  playerAvatars: Map<number, PlayerAvatarIdentity>;
  compatibilityPlayerCount: number | null;
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
  // Server-hosted draft session (ephemeral — not persisted)
  draftAdapter: ServerDraftAdapter | null;
  draftView: DraftPlayerView | null;
  draftPhase: DraftPhase | null;
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
  setCompatibilityPlayerCount: (count: number | null) => void;
  rememberHostConfig: (config: RememberedHostConfig) => void;
  clearRememberedHostConfig: () => void;
  setPlayerSlots: (slots: PlayerSlot[]) => void;
  setSpectators: (names: string[]) => void;
  setIsSpectator: (value: boolean) => void;
  setPlayerDisconnected: (playerId: number) => void;
  setPlayerReconnected: (playerId: number) => void;
  setActionPending: (pending: boolean) => void;
  setLatency: (ms: number | null) => void;
  // Hosting session actions
  startHosting: (settings: HostingSettings, deck: HostingDeck) => void;
  resumeServerHosting: () => boolean;
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
  /**
   * Transfers the pre-game host adapter to the matching game route. Once
   * claimed, the game provider is its sole owner and lobby cleanup cannot
   * later leave a disposed adapter available for a remount.
   */
  takeActiveP2PHost: (gameId: string) => P2PHostAdapter | null;
  seatMutate: (mutation: SeatMutation) => void;
  /** Like `seatMutate` but awaits P2P work; server sends are still fire-and-forget. */
  seatMutateAsync: (mutation: SeatMutation) => Promise<void>;
  /** Remove open seats, then start — mutations run in order (fixes Start-now races). */
  startLobbyWithCurrentPlayers: () => Promise<void>;
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
    opts?: Pick<
      LookupJoinTargetOptions,
      "reserve" | "displayName" | "releaseReservationToken"
    >,
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
  /**
   * Subscribe to tournament broadcasts over the shared subscription socket.
   * Returns a detach function, or `null` when the socket could not be opened.
   *
   * Shares ONE `SubscribeLobby` reference count with {@link subscribeLobby}:
   * the first subscriber of either kind sends the frame and only the last one
   * of either kind sends `UnsubscribeLobby`. Callers should not await the
   * result before their cleanup can run — follow `LobbyView.tsx`'s
   * `if (cancelled) { detach?.(); return; }` idiom.
   */
  subscribeTournaments: (
    handlers: TournamentSubscriptionHandlers,
  ) => Promise<(() => void) | null>;
  /** Create a tournament and remember its organizer token. */
  createTournament: (
    req: CreateTournamentRequest,
  ) => Promise<TournamentRpcResult<TournamentCreatedReply>>;
  /** Join a tournament and remember its player token and player key. */
  joinTournament: (
    code: string,
    displayName?: string,
  ) => Promise<TournamentRpcResult<TournamentJoinedReply>>;
  /** Fetch one tournament's current view. Ungated — codes are public. */
  getTournament: (
    code: string,
  ) => Promise<TournamentRpcResult<TournamentUpdateReply>>;
  /**
   * Organizer-gated. When no organizer token is held for `code` this resolves
   * `{ok:false, reason:"not_authorized", role:"organizer"}` locally, with no
   * wire traffic — a shape distinct from every `TournamentRpcFailureReason`,
   * so a consumer can pick "you are not the organizer" copy without inspecting
   * `message`.
   */
  startTournamentRound: (
    code: string,
  ) => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>;
  /** Organizer-gated, same local-refusal contract. */
  endTournament: (
    code: string,
  ) => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>;
  /** Player-gated; local refusal carries `role: "player"`. */
  reportMatchResult: (
    code: string,
    pairingId: PairingId,
    outcome: PodOutcome,
  ) => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>;
  /** Player-gated, same local-refusal contract. */
  dropFromTournament: (
    code: string,
  ) => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>;
  /**
   * Join a server-hosted draft room. Creates a ServerDraftAdapter and uses
   * its joinDraft method, then stores the adapter and initial view.
   */
  joinServerDraft: (
    serverUrl: string,
    draftCode: string,
    displayName: string,
    password?: string,
  ) => Promise<void>;
  /**
   * Create a new server-hosted draft pod. Opens a ServerDraftAdapter and
   * calls createDraft with the given settings.
   */
  createServerDraft: (
    serverUrl: string,
    settings: CreateDraftSettings,
  ) => Promise<void>;
}

function disposeActiveP2PHost(): void {
  if (activeP2PHostAdapter) {
    activeP2PHostAdapter.dispose();
    activeP2PHostAdapter = null;
    activeP2PHostGameId = null;
  }
}

function closeHostWebSocket(): void {
  if (hostReconnectTimer) {
    clearTimeout(hostReconnectTimer);
    hostReconnectTimer = null;
  }
  if (hostWs) {
    hostWs.close();
    hostWs = null;
  }
}

function activeServerHostingSocket(get: () => MultiplayerState): PhaseSocketTransport | null {
  if (hostWs) {
    if (hostWs.readyState !== WebSocket.OPEN) {
      throw new Error("Host connection is not active.");
    }
    return hostWs;
  }
  if (
    get().hostingStatus === "waiting" &&
    get().hostGameCode != null &&
    !activeP2PHostAdapter
  ) {
    throw new Error("Host connection is not active.");
  }
  return null;
}

async function runP2PSeatMutation(
  mutation: SeatMutation,
  set: (partial: Partial<MultiplayerState>) => void,
): Promise<void> {
  const adapter = activeP2PHostAdapter;
  if (!adapter) {
    throw new Error("P2P host is not active.");
  }
  if (mutation.type === "Start") {
    adapter.startNow();
    await startActiveP2PHostGame(set);
  } else {
    await adapter.applySeatMutation(mutation);
    set({ playerSlots: adapter.getPlayerSlots() });
  }
}

async function startActiveP2PHostGame(
  setState: (partial: Partial<MultiplayerState>) => void,
): Promise<void> {
  const adapter = activeP2PHostAdapter;
  if (!adapter) return;

  await adapter.startPregameGame();
  const gameId = activeP2PHostGameId ?? crypto.randomUUID();
  saveActiveGame({ id: gameId, mode: "p2p-host", difficulty: "" });
  useGameStore.setState({ gameId });
  setState({
    activePlayerId: 0,
    pendingGameRoute: `/game/${gameId}?mode=p2p-host`,
    hostGameCode: null,
    hostingStatus: "idle",
  });
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

/**
 * True when the client's wire-protocol can speak to `info` on the FULL-GAME
 * surface — the surface that decides whether a game can actually be played.
 * Delegates to `serverProtocolRejection` — the same decision the game
 * handshake makes — so the compatibility badge can never disagree with whether
 * the connection actually succeeds. A `LobbyOnly` server has no full-game
 * surface, so it is judged on its lobby version instead.
 */
export function isServerCompatible(info: ServerInfo | null): boolean {
  return info !== null && serverProtocolRejection(info) === null;
}

// Build the FORMAT_DEFAULTS map from the engine-authored FORMAT_REGISTRY.
// Adding a user-selectable format only needs a registry entry; its default
// config flows here automatically.
export const FORMAT_DEFAULTS: Record<GameFormat, FormatConfig> = Object.fromEntries(
  FORMAT_REGISTRY.map((m) => [m.format, m.default_config]),
) as Record<GameFormat, FormatConfig>;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object";
}

/**
 * Per-tournament bearer credentials this browser holds.
 *
 * The two token fields are independently optional on purpose, and this is NOT
 * a discriminated union waiting to be tidied into one: an organizer may also
 * join their own event, so one code can legitimately carry BOTH authorities at
 * once. This is the normal path, not an exotic one — `CreateTournament` does
 * not auto-join the creator, so an organizer who also wants to play issues a
 * separate `JoinTournament` on the same code. Each token is minted by the
 * broker in a point reply (`TournamentCreated.organizer_token`,
 * `TournamentJoined.player_token`) and is never broadcast — losing it is
 * unrecoverable, which is why this map is persisted rather than held in memory.
 */
export interface TournamentCredential {
  /** Organizer authority for this code. Present iff this browser created it. */
  organizerToken?: string;
  /** Entrant authority for this code. Present iff this browser joined it. */
  playerToken?: string;
  /**
   * The `player_key` this browser joined under — the identity every later
   * `TournamentView` keys on (`PlayerSummary.player_key`). Stored beside the
   * token rather than re-derived from `playerId` at read time so "which entrant
   * am I in THIS event" stays answerable even if the ambient id ever changes.
   */
  playerKey?: string;
  /** ms epoch of the last write. The eviction key; never rendered. */
  updatedAt: number;
}

/**
 * Cap on retained tournament credentials. Bounded because this map is
 * persisted and grows once per event the player touches, with no natural
 * shrink other than `TournamentRemoved` (which only fires while subscribed).
 */
export const MAX_TOURNAMENT_CREDENTIALS = 32;

/**
 * Trims the credential map to {@link MAX_TOURNAMENT_CREDENTIALS}, evicting
 * least-recently-written first.
 *
 * `protect` is never evicted — without it a write made under a frozen or
 * coarse clock (every entry sharing one `updatedAt`) could evict the very
 * entry that caused the overflow, whenever that entry also happens to sort
 * first by `code`.
 *
 * Ordering is `(updatedAt, code)`. `updatedAt` is the real key and carries the
 * LRU semantics; `code` is a pure tiebreak, present so that a clock tie cannot
 * hand the decision to `Object.keys`. Key order is not a safe fallback: JS
 * enumerates *canonical array-index* string keys ("9", "40" — strings that
 * round-trip through `ToString(ToUint32(k))`) in ascending numeric order ahead
 * of every other key's insertion order, so an all-digit tournament code would
 * otherwise make eviction depend on how the code happens to spell a number.
 * Note this hazard applies only to unpadded codes: "0001" does not round-trip
 * and is therefore insertion-ordered like any other string.
 */
function capTournamentCredentials(
  map: Record<string, TournamentCredential>,
  protect?: string,
): Record<string, TournamentCredential> {
  const codes = Object.keys(map);
  const overflow = codes.length - MAX_TOURNAMENT_CREDENTIALS;
  if (overflow <= 0) return map;

  const victims = codes
    .filter((code) => code !== protect)
    .sort(
      (a, b) =>
        map[a].updatedAt - map[b].updatedAt || (a < b ? -1 : a > b ? 1 : 0),
    )
    .slice(0, overflow);

  const next = { ...map };
  for (const victim of victims) delete next[victim];
  return next;
}

/**
 * Returns a new credential map with `patch` merged into `code`'s entry.
 *
 * Merging, not replacing: create-then-join on the same code accumulates both
 * authorities (see {@link TournamentCredential}). `now` is injectable so the
 * eviction tests are deterministic.
 */
export function rememberTournamentCredential(
  existing: Readonly<Record<string, TournamentCredential>>,
  code: string,
  patch: Omit<Partial<TournamentCredential>, "updatedAt">,
  now: number = Date.now(),
): Record<string, TournamentCredential> {
  const merged: Record<string, TournamentCredential> = {
    ...existing,
    [code]: { ...existing[code], ...patch, updatedAt: now },
  };
  return capTournamentCredentials(merged, code);
}

/**
 * Rehydration guard. Persisted state is external input (see this store's
 * `merge`), so a blob may be hand-edited, truncated, or written by a build
 * whose shape or cap differed. Entries carrying no authority at all are
 * dropped: a credential with neither token is not a credential.
 *
 * Accepted edge case, stated rather than guarded: an array also satisfies the
 * object check `isRecord` performs, so `normalizeTournamentCredentials([...])`
 * clears the top-level guard and enumerates numeric indices as if they were
 * tournament codes. Each such "entry" would still have to be an object
 * carrying a string `organizerToken` or `playerToken` to survive the per-entry
 * validation below, so the result is a narrow, harmless edge case rather than
 * a functional gap.
 *
 * `isRecord` is deliberately NOT narrowed to fix this. It is file-local and in
 * this phase's scope, but it has five other callers —
 * `normalizeRememberedHostConfig`, the `formatConfig` / `deck_size` projection,
 * the `loopDetection` guard and the seat validation — whose current behavior,
 * array-acceptance included, is load-bearing for the remembered-host-config and
 * migration paths. Tightening a shared predicate for one new caller's benefit is
 * an unscoped behavior change to five unrelated call sites, which is not
 * something this change should do as a side effect of adding a sixth.
 */
export function normalizeTournamentCredentials(
  persisted: unknown,
): Record<string, TournamentCredential> {
  if (!isRecord(persisted)) return {};
  const out: Record<string, TournamentCredential> = {};
  for (const [code, raw] of Object.entries(persisted)) {
    if (!isRecord(raw)) continue;
    const organizerToken =
      typeof raw.organizerToken === "string" ? raw.organizerToken : undefined;
    const playerToken =
      typeof raw.playerToken === "string" ? raw.playerToken : undefined;
    const playerKey =
      typeof raw.playerKey === "string" ? raw.playerKey : undefined;
    if (organizerToken === undefined && playerToken === undefined) continue;
    out[code] = {
      ...(organizerToken !== undefined ? { organizerToken } : {}),
      ...(playerToken !== undefined ? { playerToken } : {}),
      ...(playerKey !== undefined ? { playerKey } : {}),
      updatedAt:
        typeof raw.updatedAt === "number" && Number.isFinite(raw.updatedAt)
          ? raw.updatedAt
          : 0,
    };
  }
  return capTournamentCredentials(out);
}

function isIntegerInRange(value: unknown, upperBound: number): value is number {
  return typeof value === "number"
    && Number.isInteger(value)
    && value > 0
    && value <= upperBound;
}

function isI32(value: unknown): value is number {
  return isIntegerInRange(value, 2_147_483_647);
}

function isU16(value: unknown): value is number {
  return isIntegerInRange(value, 65_535);
}

function isU8(value: unknown): value is number {
  return isIntegerInRange(value, 255);
}

/**
 * True for a BUILT-IN format the engine registry knows. Deliberately false for
 * every `Custom:<id>` string: `FORMAT_DEFAULTS` is built from the built-in
 * registry and has no entry for one, so this is exactly the predicate that must
 * guard any `FORMAT_DEFAULTS[...]` lookup driven by a stored or user-selected
 * format. Exported because `HostSetup` needs the same guard before its own
 * seat-ceiling lookup.
 */
export function isKnownFormat(value: unknown): value is BuiltInGameFormat {
  return typeof value === "string"
    && Object.prototype.hasOwnProperty.call(FORMAT_DEFAULTS, value);
}

/**
 * Rebuilds the engine-authored part of a persisted host setting from the
 * current format registry. The browser is a durable storage boundary, so a
 * previous release's serialized `FormatConfig` must never be sent straight
 * back to a newer engine protocol.
 *
 * Only fields the host setup currently lets a player customize survive this
 * projection. Every structural/derived field comes from the current engine
 * default, which makes added and reshaped engine fields self-healing on the
 * next hydration rather than requiring a one-off migration per field.
 */
export function normalizeRememberedHostConfig(
  persisted: unknown,
): RememberedHostConfig | null {
  if (!isRecord(persisted)) return null;

  if (isKnownFormat(persisted.format)) {
    return normalizeBuiltInHostConfig(persisted, persisted.format);
  }
  if (isCustomGameFormat(persisted.format)) {
    return normalizeCustomHostConfig(persisted, persisted.format);
  }
  return null;
}

/**
 * Rehydration for a CUSTOM-format remembered config.
 *
 * Before this branch existed, `isKnownFormat` returned false for every
 * `Custom:<id>` string and the whole remembered config — player count, AI
 * seats, privacy, everything — was discarded whenever the player's last hosted
 * game used a custom format. That is silent data loss, not just a missing
 * format.
 *
 * The projection a built-in gets (rebuild from the current registry default,
 * keep only the customizable fields) is impossible here: a custom format has no
 * registry entry to rebuild from, and its only source of truth is its own saved
 * `CustomFormatRules`. Resolving those to a `FormatConfig` needs
 * `FormatConfig::for_custom_rules`, which lives in WASM — and this function
 * runs SYNCHRONOUSLY inside `set()` and cannot await. So instead:
 *
 *  1. Resolve WHICH saved definition this was, through `customFormats.ts`'s
 *     synchronous local read. Gone (deleted, or another device) → `null`.
 *  2. Structurally revalidate the persisted `FormatConfig` blob against today's
 *     client-side schema before trusting it back.
 *
 * Step 2 proves "this blob still matches today's serialization schema", NOT
 * "the engine still agrees these rules are legal". That is sufficient here
 * because a saved `CustomFormatRules` is immutable once saved in this phase —
 * no edit flow exists — and because the config is re-validated for real by the
 * engine's own `FormatConfig` deserializer at every boundary it later crosses.
 *
 * Any failure degrades to `null`, exactly like every other unresolvable case.
 */
function normalizeCustomHostConfig(
  persisted: Record<string, unknown>,
  format: CustomGameFormat,
): RememberedHostConfig | null {
  const savedCustomFormatId = persisted.savedCustomFormatId;
  if (typeof savedCustomFormatId !== "string") return null;
  if (!findSavedCustomFormat(savedCustomFormatId)) return null;

  const storedFormatConfig = persisted.formatConfig;
  if (!isFormatConfigShape(storedFormatConfig)) return null;
  // The blob must describe the format it is filed under. `isFormatConfigShape`
  // already ties `format` to `custom_rules.id`; this ties both to the key the
  // rest of the remembered config is keyed on.
  if (storedFormatConfig.format !== format) return null;

  return finalizeRememberedHostConfig(
    persisted,
    format,
    storedFormatConfig,
    savedCustomFormatId,
  );
}

function normalizeBuiltInHostConfig(
  persisted: Record<string, unknown>,
  format: BuiltInGameFormat,
): RememberedHostConfig {
  const defaults = FORMAT_DEFAULTS[format];
  const storedFormatConfig = isRecord(persisted.formatConfig)
    ? persisted.formatConfig
    : {};
  const storedDeckSize = isRecord(storedFormatConfig.deck_size)
    ? storedFormatConfig.deck_size
    : null;
  const deckSize: FormatConfig["deck_size"] =
    storedDeckSize?.type === "Minimum"
    && defaults.deck_size.type === "Minimum"
    && isU16(storedDeckSize.data)
      ? { type: "Minimum", data: storedDeckSize.data }
      : storedDeckSize?.type === "Exactly"
        && defaults.deck_size.type === "Exactly"
        && isU16(storedDeckSize.data)
        ? { type: "Exactly", data: storedDeckSize.data }
        : defaults.deck_size;
  const commanderDamageThreshold =
    defaults.commander_damage_threshold !== null
    && isU8(storedFormatConfig.commander_damage_threshold)
      ? storedFormatConfig.commander_damage_threshold
      : defaults.commander_damage_threshold;
  const formatConfig: FormatConfig = {
    ...defaults,
    deck_size: deckSize,
    starting_life: isI32(storedFormatConfig.starting_life)
      ? storedFormatConfig.starting_life
      : defaults.starting_life,
    commander_damage_threshold: commanderDamageThreshold,
    allow_debug_actions: typeof storedFormatConfig.allow_debug_actions === "boolean"
      ? storedFormatConfig.allow_debug_actions
      : defaults.allow_debug_actions,
  };
  return finalizeRememberedHostConfig(persisted, format, formatConfig, null);
}

/**
 * The format-independent tail both branches share: clamp the player count to
 * what the resolved config can seat, normalize the retired loop-detection
 * variant, and filter AI seats. Factored out so the built-in and Custom
 * branches cannot drift apart on any of it.
 */
function finalizeRememberedHostConfig(
  persisted: Record<string, unknown>,
  format: GameFormat,
  formatConfig: FormatConfig,
  savedCustomFormatId: string | null,
): RememberedHostConfig {
  const playerCount = isU8(persisted.playerCount)
    ? Math.min(Math.max(persisted.playerCount, formatConfig.min_players), formatConfig.max_players)
    : formatConfig.min_players;
  const loopDetectionType = isRecord(persisted.loopDetection)
    ? persisted.loopDetection.type
    : "Off";
  const loopDetection: LoopDetectionMode = loopDetectionType === "Interactive" || loopDetectionType === "On"
    ? { type: "Interactive" }
    : { type: "Off" };
  const aiSeats: AiSeatConfig[] = [];
  if (Array.isArray(persisted.aiSeats)) {
    for (const seat of persisted.aiSeats) {
      if (
        !isRecord(seat)
        || !isU8(seat.seatIndex)
        || seat.seatIndex >= playerCount
        || aiSeats.some((existing) => existing.seatIndex === seat.seatIndex)
        || !(
          AI_DIFFICULTIES.some((difficulty) => difficulty.id === seat.difficulty)
          || seat.difficulty === "CEDH"
        )
        || typeof seat.difficulty !== "string"
        || (typeof seat.deckName !== "string" && seat.deckName !== null)
      ) {
        continue;
      }
      aiSeats.push({
        seatIndex: seat.seatIndex,
        difficulty: seat.difficulty,
        deckName: seat.deckName,
      });
    }
  }

  return {
    format,
    formatConfig,
    savedCustomFormatId,
    playerCount,
    matchType: playerCount === 2 && persisted.matchType === "Bo3" ? "Bo3" : "Bo1",
    loopDetection,
    isPublic: typeof persisted.isPublic === "boolean" ? persisted.isPublic : true,
    startWhenFull: typeof persisted.startWhenFull === "boolean" ? persisted.startWhenFull : true,
    ranked: false,
    aiSeats,
  };
}

export function migrateOfficialServerAddress(
  address: unknown,
  targetAddress: string,
): unknown {
  return typeof address === "string" && isOfficialMultiplayerServerUrl(address)
    ? targetAddress
    : address;
}

// The host-setup selector retired its standalone "On" loop-detection choice
// in favor of "Interactive" (its surviving semantics). A `lastHostConfig`
// persisted before that change may still carry `{ type: "On" }`; forward it
// to Interactive rather than silently dropping to Off, which would turn the
// detector off for a player who had chosen it on.
export function migrateLegacyLoopDetectionOn(lastHostConfig: unknown): unknown {
  if (!lastHostConfig || typeof lastHostConfig !== "object") return lastHostConfig;
  const config = lastHostConfig as Record<string, unknown>;
  const loopDetection = config.loopDetection as { type?: unknown } | undefined;
  if (loopDetection?.type !== "On") return lastHostConfig;
  return { ...config, loopDetection: { type: "Interactive" } };
}

export function migratePersistedMultiplayerState(
  persisted: unknown,
  version: number,
): unknown {
  if (!persisted || typeof persisted !== "object") return persisted;
  const migrated = persisted as Record<string, unknown>;
  if (version < 3) {
    migrated.serverAddress = migrateOfficialServerAddress(
      migrated.serverAddress,
      DEFAULT_MULTIPLAYER_SERVER_URL,
    );
  }
  if (version < 4 && "lastHostConfig" in migrated) {
    migrated.lastHostConfig = migrateLegacyLoopDetectionOn(migrated.lastHostConfig);
  }
  if (version < 5 && "lastHostConfig" in migrated) {
    migrated.lastHostConfig = normalizeRememberedHostConfig(migrated.lastHostConfig);
  }
  return migrated;
}

type MultiplayerSet = (
  partial:
    | Partial<MultiplayerState>
    | ((state: MultiplayerState) => Partial<MultiplayerState>),
) => void;
type MultiplayerGet = () => MultiplayerState & MultiplayerActions;

function resetServerHostSession(set: MultiplayerSet): void {
  clearWsSession();
  set({
    hostGameCode: null,
    hostIsPublic: false,
    hostingStatus: "idle",
    hostSession: null,
    playerSlots: [],
  });
}

function savePregameHostSession(
  get: MultiplayerGet,
  data: { game_code: string; player_token: string; full_key?: { game_code: string; generation: number } },
): void {
  if (!data.full_key || data.full_key.game_code !== data.game_code) return;
  const existing = loadWsSession();
  const hostSession = get().hostSession ?? existing?.hostSession;
  saveWsSession({
    gameCode: data.game_code,
    playerToken: data.player_token,
    fullKey: data.full_key,
    serverUrl: get().serverAddress,
    timestamp: Date.now(),
    ...(hostSession ? { hostSession } : {}),
    ...(hostSession ? { hostIsPublic: get().hostIsPublic } : {}),
  });
}

function clearPregameHostMetadataFromWsSession(): void {
  const session = loadWsSession();
  if (!session) return;
  saveWsSession({
    gameCode: session.gameCode,
    playerToken: session.playerToken,
    fullKey: session.fullKey,
    serverUrl: session.serverUrl,
    timestamp: Date.now(),
  });
}

function handleServerHostMessage(
  set: MultiplayerSet,
  get: MultiplayerGet,
  ws: PhaseSocketTransport,
  msg: { type: string; data?: unknown },
): void {
  if (msg.type === "GameCreated") {
    const data = msg.data as {
      game_code: string;
      player_token: string;
      full_key?: { game_code: string; generation: number };
    };
    savePregameHostSession(get, data);
    // Reset reconnect counter on successful (re)connection.
    hostReconnectAttempt = 0;
    set({ hostGameCode: data.game_code, hostingStatus: "waiting" });
  } else if (msg.type === "GameStarted") {
    gameStartedFired = true;
    clearPregameHostMetadataFromWsSession();
    ws.close();
    hostWs = null;
    const gameId = crypto.randomUUID();
    saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
    useGameStore.setState({ gameId });
    const names = new Map<number, string>();
    for (const slot of get().playerSlots) {
      if (slot.name) names.set(slot.playerId, slot.name);
    }
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
    if (get().hostingStatus !== "waiting") {
      get().cancelHosting();
    }
  }
}

async function openServerHostSocket(
  set: MultiplayerSet,
  get: MultiplayerGet,
  setupFrame: () => unknown,
  onReopen: () => void,
): Promise<void> {
  if (!isValidWebSocketUrl(get().serverAddress)) {
    resetServerHostSession(set);
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
    handleServerHostMessage(set, get, socket.ws, msg);
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
}

function attemptServerHostReconnect(
  set: MultiplayerSet,
  get: MultiplayerGet,
): void {
  if (gameStartedFired) return;
  const session = loadWsSession();
  if (!session || hostReconnectAttempt >= HOST_MAX_RECONNECT_ATTEMPTS) {
    resetServerHostSession(set);
    get().showToast("Connection to server lost.");
    return;
  }

  hostReconnectAttempt++;
  const delay = Math.pow(2, hostReconnectAttempt - 1) * 1000;
  hostReconnectTimer = setTimeout(() => {
    hostReconnectTimer = null;
    if (gameStartedFired) return;
    void openServerHostSocket(
      set,
      get,
      () => ({
        type: "Reconnect",
        data: {
          game_code: session.gameCode,
          player_token: session.playerToken,
          full_key: session.fullKey,
        },
      }),
      () => attemptServerHostReconnect(set, get),
    );
  }, delay);
}

export const useMultiplayerStore = create<MultiplayerState & MultiplayerActions>()(
  persist(
    (set, get) => ({
      playerId: crypto.randomUUID(),
      displayName: "",
      serverAddress: DEFAULT_MULTIPLAYER_SERVER_URL,
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      toasts: new Map(),
      formatConfig: null,
      lastHostConfig: null,
      tournamentCredentials: {},
      playerSlots: [],
      spectators: [],
      isSpectator: false,
      playerNames: new Map(),
      playerAvatars: new Map(),
      compatibilityPlayerCount: null,
      disconnectedPlayers: new Set(),
      actionPending: false,
      latencyMs: null,
      hostGameCode: null,
      hostIsPublic: false,
      hostingStatus: "idle" as HostingStatus,
      hostSession: null,
      pendingGameRoute: null,
      serverInfo: null,
      draftAdapter: null,
      draftView: null,
      draftPhase: null,

      setServerInfo: (info) => set({ serverInfo: info }),
      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (address) => {
        // Switching servers invalidates the live subscription socket: it's
        // still connected to the previous region and would keep streaming
        // that lobby's games and PlayerCount. Tear it down so the next
        // `ensureSubscriptionSocket` dials the new address. No-op when the
        // address is unchanged (re-selecting the current server).
        if (address !== get().serverAddress) {
          get().closeSubscriptionSocket();
        }
        set({ serverAddress: address });
      },
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
      setCompatibilityPlayerCount: (count) =>
        set({ compatibilityPlayerCount: count }),
      rememberHostConfig: (config) => set({
        lastHostConfig: normalizeRememberedHostConfig(config),
      }),
      clearRememberedHostConfig: () => set({ lastHostConfig: null }),
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
        const aiSeats = effectiveAiSeats(settings);
        // Clean up any existing hosting session (server or P2P).
        closeHostWebSocket();
        disposeActiveP2PHost();
        if (activeBroker) {
          if (activeBrokerGameCode) {
            void activeBroker.unregister(activeBrokerGameCode).catch(() => {});
          }
          activeBroker.close();
          activeBroker = null;
          activeBrokerGameCode = null;
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

        void openServerHostSocket(
          set,
          get,
          () => ({
            type: "CreateGameWithSettings",
            data: {
              deck: asDeckPayload(deck),
              display_name: settings.displayName,
              public: settings.public,
              password: settings.password || null,
              timer_seconds: settings.timerSeconds,
              player_count: settings.formatConfig.max_players,
              match_config: {
                match_type: settings.matchType,
                loop_detection: settings.loopDetection,
              },
              format_config: settings.formatConfig,
              ai_seats: aiSeats,
              room_name: settings.roomName,
              start_when_full: settings.startWhenFull,
              ranked: settings.ranked,
            },
          }),
          () => attemptServerHostReconnect(set, get),
        );
      },

      resumeServerHosting: () => {
        if (hostWs || get().hostingStatus !== "idle") {
          return get().hostingStatus !== "idle";
        }

        const session = loadWsSession();
        if (!session?.hostSession || session.serverUrl !== get().serverAddress) {
          return false;
        }

        gameStartedFired = false;
        hostReconnectAttempt = 0;
        set({
          hostIsPublic: session.hostIsPublic ?? false,
          hostingStatus: "connecting",
          hostGameCode: null,
          hostSession: session.hostSession,
          pendingGameRoute: null,
          playerSlots: [],
        });

        void openServerHostSocket(
          set,
          get,
          () => ({
            type: "Reconnect",
            data: {
              game_code: session.gameCode,
              player_token: session.playerToken,
              full_key: session.fullKey,
            },
          }),
          () => attemptServerHostReconnect(set, get),
        );

        return true;
      },

      cancelHosting: () => {
        p2pHostingAttempt += 1;
        closeHostWebSocket();
        disposeActiveP2PHost();
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
        const attempt = ++p2pHostingAttempt;
        const isCurrentAttempt = () => p2pHostingAttempt === attempt;
        const aiSeats = effectiveAiSeats(settings);
        closeHostWebSocket();
        clearWsSession();
        gameStartedFired = false;
        hostReconnectAttempt = 0;

        const resetFailedHosting = () => {
          if (!isCurrentAttempt()) return;
          set({
            hostIsPublic: false,
            hostingStatus: "idle",
            hostGameCode: null,
            hostSession: null,
            playerSlots: [],
          });
        };

        set({
          hostIsPublic: opts.useBroker && settings.public,
          hostingStatus: "connecting",
          hostGameCode: null,
          hostSession: {
            formatConfig: settings.formatConfig,
            timerSeconds: settings.timerSeconds,
            matchType: settings.matchType,
          },
          pendingGameRoute: null,
        });

        let broker: BrokerClient | null = null;
        let brokerGameCode: string | null = null;
        let destroyHostedRoom: (() => void) | null = null;
        let adapter: P2PHostAdapter | null = null;
        const releaseAttempt = () => {
          if (adapter) {
            if (activeP2PHostAdapter === adapter) {
              disposeActiveP2PHost();
            } else {
              adapter.dispose();
            }
          } else {
            destroyHostedRoom?.();
          }
          if (broker) {
            if (brokerGameCode) {
              void broker.unregister(brokerGameCode).catch(() => {});
            }
            broker.close();
            if (activeBroker === broker) {
              activeBroker = null;
              activeBrokerGameCode = null;
            }
          }
        };

        try {
          const [{ hostRoom }, { P2PHostAdapter }] = await Promise.all([
            import("../network/connection"),
            import("../adapter/p2p-adapter"),
          ]);
          if (!isCurrentAttempt()) return false;

          if (activeP2PHostAdapter) {
            activeP2PHostAdapter.dispose();
            activeP2PHostAdapter = null;
            activeP2PHostGameId = null;
          }

          let nativeP2P: { expectedServerVersion?: string } | undefined;
          const nativeEngineKey = nativeEngineKeyForCurrentOrigin();
          if (
            nativeEngineKey
            && canAttemptNativeEngine(usePreferencesStore.getState().nativeEngineEnabled)
          ) {
            try {
              await ensureNativeEngine(nativeEngineKey);
              if (!isCurrentAttempt()) return false;
              nativeP2P = {
                expectedServerVersion:
                  "release" in nativeEngineKey ? nativeEngineKey.release.version : undefined,
              };
            } catch (err) {
              console.warn("[P2P] native engine unavailable; using WASM host", err);
            }
          }
          if (!isCurrentAttempt()) return false;

          const host = await hostRoom(undefined, {});
          destroyHostedRoom = () => host.destroy();
          if (!isCurrentAttempt()) {
            releaseAttempt();
            return false;
          }
          if (opts.useBroker) {
            broker = await openBrokerClient(get().serverAddress);
            if (!isCurrentAttempt()) {
              releaseAttempt();
              return false;
            }
            const registered = await broker.registerHost({
              hostPeerId: host.peer.id,
              deck: asDeckPayload(deck),
              displayName: get().displayName || "Host",
              public: settings.public,
              password: settings.password || null,
              timerSeconds: null,
              playerCount: settings.formatConfig.max_players,
              matchConfig: {
                match_type: settings.matchType,
                loop_detection: settings.loopDetection,
              },
              formatConfig: settings.formatConfig,
              aiSeats,
              roomName: opts.roomName ?? null,
              draftMetadata: null,
              startWhenFull: settings.startWhenFull,
              ranked: settings.ranked,
            });
            brokerGameCode = registered.gameCode;
            if (!isCurrentAttempt()) {
              releaseAttempt();
              return false;
            }
            activeBroker = broker;
            activeBrokerGameCode = registered.gameCode;
          }

          const gameId = crypto.randomUUID();
          const p2pAdapter = new P2PHostAdapter(
            {
              player: asDeckPayload(deck),
              opponent: { main_deck: [], sideboard: [], commander: [], planar_deck: [], scheme_deck: [] },
              ai_decks: [],
            },
            host.peer,
            host.onGuestConnected,
            settings.formatConfig.max_players,
            settings.formatConfig,
            { match_type: settings.matchType, loop_detection: settings.loopDetection },
            undefined,
            broker ?? undefined,
            false,
            brokerGameCode ?? undefined,
            {
              gameId,
              roomCode: host.roomCode,
              hostDisplayName: get().displayName || undefined,
            },
            nativeP2P,
          );
          adapter = p2pAdapter;

          p2pAdapter.onEvent((event) => {
            if (!isCurrentAttempt()) return;
            if (event.type === "playerSlotsUpdated" || event.type === "lobbyProgress") {
              set({ playerSlots: p2pAdapter.getPlayerSlots() });
            } else if (event.type === "playerIdentity") {
              const names = new Map<number, string>();
              for (const [playerId, name] of Object.entries(event.playerNames ?? {})) {
                names.set(Number(playerId), name);
              }
              set({ playerNames: names });
            } else if (event.type === "roomFull") {
              if (settings.startWhenFull) {
                void startActiveP2PHostGame(set).catch((err) => {
                  get().showToast(err instanceof Error ? err.message : String(err));
                });
              } else {
                get().showToast("Room full — ready to start!");
              }
            } else if (event.type === "error") {
              get().showToast(event.message);
            }
          });

          activeP2PHostAdapter = p2pAdapter;
          activeP2PHostGameId = gameId;

          await p2pAdapter.initialize();
          if (!isCurrentAttempt()) {
            releaseAttempt();
            return false;
          }
          destroyHostedRoom = null;

          set({
            hostIsPublic: opts.useBroker && settings.public,
            hostingStatus: "waiting",
            hostGameCode: host.roomCode,
            hostSession: {
              formatConfig: settings.formatConfig,
              timerSeconds: settings.timerSeconds,
              matchType: settings.matchType,
            },
            playerSlots: p2pAdapter.getPlayerSlots(),
            // P2P/broker hosting has no advertised game-server URL. Clear any
            // serverInfo left by a prior online-host session so the P2P share
            // string is the bare room code, never a stale `code@<old-server>`.
            serverInfo: null,
          });

          for (const seat of aiSeats) {
            await p2pAdapter.applySeatMutation({
              type: "SetKind",
              data: {
                seatIndex: seat.seatIndex,
                kind: {
                  type: "Ai",
                  data: {
                    difficulty: seat.difficulty,
                  deck: seat.deck ?? aiSeatDeckChoice(seat.deckName),
                  },
                },
              },
            });
            if (!isCurrentAttempt()) {
              releaseAttempt();
              return false;
            }
          }

          return true;
        } catch (err) {
          releaseAttempt();
          if (!isCurrentAttempt()) return false;
          console.error("[startP2PHostingSession] failed:", err);
          if (
            err instanceof AdapterError
            && err.code === AdapterErrorCode.NOT_INITIALIZED
          ) {
            get().showToast(err.message);
          }
          resetFailedHosting();
          return false;
        }
      },

      takeActiveP2PHost: (gameId) => {
        if (!activeP2PHostAdapter || activeP2PHostGameId !== gameId) return null;

        const adapter = activeP2PHostAdapter;
        activeP2PHostAdapter = null;
        activeP2PHostGameId = null;
        return adapter;
      },

      seatMutateAsync: async (mutation) => {
        const serverSocket = activeServerHostingSocket(get);
        if (serverSocket) {
          serverSocket.send(JSON.stringify({
            type: "SeatMutate",
            data: { mutation },
          }));
          return;
        }
        await runP2PSeatMutation(mutation, set);
      },

      seatMutate: (mutation) => {
        void get()
          .seatMutateAsync(mutation)
          .catch((err) => {
            console.error("[seatMutate]", mutation.type, err);
            get().showToast(err instanceof Error ? err.message : String(err));
          });
      },

      startLobbyWithCurrentPlayers: async () => {
        const waiting = get()
          .playerSlots.filter((slot) => slot.kind.type === "WaitingHuman")
          .sort((a, b) => b.playerId - a.playerId);
        for (const slot of waiting) {
          await get().seatMutateAsync({
            type: "Remove",
            data: { seatIndex: slot.playerId },
          });
        }
        await get().seatMutateAsync({ type: "Start" });
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
              // The shared subscription socket carries lobby frames only —
              // `SubscribeLobby`, the join-target RPCs, `PlayerCount`. Declaring
              // the surface keeps it usable against a server whose full-game
              // protocol has drifted from this build's, which is the whole point
              // of versioning the lobby separately. Server-run hosting and
              // joining open their own sockets and keep the exact-match window.
              openPhaseSocket(addr, { surface: "lobby" }).catch((err) => {
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
                    // Re-attach the shared subscription if anyone still wants
                    // it — a tournament subscriber alone is reason enough, and
                    // gating this on `lobbySubscribers` would leave a
                    // tournament-only page silently dead after a reconnect.
                    // The first snapshot from the server overwrites the caches;
                    // stale data is not authoritative across a reconnect.
                    if (lobbySubscriptionRefCount() > 0) {
                      acquireLobbySubscription(socket, set, get);
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
                  // Drop the handles to the old socket's listeners; they are
                  // re-bound on the next "open". Not invoked: the old socket is
                  // gone, and `subscribeLobbyOver`'s detach is `readyState`-
                  // guarded, so calling it could only remove listeners from a
                  // socket that is being discarded anyway.
                  lobbyAttachDetach = null;
                  tournamentAttachDetach = null;
                  // Both caches are per-socket-generation; a reconnect must not
                  // seed a new subscriber from a pre-drop snapshot.
                  lobbySnapshot = null;
                  tournamentListSnapshot = null;
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
        // Unconditional teardown of the shared subscription, both kinds.
        // `detachSharedSubscription` nulls both handles and both snapshots.
        detachSharedSubscription();
        lobbySubscribers.clear();
        tournamentSubscribers.clear();
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
            // The broker rejects a blank display_name on the resolve frame
            // (required-label rule) and the worker shell drops it without a
            // reply — the guest then times out at deck-select. Always carry
            // the player's name so the frame validates.
            displayName: get().displayName || "Player",
          });
        } finally {
          pendingJoinRpcAborts.delete(ac);
        }
      },

      lookupJoinTarget: async (code, password, opts) => {
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
            reserve: opts?.reserve,
            displayName: opts?.displayName,
            releaseReservationToken: opts?.releaseReservationToken,
          });
        } finally {
          pendingJoinRpcAborts.delete(ac);
        }
      },

      joinServerDraft: async (serverUrl, draftCode, displayName, password) => {
        // Dispose any previous draft adapter before creating a new one.
        get().draftAdapter?.dispose();
        const adapter = new ServerDraftAdapter(serverUrl);
        const view = await adapter.joinDraft(draftCode, displayName, password);
        set({ draftAdapter: adapter, draftView: view, draftPhase: adapter.currentPhase });
      },

      createServerDraft: async (serverUrl, settings) => {
        // Dispose any previous draft adapter before creating a new one.
        get().draftAdapter?.dispose();
        const adapter = new ServerDraftAdapter(serverUrl);
        await adapter.createDraft(settings);
        set({ draftAdapter: adapter, draftView: null, draftPhase: "lobby" });
      },

      subscribeLobby: async (onUpdate) => {
        const socket = await get().ensureSubscriptionSocket();
        if (!socket) return null;
        lobbySubscribers.add(onUpdate);
        // Acquire against the count that spans BOTH subscriber kinds: a
        // tournament subscriber may already hold the subscription, in which
        // case this must not re-send `SubscribeLobby`.
        acquireLobbySubscription(socket, set, get);
        if (lobbySnapshot) {
          // Immediate seed so a late subscriber renders without waiting for
          // the next server push. Unconditional (not an `else`): under the
          // unified count the very first LOBBY subscriber can still arrive
          // to an existing snapshot, because a tournament subscriber may
          // have acquired the subscription first and the `LobbyUpdate` push
          // may already have landed.
          onUpdate(lobbySnapshot);
        }
        return () => {
          lobbySubscribers.delete(onUpdate);
          // Only the last subscriber of EITHER kind may release — see
          // `lobbySubscriptionRefCount`.
          releaseLobbySubscription();
        };
      },

      subscribeTournaments: async (handlers) => {
        const socket = await get().ensureSubscriptionSocket();
        if (!socket) return null;
        tournamentSubscribers.add(handlers);
        // First subscriber of EITHER kind puts `SubscribeLobby` on the wire.
        // That frame is not optional for tournaments: `AddSubscriber` is the
        // only path into the broker's delivery set, and its
        // `ToSelf(TournamentListUpdate)` is the only way this client ever
        // learns the list without waiting on someone else's mutation.
        acquireLobbySubscription(socket, set, get);
        if (tournamentListSnapshot) {
          handlers.onListUpdate?.(tournamentListSnapshot);
        }
        return () => {
          tournamentSubscribers.delete(handlers);
          releaseLobbySubscription();
        };
      },

      createTournament: async (req) =>
        runTournamentRpc(get, async (socket, signal) => {
          const result = await createTournamentOver(socket, req, { signal });
          if (result.ok) {
            // Keyed by the code in the REPLY: `CreateTournament` carries no
            // client-chosen code (the broker mints it), so the reply is the
            // only authority for which tournament this token opens.
            set((state) => ({
              tournamentCredentials: rememberTournamentCredential(
                state.tournamentCredentials,
                result.value.code,
                { organizerToken: result.value.organizer_token },
              ),
            }));
          }
          return result;
        }),

      joinTournament: async (code, displayName) =>
        runTournamentRpc(get, async (socket, signal) => {
          // Captured BEFORE the await so the credential records the key that
          // was actually sent, not whatever `playerId` reads as afterwards.
          const playerKey = get().playerId;
          const result = await joinTournamentOver(
            socket,
            code,
            playerKey,
            displayName || get().displayName || "Player",
            { signal },
          );
          if (result.ok) {
            set((state) => ({
              tournamentCredentials: rememberTournamentCredential(
                state.tournamentCredentials,
                result.value.code,
                { playerToken: result.value.player_token, playerKey },
              ),
            }));
          }
          return result;
        }),

      getTournament: async (code) =>
        runTournamentRpc(get, (socket, signal) =>
          getTournamentOver(socket, code, { signal }),
        ),

      startTournamentRound: async (code) =>
        runGatedTournamentRpc(get, code, "organizer", (socket, token, signal) =>
          startTournamentRoundOver(socket, code, token, { signal }),
        ),

      endTournament: async (code) =>
        runGatedTournamentRpc(get, code, "organizer", (socket, token, signal) =>
          endTournamentOver(socket, code, token, { signal }),
        ),

      reportMatchResult: async (code, pairingId, outcome) =>
        runGatedTournamentRpc(get, code, "player", (socket, token, signal) =>
          reportMatchResultOver(socket, code, pairingId, token, outcome, {
            signal,
          }),
        ),

      dropFromTournament: async (code) =>
        runGatedTournamentRpc(get, code, "player", (socket, token, signal) =>
          dropFromTournamentOver(socket, code, token, { signal }),
        ),
    }),
    {
      name: "phase-multiplayer",
      version: 5,
      // v0/v1 → v2: official hosted lobby addresses are deployment defaults,
      // not user intent. A self-hosted build must move returning browsers from
      // the official lobby to its configured default while preserving explicit
      // custom/self-hosted addresses.
      //
      // v2 → v3: same rule, re-applied because the official set now spans a
      // broker PER RELEASE CHANNEL. Without this bump a returning preview
      // browser keeps its persisted production address, and detectServerUrl
      // honours any valid stored address, so it would silently stay pinned to a
      // lobby its build cannot handshake with. Re-running the same migration
      // repoints it at this channel's broker; a user-typed non-official address
      // is still preserved.
      //
      // v3 → v4: the host-setup selector dropped its standalone "On"
      // loop-detection choice. A `lastHostConfig.loopDetection` of `{ type:
      // "On" }` persisted under an older build is forwarded to `Interactive`
      // (its surviving semantics) rather than left to silently fall back to
      // `Off` on next read.
      //
      // v4 → v5: persisted host configurations used to retain a serialized
      // `FormatConfig`. Project it onto the current engine registry while
      // retaining only user-editable fields, so engine protocol shape changes
      // (such as `deck_size: 100` becoming `{ type: "Exactly", data: 100 }`)
      // cannot leave hosting stuck before GameCreated.
      migrate: migratePersistedMultiplayerState,
      // Persisted state is external input. Migration only runs when the schema
      // version changes, so hydrate current-version blobs through the same
      // normalizer before the store exposes them to host setup.
      merge: (persisted, current) => {
        const saved = persisted && typeof persisted === "object"
          ? persisted as Partial<MultiplayerState>
          : {};
        return {
          ...current,
          ...saved,
          lastHostConfig: normalizeRememberedHostConfig(saved.lastHostConfig),
          tournamentCredentials: normalizeTournamentCredentials(
            saved.tournamentCredentials,
          ),
        };
      },
      partialize: (state) => ({
        playerId: state.playerId,
        displayName: state.displayName,
        serverAddress: state.serverAddress,
        lastHostConfig: state.lastHostConfig,
        tournamentCredentials: state.tournamentCredentials,
      }),
    },
  ),
);

export function getPlayerDisplayName(playerId: number, myId?: number): string {
  if (playerId === myId) return "You";
  return getOpponentDisplayName(playerId);
}

export function getOpponentDisplayName(playerId: number): string {
  const state = useMultiplayerStore.getState();
  const name = state.playerNames.get(playerId);
  if (name) return name;
  return `Opp ${playerId + 1}`;
}
