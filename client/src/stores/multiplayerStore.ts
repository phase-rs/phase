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
  PlayerId,
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
  HandshakeError,
  openPhaseSocket,
  withReconnect,
  type PhaseSocket,
  type PhaseSocketTransport,
  type ReconnectHandle,
  type ReconnectState,
} from "../services/openPhaseSocket";
import {
  SERVER_PRESETS,
  isValidWebSocketUrl,
  parseWebSocketUrl,
  type ServerPreset,
} from "../services/serverDetection";
// TYPE-ONLY, and worth keeping that way: `verbatimModuleSyntax` erases it, so
// it costs this module nothing. It is NOT what keeps `serverDirectory` out of
// the store's runtime graph, though — the `serverMetrics` import below reaches
// it anyway (see there).
import type { DirectorySource } from "../services/serverDirectory";
// A VALUE import, and with it a real runtime edge: `serverMetrics` value-imports
// `directoryUrl` from `serverDirectory`, which closes back on this store TWICE
// — directly (`serverDirectory.ts:23` imports `useMultiplayerStore`) and via
// `serverDetection`. The store must take this edge: it is the single authority
// for opening a lobby socket, so it is the only place a connect outcome can be
// observed.
//
// THE INVARIANT THAT ACTUALLY MATTERS, and that this cycle must keep: no module
// in it may reach INTO ANOTHER MODULE OF THE CYCLE during MODULE EVALUATION.
// Every cross-cycle access in `serverMetrics`, `serverDirectory` and
// `serverDetection` sits inside a function body, so the cycle is resolved by
// the time anything calls them. Intra-module top-level work is fine and does
// happen — `serverDetection.ts:37`'s `DEFAULT_SERVER = SERVER_PRESETS[0].url`
// reads a constant declared in its own file, which no cycle can starve.
// Module evaluation is the only window `create()`, `migrate` and `merge` care
// about, and this is what a change to any of those three files has to preserve:
// a top-level `useMultiplayerStore.getState()` — or a `SERVER_PRESETS` read
// from a file that does not declare it — is a temporal-dead-zone crash at boot,
// not a lint nit.
import { reportConnectOutcome } from "../services/serverMetrics";
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

/** Where a lobby source came from. `directory` entries are projected from the
 * official directory by `services/serverDirectory.ts`; they are derived each
 * session and never persisted. */
export type LobbySourceOrigin = "official" | "directory" | "user";

/**
 * One lobby authority the client browses. The client is a multi-authority
 * cache: every enabled source gets its own subscription socket and its rows
 * are merged into one list, tagged with the source that listed them.
 */
export interface LobbySource {
  /** Canonical `URL.href` of a `ws(s)://` endpoint. */
  readonly url: string;
  /** Display label — the URL host for built-in and hand-added sources. */
  readonly name: string;
  readonly origin: LobbySourceOrigin;
  /** Learned from the handshake's `ServerHello`; undefined until this
   * source's socket has opened at least once. */
  readonly kind?: ServerInfo["mode"];
  /** 0–100 health score, produced by `services/serverDirectory.ts` from a
   * listing's `score.value`; the list comparator treats `undefined` as the
   * lowest rank. Never the whole `WireScore` — the components stay on
   * `DirectorySource.row.score`. */
  readonly score?: number;
}

/** A lobby row together with the source that listed it. `LobbyGame` mirrors
 * an engine-authored wire type and must not grow an origin field, so the
 * origin rides beside it in this client-only wrapper. */
export interface LobbyGameEntry {
  game: LobbyGame;
  source: LobbySource;
}

/** Live connection state of one source's subscription channel. Reuses
 * `ReconnectState` rather than inventing a parallel status enum;
 * `"offline"` is the degraded state the UI reports. */
export interface LobbySourceStatus {
  state: ReconnectState;
  serverInfo: ServerInfo | null;
  /**
   * Last `PlayerCount` this source reported *on its current socket*, or
   * `null` when it has reported none. Required-and-nullable rather than
   * optional (mirrors `serverInfo`) so every construction site has to state
   * the count: the row is rebuilt on each state change, and a count that
   * outlived the socket that sent it would otherwise be advertised as live
   * for the rest of the session after a single reconnect.
   */
  playerCount: number | null;
}

/** Result of {@link MultiplayerActions.addUserLobbySource}. Mirrors the
 * `{ ok, reason }` result idiom used by the broker RPCs. */
export type AddLobbySourceResult =
  | { ok: true; source: LobbySource }
  | { ok: false; reason: "invalid_url" | "duplicate" | "cap_reached" };

/**
 * Bound on hand-added (`user`) lobby sources. Built-in presets are not
 * counted — they are not user-removable — and neither are directory listings,
 * which carry their own bound, so the dialed total is at most
 * `SERVER_PRESETS.length + MAX_USER_LOBBY_SOURCES +
 * MAX_DIRECTORY_LOBBY_SOURCES` (`services/serverDirectory.ts`). Hydration trims
 * to the same constant, so a persisted blob can never dial more than the add
 * path would allow.
 */
export const MAX_USER_LOBBY_SOURCES = 8;

/** Built-in source for a picker preset. */
export function presetLobbySource(preset: ServerPreset): LobbySource {
  return {
    url: preset.url,
    name: parseWebSocketUrl(preset.url)?.host ?? preset.url,
    origin: "official",
  };
}

/** A hand-added source, canonicalised through the URL parser. `null` when
 * the value is not a `ws(s)://` URL. */
export function userLobbySource(url: string): LobbySource | null {
  const parsed = parseWebSocketUrl(url.trim());
  if (!parsed) return null;
  return { url: parsed.href, name: parsed.host, origin: "user" };
}

/**
 * The origin of a `CODE@host` join: a one-off authority that is browsed by
 * nobody and persisted nowhere. Same shape as a hand-added source — the
 * distinct name is what makes the intent readable at the call sites.
 */
export function adHocLobbySource(url: string): LobbySource | null {
  return userLobbySource(url);
}

/**
 * The enabled lobby sources, derived at call time rather than persisted.
 *
 * Built-in presets are rebuilt every session (a build's default can move
 * between releases) and only `user` entries are stored, so `partialize` has
 * nothing to filter and `merge` has nothing to re-insert. Deriving here is
 * also what keeps `SERVER_PRESETS` out of the store's own module evaluation:
 * `serverDetection.ts` imports this module, so reading that `export const`
 * while this module evaluates (the `create()` initializer, or persist
 * hydration, which zustand runs synchronously inside `create()`) would hit
 * the import cycle's temporal dead zone.
 */
export function lobbySources(
  state: Pick<
    MultiplayerState,
    "userLobbySources" | "sourceStatus" | "directorySources" | "disabledDirectorySources"
  >,
): LobbySource[] {
  const presets = SERVER_PRESETS.map(presetLobbySource);
  const presetUrls = new Set(presets.map((preset) => preset.url));
  // A hand-added URL that is also a preset is dropped here rather than at
  // hydration: `merge` runs while this module evaluates and must not read
  // `SERVER_PRESETS` (import cycle, temporal dead zone).
  //
  // Precedence is presets → user → directory. Order matters beyond looks:
  // `findLobbyGameByCode` scans in derived order, so a code listed by both a
  // preset and a directory server still resolves to the preset.
  return [
    ...presets,
    ...state.userLobbySources.filter((source) => !presetUrls.has(source.url)),
    ...unshadowedDirectorySources(state)
      .filter((entry) => !state.disabledDirectorySources.includes(entry.source.url))
      .map((entry) => entry.source),
  ].map((source) => {
    const mode = state.sourceStatus.get(source.url)?.serverInfo?.mode;
    return mode === undefined ? source : { ...source, kind: mode };
  });
}

/**
 * Directory entries that are not already a preset or a hand-added source.
 *
 * Precedence is presets → user → directory, extending the existing
 * preset-beats-user rule: a hand-added entry is an explicit, persisted claim
 * and a listing is transient, so the user's own row wins and keeps its
 * `Remove` / `Use for hosting` affordances.
 *
 * Reads `SERVER_PRESETS` at CALL TIME, exactly as {@link lobbySources} does and
 * for the same reason — a build's preset set can move between releases, and
 * reading the constant during module evaluation would hit the
 * `serverDetection` ⇄ `multiplayerStore` cycle's temporal dead zone. All three
 * callers (`lobbySources`, `directoryLobbySources`, `ensureSubscriptionSocket`)
 * run after hydration; none is reachable from `create()`, `migrate` or `merge`.
 *
 * Both sides of every comparison are CANONICAL. A preset URL is a build-time
 * define, spelled by hand; a directory URL has been through
 * `parseWebSocketUrl(...).href`. Under every shipped define the two spellings
 * coincide, so comparing raw would pass today — and would silently stop
 * shadowing the official preset the day a pathless or otherwise non-canonical
 * URL is configured, which is exactly the duplicate-preset failure this helper
 * exists to prevent. `userUrls` needs no such call: every `userLobbySources`
 * entry was minted by `userLobbySource`, and hydration rebuilds each one
 * through the same function, so those URLs are canonical by construction.
 */
function unshadowedDirectorySources(
  state: Pick<MultiplayerState, "userLobbySources" | "directorySources">,
): DirectorySource[] {
  const presetUrls = new Set(
    SERVER_PRESETS.map((preset) => parseWebSocketUrl(preset.url)?.href ?? preset.url),
  );
  const userUrls = new Set(state.userLobbySources.map((source) => source.url));
  return state.directorySources.filter(
    (entry) => !presetUrls.has(entry.source.url) && !userUrls.has(entry.source.url),
  );
}

/**
 * The ANNOUNCED key for a URL this client dialed, or `null` when the URL is not
 * a directory listing this client can name to the directory.
 *
 * Two spellings of "that server" exist and they are not always the same string:
 * `entry.source.url` is the CLIENT key (`parseWebSocketUrl(...).href`) and
 * `entry.row.url` is the ANNOUNCED key — the `servers` PRIMARY KEY, and the
 * only spelling the Worker's fold will accept. The client key is not invertible
 * back to it, so a caller that has to name a server TO the directory must come
 * through here.
 *
 * `null` is returned for a preset, a hand-added source, a one-off join origin,
 * and for a listing SHADOWED by either — which is also what keeps a user's
 * private or LAN address off the wire, since only announced servers can match.
 */
function announcedUrlFor(
  state: Pick<MultiplayerState, "userLobbySources" | "directorySources">,
  url: string,
): string | null {
  return unshadowedDirectorySources(state).find((entry) => entry.source.url === url)?.row.url
    ?? null;
}

/** Every unshadowed directory entry with its enabled flag — including the
 * DISABLED ones, which {@link lobbySources} omits by construction. The picker
 * is the only place a disabled entry can be switched back on, so it needs the
 * set `lobbySources` filters away — but NOT the set it shadows: an entry a
 * preset or a hand-added source already claims renders as that row, not as a
 * second directory row. Built on `unshadowedDirectorySources`, the one
 * shadowing predicate this file has, so the three consumers (this,
 * `lobbySources`, and `ensureSubscriptionSocket`'s dial gate) cannot disagree
 * about what is shadowed. */
export function directoryLobbySources(
  state: Pick<
    MultiplayerState,
    "userLobbySources" | "sourceStatus" | "directorySources" | "disabledDirectorySources"
  >,
): { entry: DirectorySource; enabled: boolean }[] {
  return unshadowedDirectorySources(state).map((entry) => {
    // Same kind-from-status decoration `lobbySources` applies, so a row's kind
    // reads identically in both lists.
    const mode = state.sourceStatus.get(entry.source.url)?.serverInfo?.mode;
    return {
      entry: mode === undefined ? entry : { ...entry, source: { ...entry.source, kind: mode } },
      enabled: !state.disabledDirectorySources.includes(entry.source.url),
    };
  });
}

/** The source games are hosted/registered on, as a `LobbySource`. `null` in
 * direct-codes mode. A hosting server that is not (or no longer) a browsed
 * source still resolves, as an ad-hoc origin. */
export function hostingLobbySource(
  state: Pick<
    MultiplayerState,
    | "hostingServer"
    | "userLobbySources"
    | "sourceStatus"
    | "directorySources"
    | "disabledDirectorySources"
  >,
): LobbySource | null {
  const { hostingServer } = state;
  if (hostingServer === null) return null;
  return (
    // Hosting placement over a directory-listed server is now a supported
    // choice, disclosed in the host-setup picker, so a `hostingServer` that
    // matches a listing resolves AS that listing — carrying its name, kind and
    // score — instead of falling through to `adHocLobbySource`. The dial target
    // is unchanged either way: both spellings are the same URL, and a
    // `LobbySource` is consumed as a dial target by URL. What the listing adds
    // is its label and its stored compatibility verdict, so an incompatible
    // listed server is refused before the socket rather than at the handshake.
    lobbySources(state).find((source) => source.url === hostingServer)
    ?? adHocLobbySource(hostingServer)
  );
}

/**
 * Display order for the merged multi-authority list: official sources
 * first, then by source score (undefined ranks lowest), then oldest table
 * first so the longest-waiting host is at the top.
 *
 * This is presentation of a client-side cache — the engine has no ordering
 * opinion about rows that came from different authorities.
 */
export function compareLobbyGameEntries(a: LobbyGameEntry, b: LobbyGameEntry): number {
  const officialRank = (entry: LobbyGameEntry) => (entry.source.origin === "official" ? 0 : 1);
  const byOfficial = officialRank(a) - officialRank(b);
  if (byOfficial !== 0) return byOfficial;
  const byScore = (b.source.score ?? -1) - (a.source.score ?? -1);
  if (byScore !== 0) return byScore;
  return a.game.created_at - b.game.created_at;
}

/**
 * One lobby source's long-lived, reconnecting subscription channel. Opened
 * on first multiplayer-home entry via `ensureSubscriptionSocket`, not at app
 * boot: users who never touch multiplayer don't pay for a WS. Shared between
 * the lobby subscribe path (SubscribeLobby / LobbyUpdate traffic) and the
 * join-adjacent RPCs aimed at that same authority. The `withReconnect`
 * wrapper re-handshakes on unexpected drops; `onStateChange` drives
 * pending-RPC rejection, per-source status and re-subscribe.
 */
interface SourceChannel {
  reconnect: ReconnectHandle | null;
  /** Awaiters of the first open — resolves once the handshake lands, or with
   * `null` if the factory exhausts all retries without ever connecting. */
  firstOpen: Promise<PhaseSocket | null> | null;
  /**
   * AbortControllers for in-flight join-adjacent RPCs (`resolveGuest`,
   * `lookupJoinTarget`) on this channel. On the socket's `reconnecting`
   * transition we abort every pending call so the caller gets a
   * `connection_lost` result immediately rather than waiting for its own
   * timeout. New calls after reconnect use fresh controllers.
   */
  pendingRpcAborts: Set<AbortController>;
  /** Per-socket detach returned by `subscribeLobbyOver`. Re-bound on
   * reconnect; `null` when no listener is attached. */
  attachDetach: (() => void) | null;
  /** Per-socket detach for this channel's ambient-frame listener. Bound and
   * dropped in lockstep with `attachDetach` — both listen on the same
   * socket and must follow it across a reconnect. */
  ambientDetach: (() => void) | null;
  /** Most recent `LobbyUpdate` snapshot from this source, used to seed new
   * subscribers and to resolve a typed code to its listing authority. */
  snapshot: LobbyGame[] | null;
}

const subscriptionChannels = new Map<string, SourceChannel>();

/**
 * Registered lobby subscribers. The store multiplexes one
 * `subscribeLobbyOver` attachment per channel across all of them: the first
 * subscriber attaches on every source, subsequent subscribers are seeded
 * from each channel's cached snapshot, and only the *last* subscriber
 * leaving sends `UnsubscribeLobby`. This prevents the ref-counting bug
 * where one caller's unsubscribe would silence every other caller.
 */
const lobbySubscribers: Set<(games: LobbyGame[], source: LobbySource) => void> = new Set();

/**
 * A frame a subscription socket carries outside the `LobbyUpdate` family.
 * Typed as a union rather than raw wire messages so consumers never parse
 * JSON and never see a frame they don't handle. Growing this union is a
 * compile error at every consumer that closes its `kind` switch with
 * `assertNever` — today that is `LobbyView`'s `subscribeAmbientLobby`
 * handler, the sole consumer, whose `default` arm is what turns a new
 * variant into a `type-check` failure instead of a frame the view drops.
 */
export type AmbientLobbyFrame =
  | { kind: "playerCount"; count: number }
  | { kind: "passwordRequired"; gameCode: string };

/** Registered ambient-frame subscribers, multiplexed over one listener per
 * channel exactly like {@link lobbySubscribers}. */
const ambientSubscribers: Set<
  (frame: AmbientLobbyFrame, source: LobbySource) => void
> = new Set();

function channelFor(url: string): SourceChannel {
  const existing = subscriptionChannels.get(url);
  if (existing) return existing;
  const channel: SourceChannel = {
    reconnect: null,
    firstOpen: null,
    pendingRpcAborts: new Set(),
    attachDetach: null,
    ambientDetach: null,
    snapshot: null,
  };
  subscriptionChannels.set(url, channel);
  return channel;
}

/** Tear one source's channel down: abort its RPCs, stop listening, close
 * the socket and drop its status row. */
function closeChannel(set: MultiplayerSet, get: MultiplayerGet, url: string): void {
  const channel = subscriptionChannels.get(url);
  if (!channel) return;
  for (const ac of channel.pendingRpcAborts) ac.abort();
  channel.pendingRpcAborts.clear();
  channel.attachDetach?.();
  channel.attachDetach = null;
  channel.ambientDetach?.();
  channel.ambientDetach = null;
  channel.snapshot = null;
  channel.firstOpen = null;
  channel.reconnect?.close();
  channel.reconnect = null;
  subscriptionChannels.delete(url);
  const status = new Map(get().sourceStatus);
  if (status.delete(url)) set({ sourceStatus: status });
}

function setSourceStatus(
  set: MultiplayerSet,
  get: MultiplayerGet,
  url: string,
  status: LobbySourceStatus,
): void {
  const next = new Map(get().sourceStatus);
  next.set(url, status);
  set({ sourceStatus: next });
}

/** Attach this channel's `LobbyUpdate` listener and fan its snapshots out to
 * every subscriber, tagged with the source that listed them. */
function attachLobbyListener(
  get: MultiplayerGet,
  channel: SourceChannel,
  url: string,
  socket: PhaseSocket,
): void {
  channel.attachDetach = subscribeLobbyOver(socket, (games) => {
    channel.snapshot = games;
    const source = lobbySources(get()).find((s) => s.url === url);
    if (!source) return;
    for (const cb of lobbySubscribers) cb(games, source);
  });
}

/** Map a raw frame to the ambient union, or `null` for anything this
 * listener does not own — `LobbyUpdate`-family frames belong to the lobby
 * listener and RPC replies to their callers. */
function parseAmbientFrame(msg: {
  type: string;
  data?: unknown;
}): AmbientLobbyFrame | null {
  switch (msg.type) {
    case "PlayerCount":
      return { kind: "playerCount", count: (msg.data as { count: number }).count };
    case "PasswordRequired":
      return {
        kind: "passwordRequired",
        gameCode: (msg.data as { game_code: string }).game_code,
      };
    default:
      return null;
  }
}

/**
 * Attach this channel's ambient-frame listener and fan its frames out to
 * every subscriber, tagged with the source they arrived on. Bound on every
 * `"open"` alongside the lobby listener, so a reconnect's brand-new socket
 * keeps both flowing — a consumer that held a socket reference itself would
 * be listening to the pre-drop socket forever.
 *
 * `PlayerCount` is recorded on the source's own status row rather than
 * fanned out as the number to store: the count is per-source state the
 * store already owns a home for, and tying it to the status row is what
 * makes "this count is live" structural — every state change rewrites the
 * row, so a count can never outlive the socket that sent it.
 */
function attachAmbientListener(
  set: MultiplayerSet,
  get: MultiplayerGet,
  channel: SourceChannel,
  url: string,
  socket: PhaseSocket,
): void {
  const listener = (event: MessageEvent) => {
    let msg: { type: string; data?: unknown };
    try {
      msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
    } catch {
      return;
    }
    const frame = parseAmbientFrame(msg);
    if (!frame) return;
    if (frame.kind === "playerCount") {
      const status = get().sourceStatus.get(url);
      // No status row means this channel is not tracked (closed, or never
      // opened as a browsed source); a count with no live row to sit on is
      // dropped rather than resurrecting one.
      if (status) {
        setSourceStatus(set, get, url, { ...status, playerCount: frame.count });
      }
    }
    const source = lobbySources(get()).find((s) => s.url === url);
    if (!source) return;
    for (const cb of ambientSubscribers) cb(frame, source);
  };
  socket.ws.addEventListener("message", listener);
  channel.ambientDetach = () => {
    socket.ws.removeEventListener("message", listener);
  };
}

/** Lobby row for a game/draft code, with the source that listed it, from the
 * cached channel snapshots. Sources are scanned in derived order, so a code
 * listed by two authorities resolves to the first one browsed.
 *
 * `sourceUrl` scopes the search to one authority's snapshot. `game_code` is
 * unique per authority, not across the merged list, so any caller that
 * already knows which server it is talking to (a frame that arrived on a
 * specific socket) must scope: an unscoped rescan can otherwise return a
 * colliding row listed by a different server. Unscoped stays the right call
 * for a code the user typed, which names no authority. */
export function findLobbyGameByCode(
  code: string,
  sourceUrl?: string,
): LobbyGameEntry | undefined {
  const normalized = code.trim().toUpperCase();
  const sources = lobbySources(useMultiplayerStore.getState());
  const scoped =
    sourceUrl === undefined
      ? sources
      : sources.filter((source) => source.url === sourceUrl);
  for (const source of scoped) {
    const game = subscriptionChannels
      .get(source.url)
      ?.snapshot
      ?.find((g) => g.game_code.toUpperCase() === normalized);
    if (game) return { game, source };
  }
  return undefined;
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
  /**
   * Where this client hosts and registers games — the P2P broker target and
   * the server-run hosting endpoint. `null` is the direct-codes sentinel:
   * no lobby is browsed and `MultiplayerPage` runs in P2P mode, so any
   * `userLobbySources` are inert until a hosting server is chosen again.
   * A non-null value is always a valid `ws(s)://` URL (enforced at
   * `setHostingServer`, migration and hydration).
   */
  hostingServer: string | null;
  /** Hand-added lobby authorities. Persisted; built-in presets are derived
   * per session by {@link lobbySources} and are never stored here. */
  userLobbySources: LobbySource[];
  /** Directory-listed authorities, as projected by
   * `services/serverDirectory.ts`. Rebuilt each session and never persisted; a
   * failed refresh leaves the last good list in place, which IS the last-good
   * fallback. */
  directorySources: DirectorySource[];
  /** When the last directory read completed (any HTTP status), or `null` when
   * none has. Owned here rather than in the service so tests reset it with the
   * same `setState` they reset every other store field with. */
  directoryFetchedAtMs: number | null;
  /** Directory sources the player switched off, by client-canonical URL.
   * PERSISTED: a disable is a preference, and it deliberately outlives the
   * entry vanishing from the directory and coming back. */
  disabledDirectorySources: string[];
  /** Per-source connection state, keyed by source URL. Ephemeral. */
  sourceStatus: Map<string, LobbySourceStatus>;
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
  /** Choose the hosting/registration server, or `null` for direct codes.
   * Invalid URLs are ignored. Refreshes the global `serverInfo` from the
   * new target's live socket, if it has one. */
  setHostingServer: (url: string | null) => void;
  /** Add a hand-added lobby source. Refuses malformed URLs, URLs already
   * derived as a source (presets included) and adds past the cap. */
  addUserLobbySource: (url: string) => AddLobbySourceResult;
  /** Remove a hand-added lobby source and close its channel. */
  removeUserLobbySource: (url: string) => void;
  /** Switch one directory listing on or off for this player. Disabling drops it
   * from the dialed set and tears its channel down; it does NOT delete the
   * listing, which the picker keeps showing so it can be switched back on. */
  setDirectorySourceEnabled: (url: string, enabled: boolean) => void;
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
  /** `serverUrl` is the server THIS game is hosted on, chosen at host-setup
   *  submit. It is deliberately not `hostingServer`: that field is the P2P /
   *  browsing anchor and choosing a game server for one match must not move
   *  it. */
  startHosting: (settings: HostingSettings, deck: HostingDeck, serverUrl: string) => void;
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
   * Lazily open one source's long-lived subscription socket and return the
   * `PhaseSocket`. Idempotent per URL: a second call while that channel's
   * open is in flight returns the same promise. Resolves `null` if the URL
   * is invalid or the handshake fails so callers can fall back rather than
   * crash.
   */
  ensureSubscriptionSocket: (url: string) => Promise<PhaseSocket | null>;
  /** Close and discard every source's subscription socket. Called on store
   * teardown. */
  closeSubscriptionSocket: () => void;
  /**
   * Send `JoinGameWithPassword` to `origin` and return a discriminated
   * `ResolveResult`. Opens that source's socket lazily if it's not yet
   * alive. Does NOT navigate — the caller inspects the result and handles
   * password retry, build mismatch, etc. before navigation.
   */
  resolveGuest: (
    code: string,
    origin: LobbySource,
    password?: string,
  ) => Promise<ResolveResult>;
  /**
   * Read-only typed-code lookup against `origin`. Returns format/routing
   * metadata without consuming a seat.
   */
  lookupJoinTarget: (
    code: string,
    origin: LobbySource,
    password?: string,
    opts?: Pick<
      LookupJoinTargetOptions,
      "reserve" | "displayName" | "releaseReservationToken"
    >,
  ) => Promise<LookupJoinTargetResult>;
  /**
   * Subscribe to lobby-list updates across every enabled source. `onUpdate`
   * fires once per source per snapshot, tagged with the source that listed
   * the rows, so a degraded source never blocks the others. Returns a
   * cleanup function that detaches listeners and sends `UnsubscribeLobby`,
   * or `null` when *every* source failed to open so the caller can render
   * a fallback.
   */
  subscribeLobby: (
    onUpdate: (games: LobbyGame[], source: LobbySource) => void,
  ) => Promise<(() => void) | null>;
  /**
   * Subscribe to the ambient frames every source's subscription socket
   * carries beside its listings, tagged with the source they arrived on.
   * Synchronous and dial-free: it rides the channels `subscribeLobby`
   * opens, and each channel re-attaches its listener on every reconnect, so
   * a subscriber keeps receiving frames across a flap without ever holding
   * a socket reference. Player counts are recorded on `sourceStatus` as
   * well as fanned out — read them from there.
   *
   * Delivery is coupled to `subscribeLobby`, not to this registration: the
   * per-channel ambient listeners attach only once a lobby subscriber is
   * registered and are dropped when the *last* one leaves. So a caller that
   * registers here without a live `subscribeLobby` subscription receives
   * nothing — silently, with no error and no dial of its own; this action
   * never opens a channel on its own behalf. Detaching is still required in
   * that case: an un-detached callback would start receiving frames again
   * the moment some other consumer's `subscribeLobby` re-attaches the
   * listeners.
   */
  subscribeAmbientLobby: (
    onFrame: (frame: AmbientLobbyFrame, source: LobbySource) => void,
  ) => () => void;
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

/**
 * v5 → v6: the single persisted `serverAddress` becomes a `hostingServer`
 * plus, for a hand-typed address, one `user` lobby source. An official or
 * build-default address is already derived as a preset, so it yields no
 * user source; the `""` direct-codes sentinel becomes `null`.
 */
export function migrateServerAddressToSources(serverAddress: unknown): {
  hostingServer: string | null;
  userLobbySources: LobbySource[];
} {
  if (serverAddress === "") {
    return { hostingServer: null, userLobbySources: [] };
  }
  const source = typeof serverAddress === "string" ? userLobbySource(serverAddress) : null;
  if (!source) {
    return { hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL, userLobbySources: [] };
  }
  const isBuiltIn =
    isOfficialMultiplayerServerUrl(source.url)
    || source.url === DEFAULT_MULTIPLAYER_SERVER_URL;
  return {
    hostingServer: source.url,
    userLobbySources: isBuiltIn ? [] : [source],
  };
}

/**
 * Persisted user sources are external input: rebuild every entry through
 * the URL canonicaliser, drop anything that is not a valid `user` row,
 * dedupe, and trim to the same cap the add path enforces so a hydrated blob
 * can never dial more sources than a user could have added.
 */
export function normalizeUserLobbySources(persisted: unknown): LobbySource[] {
  if (!Array.isArray(persisted)) return [];
  const sources: LobbySource[] = [];
  for (const entry of persisted) {
    if (!isRecord(entry) || entry.origin !== "user" || typeof entry.url !== "string") {
      continue;
    }
    const source = userLobbySource(entry.url);
    if (!source || sources.some((existing) => existing.url === source.url)) continue;
    sources.push(source);
    if (sources.length === MAX_USER_LOBBY_SOURCES) break;
  }
  return sources;
}

/**
 * Persisted disable preferences are external input, like every other persisted
 * field: rebuild each entry through `userLobbySource` — the same canonicaliser
 * the URLs were minted with — drop anything that is not a `ws(s)://` URL, and
 * dedupe.
 *
 * Deliberately UNCAPPED, unlike `userLobbySources`: every entry requires a
 * deliberate click on a row the directory listed, so the list is bounded by
 * user action, and a cap would silently start re-enabling the oldest disabled
 * server.
 */
export function normalizeDisabledDirectorySources(persisted: unknown): string[] {
  if (!Array.isArray(persisted)) return [];
  const urls: string[] = [];
  for (const entry of persisted) {
    if (typeof entry !== "string") continue;
    const source = userLobbySource(entry);
    if (!source || urls.includes(source.url)) continue;
    urls.push(source.url);
  }
  return urls;
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
  if (version < 6 && "serverAddress" in migrated) {
    const legacyAddress = migrated.serverAddress;
    delete migrated.serverAddress;
    return { ...migrated, ...migrateServerAddressToSources(legacyAddress) };
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
  serverUrl: string,
): void {
  if (!data.full_key || data.full_key.game_code !== data.game_code) return;
  const existing = loadWsSession();
  const hostSession = get().hostSession ?? existing?.hostSession;
  saveWsSession({
    gameCode: data.game_code,
    playerToken: data.player_token,
    fullKey: data.full_key,
    serverUrl,
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
  serverUrl: string,
): void {
  if (msg.type === "GameCreated") {
    const data = msg.data as {
      game_code: string;
      player_token: string;
      full_key?: { game_code: string; generation: number };
    };
    savePregameHostSession(get, data, serverUrl);
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
  serverUrl: string,
): Promise<void> {
  // The dialed URL arrives as an argument rather than being read from store
  // state, and every caller supplies the one the session records: every frame
  // this socket sends, and the session it records, must belong to the URL we
  // actually dialed. `startHosting` passes the host's choice for this game;
  // both re-dial paths pass `session.serverUrl`, which IS the record of what
  // was dialed — so a `setHostingServer` mid-game moves nothing here.
  const url = serverUrl;
  if (!isValidWebSocketUrl(url)) {
    resetServerHostSession(set);
    get().showToast("Invalid server address. Update it in Settings.");
    return;
  }

  let socket;
  try {
    socket = await openPhaseSocket(url);
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
    handleServerHostMessage(set, get, socket.ws, msg, url);
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
      // The session, never `hostingServer`: this is the live re-dial after a
      // mid-game host-socket drop, and it must return to the server the game
      // is actually on even if the browsing anchor has since moved.
      session.serverUrl,
    );
  }, delay);
}

/** The shared "we could not reach that authority" result, structurally
 * compatible with both broker RPC result types. */
type ConnectionLostResult = {
  ok: false;
  reason: "connection_lost";
  message: string;
};

/**
 * Run one join-adjacent RPC against a specific lobby authority.
 *
 * Opens (or reuses) that source's channel, registers an abort controller on
 * it so a mid-RPC `reconnecting` transition cuts the wait short, and — when
 * the URL is a one-off join origin rather than a browsed source — closes the
 * channel once the last RPC on it settles, so a `CODE@host` join leaves no
 * lingering reconnect loop behind.
 */
async function withOriginSocket<T>(
  set: MultiplayerSet,
  get: MultiplayerGet,
  url: string,
  run: (socket: PhaseSocket, signal: AbortSignal) => Promise<T>,
): Promise<T | ConnectionLostResult> {
  const socket = await get().ensureSubscriptionSocket(url);
  if (!socket) {
    // The failed open still created the channel and wrote an "offline"
    // status row. Tear both down on the same condition the success path
    // uses, or a mistyped host — the likeliest way to reach this branch —
    // accumulates a dead reconnect handle and a phantom status row per try.
    if (!lobbySources(get()).some((source) => source.url === url)) {
      closeChannel(set, get, url);
    }
    return {
      ok: false,
      reason: "connection_lost",
      message: "Lobby connection unavailable. Check your server address.",
    };
  }
  const channel = channelFor(url);
  const ac = new AbortController();
  channel.pendingRpcAborts.add(ac);
  try {
    return await run(socket, ac.signal);
  } finally {
    channel.pendingRpcAborts.delete(ac);
    if (
      channel.pendingRpcAborts.size === 0
      && !lobbySources(get()).some((source) => source.url === url)
    ) {
      closeChannel(set, get, url);
    }
  }
}

export const useMultiplayerStore = create<MultiplayerState & MultiplayerActions>()(
  persist(
    (set, get) => ({
      playerId: crypto.randomUUID(),
      displayName: "",
      hostingServer: DEFAULT_MULTIPLAYER_SERVER_URL as string | null,
      userLobbySources: [] as LobbySource[],
      directorySources: [] as DirectorySource[],
      directoryFetchedAtMs: null as number | null,
      disabledDirectorySources: [] as string[],
      sourceStatus: new Map<string, LobbySourceStatus>(),
      connectionStatus: "disconnected",
      activePlayerId: null,
      opponentDisplayName: null,
      toasts: new Map(),
      formatConfig: null,
      lastHostConfig: null,
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
      setHostingServer: (url) => {
        if (url !== null && !isValidWebSocketUrl(url)) return;
        if (url === get().hostingServer) return;
        // `serverInfo` is the hosting server's handshake identity (the
        // LobbyOnly-vs-Full branch reads it). Re-point it at the new
        // target's live socket, or clear it until that socket opens.
        const live = url === null
          ? null
          : subscriptionChannels.get(url)?.reconnect?.current() ?? null;
        set({ hostingServer: url, serverInfo: live?.serverInfo ?? null });
      },

      addUserLobbySource: (url) => {
        const source = userLobbySource(url);
        if (!source) return { ok: false, reason: "invalid_url" };
        // Duplicates are judged against presets and hand-added entries only,
        // so a preset URL cannot be re-added as a user entry; the cap counts
        // user entries only, so the allowance does not shift with the preset
        // count. A directory listing is transient and is SHADOWED by a user
        // entry (see `unshadowedDirectorySources`), so pinning a
        // currently-listed server as your own is the intended use, not a
        // duplicate.
        if (
          lobbySources(get()).some(
            (existing) => existing.origin !== "directory" && existing.url === source.url,
          )
        ) {
          return { ok: false, reason: "duplicate" };
        }
        if (get().userLobbySources.length >= MAX_USER_LOBBY_SOURCES) {
          return { ok: false, reason: "cap_reached" };
        }
        set({ userLobbySources: [...get().userLobbySources, source] });
        return { ok: true, source };
      },

      removeUserLobbySource: (url) => {
        set({
          userLobbySources: get().userLobbySources.filter((s) => s.url !== url),
        });
        // Hosting on a source the user no longer browses is unreachable: the
        // removed URL is absent from `lobbySources`, so the player's own
        // hosted game drops off the merged list and the picker's hosting
        // section (presets + None) shows no active selection to change.
        // Fall back to this build's official server through `setHostingServer`
        // so `serverInfo` is re-pointed with the choice.
        if (url === get().hostingServer) {
          get().setHostingServer(DEFAULT_MULTIPLAYER_SERVER_URL);
        }
        // The URL may still be browsed as a directory listing once the user
        // entry that shadowed it is gone; only tear the channel down when
        // nothing lists it, or the source would survive in `lobbySources` with
        // no socket until the next `LobbyView` mount.
        if (!lobbySources(get()).some((s) => s.url === url)) closeChannel(set, get, url);
      },

      setDirectorySourceEnabled: (url, enabled) => {
        const disabled = get().disabledDirectorySources;
        if (enabled) {
          set({ disabledDirectorySources: disabled.filter((entry) => entry !== url) });
          return;
        }
        if (!disabled.includes(url)) {
          set({ disabledDirectorySources: [...disabled, url] });
        }
        // A source the player switched off must stop holding a socket.
        // `closeChannel` also drops its `sourceStatus` row, so the picker stops
        // showing a stale status line for a source it is no longer dialing.
        closeChannel(set, get, url);
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

      startHosting: (settings, deck, serverUrl) => {
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
          serverUrl,
        );
      },

      resumeServerHosting: () => {
        if (hostWs || get().hostingStatus !== "idle") {
          return get().hostingStatus !== "idle";
        }

        const session = loadWsSession();
        // No comparison against `hostingServer`: the persisted session IS the
        // record of which server this game was hosted on, and a game hosted on
        // a server other than the browsing anchor is now an ordinary case, not
        // a reason to refuse the resume.
        if (!session?.hostSession) {
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
          session.serverUrl,
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
        const url = get().hostingServer;
        if (url === null) {
          console.error("[openBroker] no hosting server selected");
          return null;
        }
        try {
          const broker = await openBrokerClient(url);
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
            // Unreachable through `MultiplayerPage`: `useBroker` is only set
            // after the hosting server's own socket reported `LobbyOnly`. The
            // throw lands in this function's catch and resets hosting.
            const brokerUrl = get().hostingServer;
            if (brokerUrl === null) {
              throw new Error("No hosting server to register on.");
            }
            broker = await openBrokerClient(brokerUrl);
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

      ensureSubscriptionSocket: async (url) => {
        if (!isValidWebSocketUrl(url)) return null;
        // The protocol window, decided before the socket. A directory-listed
        // authority arrives with its versions already confirmed against that
        // server's own `/info` at announce time, so a verdict exists before any
        // handshake — and opening a socket to a server whose lobby version this
        // client cannot speak, ON THE VERSIONS THAT SERVER LAST ANNOUNCED, can
        // only produce a rejected handshake and a toast. The qualifier is load
        // bearing: the verdict is a snapshot, so a server that upgrades is
        // refused for up to the announce interval plus the directory TTL after
        // the next `LobbyView` mount (~6 min today); there is no timer, so a
        // session that never remounts the lobby keeps the verdict. That is the
        // cost of deciding before the socket, and the escape hatch below is
        // what makes it recoverable. The verdict is READ here, never recomputed:
        // `serverProtocolRejection` stays the only protocol-window authority
        // and `serverDirectory.ts` is the only place it is applied to a row.
        //
        // Keyed through `unshadowedDirectorySources`, NOT through the raw
        // `directorySources` field. The field is the unfiltered projection, so
        // a URL the user hand-added — or a preset URL — that the directory also
        // lists would otherwise be matched here and refused a socket,
        // contradicting the rule that preset and hand-added sources are judged
        // at the handshake. Pinning a listed server as your own is the
        // deliberate escape hatch: you opt back into the handshake's verdict,
        // which is the same authority applied to the identity the server
        // actually presents rather than the one it announced.
        //
        // Placed BEFORE `channelFor`: no channel is created and no
        // `sourceStatus` row is written, so a gated server does not read as
        // "offline" and does not light the degraded-sources chip.
        const listed = unshadowedDirectorySources(get()).find((d) => d.source.url === url);
        if (listed && listed.rejection !== null) return null;
        const channel = channelFor(url);
        // Fast path: handle is live and currently has a connected socket.
        const existing = channel.reconnect?.current();
        if (existing && existing.ws.readyState === WebSocket.OPEN) {
          return existing;
        }
        // Deduped first-open promise: concurrent callers await the same
        // `withReconnect` bootstrapping without racing handshakes.
        if (channel.firstOpen) return channel.firstOpen;

        channel.firstOpen = new Promise<PhaseSocket | null>((resolve) => {
          let settled = false;
          const settle = (val: PhaseSocket | null) => {
            if (settled) return;
            settled = true;
            resolve(val);
          };

          channel.reconnect = withReconnect(
            (attempt) => {
              // The announced key for this dial, resolved BEFORE the socket is
              // opened and latched into whatever report follows. `null` for a
              // preset, a hand-added source or a shadowed listing — those are
              // never reported, both because the directory would drop them and
              // because a private address must not leave this machine.
              const announced = announcedUrlFor(get(), url);
              // The full handshake round trip, not a ping: `openPhaseSocket`
              // resolves only after `ServerHello` is parsed, validated against
              // the protocol window, and `ClientHello` is sent. That is the
              // quantity the directory's histogram edges are scaled for.
              const startedAt = Date.now();
              // The shared subscription socket carries lobby frames only —
              // `SubscribeLobby`, the join-target RPCs, `PlayerCount`. Declaring
              // the surface keeps it usable against a server whose full-game
              // protocol has drifted from this build's, which is the whole point
              // of versioning the lobby separately. Server-run hosting and
              // joining open their own sockets and keep the exact-match window.
              return openPhaseSocket(url, { surface: "lobby" })
                .then((socket) => {
                  // FIRST attempt only. `scheduleRetry` bumps the index before
                  // re-invoking this factory, and a successful open resets it
                  // to 0 — so a re-dial always arrives as `attempt >= 1` and
                  // reports NOTHING. The cadence is therefore one outcome per
                  // channel HANDLE, recorded at its first open, and none on any
                  // re-dial: a flapping server contributes one report per
                  // handle rather than drowning the window in identical
                  // retries.
                  if (announced !== null && attempt === 0) {
                    reportConnectOutcome(announced, "connect_ok", Date.now() - startedAt);
                  }
                  return socket;
                })
                .catch((err) => {
                  if (announced !== null && attempt === 0) {
                    reportConnectOutcome(announced, "connect_fail");
                  }
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
                });
            },
            {
              // One retry on the initial open (~500ms to "offline") so the
              // user sees the `ServerOfflinePrompt` quickly when the server
              // is down, rather than after 6.5s of exponential backoff. The
              // prompt's "Keep trying" button remounts `LobbyView` and
              // starts a fresh retry cycle — recovery stays available.
              attempts: 1,
              onStateChange: (state) => {
                if (state === "open") {
                  const socket = channel.reconnect?.current() ?? null;
                  if (socket) {
                    setSourceStatus(set, get, url, {
                      state,
                      serverInfo: socket.serverInfo,
                      // A reconnect hands us a brand-new socket that has
                      // sent no `PlayerCount` yet. Carrying the pre-drop
                      // number over would advertise a count no live socket
                      // is backing; the next frame fills it in.
                      playerCount: null,
                    });
                    // `serverInfo` is the *hosting* server's identity — the
                    // LobbyOnly-vs-Full host branch reads it. Another
                    // source's handshake must not overwrite it.
                    if (url === get().hostingServer) {
                      set({ serverInfo: socket.serverInfo });
                    }
                    // Re-attach this channel's multiplexed lobby listener if
                    // any subscribers are registered AND this URL is still a
                    // browsed source — a channel opened only to carry a
                    // `CODE@host` RPC is never fanned out as a listing. The
                    // first snapshot from the server overwrites the cached
                    // one; stale data is not authoritative across a reconnect.
                    if (
                      lobbySubscribers.size > 0
                      && lobbySources(get()).some((source) => source.url === url)
                    ) {
                      attachLobbyListener(get, channel, url, socket);
                      // Same socket, same condition, same lifetime: the
                      // ambient listener has to follow the reconnect too,
                      // or this source silently stops reporting its player
                      // count and its `PasswordRequired` frames.
                      attachAmbientListener(set, get, channel, url, socket);
                    }
                  }
                  settle(socket);
                } else if (state === "reconnecting") {
                  // The row is rewritten, not merged: leaving `"open"` drops
                  // this source's `serverInfo` AND its player count, because
                  // the socket that reported them is gone.
                  setSourceStatus(set, get, url, {
                    state,
                    serverInfo: null,
                    playerCount: null,
                  });
                  // In-flight RPCs would otherwise hang until their own
                  // timeout. Abort them now so the caller can branch
                  // immediately. New RPCs registered after this point
                  // use fresh controllers and are unaffected.
                  for (const ac of channel.pendingRpcAborts) ac.abort();
                  channel.pendingRpcAborts.clear();
                  // Drop the handles to the old socket's listeners; both
                  // are re-bound on the next "open".
                  channel.attachDetach = null;
                  channel.ambientDetach = null;
                } else if (state === "offline") {
                  // Reconnect exhausted. This source is degraded; the others
                  // keep streaming. `ensureSubscriptionSocket` resolves
                  // `null` so the caller renders a fallback. Also drain any
                  // stragglers that joined between reconnecting and offline.
                  setSourceStatus(set, get, url, {
                    state,
                    serverInfo: null,
                    playerCount: null,
                  });
                  for (const ac of channel.pendingRpcAborts) ac.abort();
                  channel.pendingRpcAborts.clear();
                  settle(null);
                }
              },
            },
          );
        }).finally(() => {
          channel.firstOpen = null;
        });

        return channel.firstOpen;
      },

      closeSubscriptionSocket: () => {
        lobbySubscribers.clear();
        ambientSubscribers.clear();
        for (const url of [...subscriptionChannels.keys()]) {
          closeChannel(set, get, url);
        }
      },
      resolveGuest: async (code, origin, password) =>
        withOriginSocket(set, get, origin.url, (socket, signal) =>
          resolveGuestOver(socket, code, password, {
            signal,
            // The broker rejects a blank display_name on the resolve frame
            // (required-label rule) and the worker shell drops it without a
            // reply — the guest then times out at deck-select. Always carry
            // the player's name so the frame validates.
            displayName: get().displayName || "Player",
          }),
        ),

      lookupJoinTarget: async (code, origin, password, opts) =>
        withOriginSocket(set, get, origin.url, (socket, signal) =>
          lookupJoinTargetOver(socket, code, password, {
            signal,
            reserve: opts?.reserve,
            displayName: opts?.displayName,
            releaseReservationToken: opts?.releaseReservationToken,
          }),
        ),

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

      subscribeAmbientLobby: (onFrame) => {
        ambientSubscribers.add(onFrame);
        return () => {
          ambientSubscribers.delete(onFrame);
        };
      },

      subscribeLobby: async (onUpdate) => {
        // Register before dialing: each channel's "open" handler attaches its
        // own listener when subscribers exist, so a source that connects
        // while we are still awaiting a slower one starts streaming at once.
        lobbySubscribers.add(onUpdate);
        const sources = lobbySources(get());
        const sockets = await Promise.all(
          sources.map((source) => get().ensureSubscriptionSocket(source.url)),
        );

        let anyOpen = false;
        sources.forEach((source, index) => {
          const socket = sockets[index];
          if (!socket) return;
          anyOpen = true;
          const channel = channelFor(source.url);
          // First subscriber attaches this channel's listener. Later
          // subscribers ride the same upstream attachment — sending
          // `SubscribeLobby` again per subscriber, then detaching on their
          // own cleanup, would send `UnsubscribeLobby` on the shared socket
          // and silence every other subscriber (the ref-counting bug this
          // structure fixes) — and are seeded from the cached snapshot so
          // they don't wait on the next server push to render anything.
          if (channel.attachDetach === null) {
            attachLobbyListener(get, channel, source.url, socket);
            attachAmbientListener(set, get, channel, source.url, socket);
          } else if (channel.snapshot) {
            onUpdate(channel.snapshot, source);
          }
        });

        // Only "every source is unreachable" is an offline lobby. A single
        // degraded authority leaves the rest browsable. This waits for the
        // slowest source's first open before answering, which is bounded by
        // the handshake timeout; listings from faster sources have already
        // streamed to the subscriber by then.
        if (!anyOpen) {
          lobbySubscribers.delete(onUpdate);
          return null;
        }

        return () => {
          lobbySubscribers.delete(onUpdate);
          if (lobbySubscribers.size === 0) {
            for (const channel of subscriptionChannels.values()) {
              channel.attachDetach?.();
              channel.attachDetach = null;
              channel.ambientDetach?.();
              channel.ambientDetach = null;
              channel.snapshot = null;
            }
          }
        };
      },
    }),
    {
      name: "phase-multiplayer",
      version: 6,
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
      //
      // v5 → v6: the single `serverAddress` splits into `hostingServer` (where
      // this client hosts and registers) and `userLobbySources` (the
      // authorities it browses). A hand-typed address becomes both; an
      // official or build-default address is already derived as a preset, so
      // it becomes the hosting server only.
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
          userLobbySources: normalizeUserLobbySources(saved.userLobbySources),
          disabledDirectorySources: normalizeDisabledDirectorySources(
            saved.disabledDirectorySources,
          ),
          // `null` is a meaningful stored value (direct-codes mode), so it is
          // honoured; anything else that is not a valid URL falls back to the
          // initial hosting server rather than leaving the store unusable.
          hostingServer:
            typeof saved.hostingServer === "string" && isValidWebSocketUrl(saved.hostingServer)
              ? saved.hostingServer
              : saved.hostingServer === null
                ? null
                : current.hostingServer,
        };
      },
      partialize: (state) => ({
        playerId: state.playerId,
        displayName: state.displayName,
        hostingServer: state.hostingServer,
        userLobbySources: state.userLobbySources,
        // No persist version bump: an absent key hydrates through `merge` to
        // the initial `[]`, and an older build reading a newer blob spreads a
        // key it never reads and drops it on its next write. A bump would only
        // force `migratePersistedMultiplayerState` to grow an arm that does
        // nothing. `directorySources` / `directoryFetchedAtMs` stay out — the
        // projection is rebuilt each session, never persisted.
        disabledDirectorySources: state.disabledDirectorySources,
        lastHostConfig: state.lastHostConfig,
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
