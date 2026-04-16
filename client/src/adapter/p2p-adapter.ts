import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import type {
  EngineAdapter,
  FormatConfig,
  GameAction,
  GameEvent,
  GameState,
  LegalActionsResult,
  MatchConfig,
  PlayerId,
  SubmitResult,
} from "./types";

import { AdapterError } from "./types";
import { WasmAdapter } from "./wasm-adapter";
import { createPeerSession, type PeerSession } from "../network/peer";
import type { P2PMessage } from "../network/protocol";
import type {
  PlayerSlot,
  SeatKind,
  SeatMutation,
  SeatState,
  SeatMutationResult,
  SeatView,
} from "../multiplayer/seatTypes";
import type { BrokerClient } from "../services/brokerClient";
import {
  clearP2PHostSession,
  type PersistedP2PHostSession,
  saveP2PHostSession,
} from "../services/gamePersistence";
import { saveP2PSession } from "../services/p2pSession";

/**
 * Adapter-level events emitted to the UI. Wire-protocol messages are
 * snake_case (`player_kicked`); adapter events stay camelCase
 * (`playerKicked`). The adapter performs the remap inside its message
 * handlers — the UI never sees wire types.
 */
export type P2PAdapterEvent =
  | { type: "playerIdentity"; playerId: PlayerId; playerNames?: Record<number, string> }
  | { type: "roomCreated"; roomCode: string }
  | { type: "waitingForGuest" }
  | { type: "guestConnected" }
  | { type: "opponentDisconnected"; reason: string }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "error"; message: string }
  /**
   * Pre-game setup failure on the host side. Distinct from the catch-all
   * `error` event because it carries a typed `reason` for the UI to render
   * a specific remediation — not every setup error is the same problem.
   * Currently only `room_still_claimed` fires (PeerJS signaling server
   * still holds the prior host's peer-id registration after a fast
   * resume); future classifications slot in as additional `reason` arms.
   */
  | { type: "hostingFailed"; reason: "room_still_claimed"; message: string }
  | {
      type: "stateChanged";
      state: GameState;
      events: GameEvent[];
      legalResult: LegalActionsResult;
    }
  // 3-4p multiplayer additions:
  | {
      type: "opponentDisconnectedWithChoice";
      playerId: PlayerId;
      gracePeriodMs: number;
    }
  | { type: "playerKicked"; playerId: PlayerId; reason: string }
  | { type: "playerConceded"; playerId: PlayerId; reason: string }
  | { type: "playerReconnected"; playerId: PlayerId }
  | { type: "gamePaused"; reason: string }
  | { type: "gameResumed" }
  | { type: "lobbyProgress"; joined: number; total: number }
  | { type: "playerSlotsUpdated"; slots: PlayerSlot[] }
  | { type: "reconnecting"; attempt: number }
  | { type: "reconnectFailed"; reason: string };

type P2PAdapterEventListener = (event: P2PAdapterEvent) => void;

interface DeckListPayload {
  player: { main_deck: string[]; sideboard: string[]; commander: string[] };
  opponent: { main_deck: string[]; sideboard: string[]; commander: string[] };
  ai_decks: Array<{ main_deck: string[]; sideboard: string[]; commander: string[] }>;
}

/**
 * Game-run state. Typed enum (per CLAUDE.md §4: no raw bool flags).
 * - `running`     — normal play, `submitAction` accepted.
 * - `paused-disconnect` — automatic pause due to a guest dropping; auto-resumes
 *   on reconnect or auto-concedes at grace expiry. Blocks `submitAction`.
 * - `paused-manual` — host-initiated pause (either via "Pause and wait" on the
 *   disconnect dialog, or an explicit pause request). Released by host or by
 *   the dropped player reconnecting (see plan §6 DisconnectChoiceDialog
 *   semantics). Blocks `submitAction`.
 */
type GameRunState = "running" | "paused-disconnect" | "paused-manual";

/** Default grace window for guest auto-reconnect, in milliseconds. */
const DEFAULT_GRACE_PERIOD_MS = 30_000;
/**
 * Grace window applied to every persisted seat when a host resumes.
 * Wider than the in-flight default because the entire resume scenario
 * is "host was away for a while, guests may take a while to come
 * back." 5 minutes leaves headroom for browser reopen + tab warm-up +
 * reconnect backoff.
 */
const RESUME_GRACE_PERIOD_MS = 5 * 60_000;

/**
 * Guest auto-reconnect backoff schedule. Escalates briskly for early
 * attempts (WiFi blip case), then levels at 60s for the long tail.
 * After the explicit schedule, retries continue at `RECONNECT_STEADY_STATE_MS`
 * indefinitely until the adapter is `terminated` (explicit user leave).
 *
 * This tolerates host-resume scenarios where the host is down for
 * several minutes (browser crash + reopen + reconnect all happen
 * asynchronously). Giving up after 80s — the prior schedule — would
 * orphan guests whose host is in the middle of a legitimate resume.
 */
const RECONNECT_BACKOFF_MS = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000, 60_000];
const RECONNECT_STEADY_STATE_MS = 60_000;

function defaultSeatState(playerCount: number, formatConfig?: FormatConfig): SeatState {
  return {
    seats: [
      { type: "HostHuman" },
      ...Array.from({ length: playerCount - 1 }, () => ({ type: "WaitingHuman" as const })),
    ],
    tokens: Array.from({ length: playerCount }, (_, idx) => (idx === 0 ? "host" : "")),
    format: formatConfig ?? {
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
    gameStarted: false,
  };
}

function seatStateToView(state: SeatState): SeatView {
  return {
    seats: state.seats,
    format: state.format,
    isFull: state.seats.every((seat) => seat.type !== "WaitingHuman"),
    gameStarted: state.gameStarted,
  };
}

function occupiedSeatCount(state: SeatState): number {
  return state.seats.filter((seat) => seat.type !== "WaitingHuman").length;
}

function playerSlotsFromSeatView(view: SeatView): PlayerSlot[] {
  return view.seats.map((kind, playerId) => ({
    playerId,
    kind,
    name:
      playerId === 0
        ? "Host"
        : kind.type === "Ai"
          ? `AI (${kind.data.difficulty})`
          : kind.type === "WaitingHuman"
            ? ""
            : `Player ${playerId + 1}`,
  }));
}

function traceAdapter(side: "Host" | "Guest", event: string, data?: Record<string, unknown>): void {
  console.debug(`[P2P ${side} Adapter]`, performance.now().toFixed(1), event, data ?? {});
}

/**
 * Host-side P2P adapter.
 *
 * Hub-and-spoke topology: the host runs the authoritative WASM engine and
 * maintains one `PeerSession` per guest. State updates are filtered per-seat
 * via `wasm.getFilteredState(pid)` and fanned out to each guest. Guest
 * actions are routed through the host's WASM and re-broadcast as filtered
 * state.
 *
 * The host does NOT destroy the parent `Peer` on per-session disconnects —
 * that lifetime is owned by `dispose()`. Per-session cleanup releases only
 * the `DataConnection` (see `peer.ts` `onSessionEnd` contract).
 */
export class P2PHostAdapter implements EngineAdapter {
  private wasm = new WasmAdapter();
  private listeners: P2PAdapterEventListener[] = [];

  private guestSessions = new Map<PlayerId, PeerSession>();
  private guestDecks = new Map<PlayerId, DeckListPayload["player"]>();
  private aiDecks = new Map<PlayerId, DeckListPayload["player"]>();
  private playerTokens = new Map<PlayerId, string>();
  /**
   * Mid-game disconnect tracker. `timer` is nullable: it is set when the grace
   * window is armed (auto-concede on expiry) and nulled by `holdForReconnect`
   * (indefinite wait). Using `Timer | null` in the shape instead of a cast
   * keeps the "manual pause" transition type-honest (per CLAUDE.md: no raw
   * bool flags, no cast-arounds).
   */
  private disconnectedSeats = new Map<
    PlayerId,
    { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  private kickedTokens = new Set<string>();
  /**
   * Seats whose engine `PlayerId` has been conceded (CR 800.4a). Populated by
   * `concedePlayer`; used by `handleGuestMessage` to short-circuit actions
   * from already-eliminated guests without a WASM round-trip.
   */
  private eliminatedSeats = new Set<PlayerId>();
  private gameRunState: GameRunState = "running";

  private gameStarted = false;
  private guestDeckResolvers: Array<() => void> = [];
  private hostConnectionUnsub: (() => void) | null = null;
  private guestNames = new Map<PlayerId, string>();
  private hostDisplayName: string | null = null;
  private pregameSeatState: SeatState;
  private pregameOpQueue: Promise<void> = Promise.resolve();
  private allowPartialStart = false;

  /**
   * Identifier used as the key when this adapter writes its resume
   * metadata via `saveP2PHostSession`. Absent means the adapter is
   * running without persistence (tests, ephemeral hosts) — save-hooks
   * short-circuit as no-ops.
   */
  private readonly gameId: string | null;
  /** Bare 5-char room code without PEER_ID_PREFIX — persisted in the session record. */
  private readonly roomCode: string | null;
  /** True when the adapter was constructed from a persisted session (resume flow). */
  private readonly isResume: boolean;
  /**
   * Pending GameState snapshot to hand to `wasm.resumeMultiplayerHostState`
   * during `initialize()`. Set in the constructor from `resumeData.state`;
   * nulled after the WASM call consumes it. Held on the adapter rather
   * than threaded through `initialize()` so the EngineAdapter interface
   * stays uniform across fresh/resume flows.
   */
  private resumeGameState: GameState | null = null;

  constructor(
    private readonly hostDeckData: unknown,
    private readonly hostPeer: Peer,
    /**
     * Subscribe to inbound guest `DataConnection`s via `hostRoom()`'s
     * documented API. Using this (instead of `hostPeer.on("connection")`
     * directly) avoids double-dispatch with `hostRoom()`'s internal
     * listener, and drains any connections that were buffered while the
     * adapter was still under construction.
     */
    private readonly onGuestConnected: (
      handler: (conn: DataConnection) => void,
    ) => () => void,
    private readonly playerCount: number,
    private readonly formatConfig?: FormatConfig,
    private readonly matchConfig?: MatchConfig,
    private readonly gracePeriodMs: number = DEFAULT_GRACE_PERIOD_MS,
    /**
     * Optional broker that registered this room's lobby entry. When set,
     * the adapter fires `broker.unregister(brokerGameCode)` after a
     * successful `initializeGame` so the public listing disappears as
     * soon as the engine is live. Absent for legacy pure-PeerJS rooms
     * where no server-side listing exists.
     */
    private readonly broker?: BrokerClient,
    private readonly ownsBroker: boolean = true,
    /**
     * Server-assigned game code for the lobby entry the broker holds.
     * Required when `broker` is set; unused otherwise. Distinct from the
     * PeerJS peer ID the guest dials over.
     */
    private readonly brokerGameCode?: string,
    /**
     * Persistence binding for host resume. When provided, the adapter
     * writes a `PersistedP2PHostSession` snapshot at every lifecycle
     * event (guest join, reconnect, game start, kick, concede) so a
     * crashed/reloaded host can come back on the same room code.
     *
     * `resumeData` carries a prior session to rehydrate (for resume
     * flows) — the engine state is separately loaded via
     * `wasm.resumeMultiplayerHostState` in `initialize()`.
     */
    persistence?: {
      gameId: string;
      roomCode: string;
      hostDisplayName?: string;
      resumeData?: { state: GameState; session: PersistedP2PHostSession };
    },
  ) {
    if (playerCount < 2 || playerCount > 4) {
      throw new AdapterError(
        "P2P_PLAYER_COUNT",
        `P2P supports 2-4 players; got ${playerCount}`,
        false,
      );
    }
    if (broker && !brokerGameCode) {
      throw new AdapterError(
        "P2P_BROKER_CONFIG",
        "brokerGameCode is required when broker is provided",
        false,
      );
    }
    this.pregameSeatState = defaultSeatState(playerCount, formatConfig);
    this.gameId = persistence?.gameId ?? null;
    this.roomCode = persistence?.roomCode ?? null;
    this.hostDisplayName = persistence?.hostDisplayName ?? null;
    this.isResume = persistence?.resumeData !== undefined;

    if (persistence?.resumeData) {
      this.resumeGameState = persistence.resumeData.state;
      this.rehydrateFromPersistedSession(persistence.resumeData.session);
    }
  }

  /**
   * Restore in-memory adapter maps from a persisted session so the
   * resumed host agrees with its guests about seat assignments,
   * kicked tokens, and eliminated players. Called from the constructor
   * when `resumeData` is provided.
   *
   * Engine state is restored separately via
   * `wasm.resumeMultiplayerHostState` in `initialize()` — this method
   * only handles adapter-owned transport + security state.
   */
  private rehydrateFromPersistedSession(session: PersistedP2PHostSession): void {
    if (session.seatState) {
      this.pregameSeatState = session.seatState;
    }
    for (const [pidStr, token] of Object.entries(session.playerTokens)) {
      this.playerTokens.set(Number(pidStr), token);
    }
    for (const [pidStr, deck] of Object.entries(session.guestDecks)) {
      this.guestDecks.set(Number(pidStr), deck as DeckListPayload["player"]);
    }
    for (const [pidStr, deck] of Object.entries(session.aiDecks ?? {})) {
      this.aiDecks.set(Number(pidStr), deck as DeckListPayload["player"]);
    }
    for (const token of session.kickedTokens) this.kickedTokens.add(token);
    for (const pid of session.eliminatedSeats) {
      this.eliminatedSeats.add(pid);
    }
    this.gameStarted = session.gameStarted;

    // Every persisted guest is "disconnected" from the resumed host's
    // POV until they dial back in. Arming a grace window for each means
    // `handleReconnect` takes its existing valid path when a returning
    // guest sends their token — no special-case branch needed.
    // Skip the host seat (PlayerId 0) which is this adapter's owner.
    // Skip eliminated seats — already out, no grace needed.
    for (const pidStr of Object.keys(session.playerTokens)) {
      const pid = Number(pidStr);
      if (pid === 0) continue;
      if (this.eliminatedSeats.has(pid)) continue;
      this.armResumeGrace(pid);
    }
    // Mid-game resume: the game is paused until at least one guest
    // reconnects. Pre-game resume (lobby): state stays "running" since
    // `initializeGame` hasn't been called yet.
    if (this.gameStarted && this.disconnectedSeats.size > 0) {
      this.gameRunState = "paused-disconnect";
    }
  }

  /**
   * Open a reconnect grace window for a seat. Used only by resume
   * rehydration to pre-seed `disconnectedSeats` — mid-game disconnects
   * go through `handleGuestDisconnect` which carries additional
   * broadcast and run-state responsibilities.
   *
   * Uses `RESUME_GRACE_PERIOD_MS` (5m) rather than the default 30s:
   * returning guests may need to discover the host's revival through
   * their reconnect backoff, which can take minutes.
   */
  private armResumeGrace(pid: PlayerId): void {
    const timer = setTimeout(() => {
      if (!this.playerTokens.has(pid)) return;
      void this.concedePlayer(pid, "Resume grace expired", "conceded")
        .catch((err) => {
          console.error("[P2PHost] resume-grace auto-concede failed:", err);
        });
    }, RESUME_GRACE_PERIOD_MS);
    this.disconnectedSeats.set(pid, { disconnectedAt: Date.now(), timer });
  }

  /**
   * Build a persisted snapshot from the current in-memory adapter
   * state. Returns null when persistence isn't configured (tests,
   * ephemeral hosts) so save-hooks can short-circuit cleanly.
   */
  private buildPersistedSession(): PersistedP2PHostSession | null {
    if (!this.gameId || !this.roomCode) return null;
    const playerTokens: Record<number, string> = {};
    for (const [pid, token] of this.playerTokens.entries()) {
      playerTokens[pid] = token;
    }
    const guestDecks: Record<number, unknown> = {};
    for (const [pid, deck] of this.guestDecks.entries()) {
      guestDecks[pid] = deck;
    }
    const aiDecks: Record<number, unknown> = {};
    for (const [pid, deck] of this.aiDecks.entries()) {
      aiDecks[pid] = deck;
    }
    return {
      gameId: this.gameId,
      roomCode: this.roomCode,
      brokerGameCode: this.brokerGameCode,
      useBroker: this.broker !== undefined,
      playerTokens,
      guestDecks,
      aiDecks,
      kickedTokens: [...this.kickedTokens],
      eliminatedSeats: [...this.eliminatedSeats],
      playerCount: this.playerCount,
      formatConfig: this.formatConfig,
      matchConfig: this.matchConfig,
      hostDeckData: this.hostDeckData,
      gameStarted: this.gameStarted,
      seatState: this.pregameSeatState,
    };
  }

  getPlayerSlots(): PlayerSlot[] {
    return this.pregameSeatState.seats.map((kind, playerId) => ({
      playerId,
      kind,
      name: this.displayNameForSeat(playerId, kind),
    }));
  }

  private displayNameForSeat(playerId: number, kind: SeatKind): string {
    if (playerId === 0) {
      return this.hostDisplayName ?? "Host";
    }
    if (kind.type === "Ai") {
      return `AI (${kind.data.difficulty})`;
    }
    return this.guestNames.get(playerId) ?? "";
  }

  /**
   * Write the current adapter state to disk. Fire-and-forget:
   * lifecycle event handlers don't block on IDB. Failures are logged
   * but never thrown — losing a write means a slightly stale resume
   * snapshot, not a crash.
   */
  private saveSession(): void {
    if (!this.gameId) return;
    const snapshot = this.buildPersistedSession();
    if (!snapshot) return;
    void saveP2PHostSession(this.gameId, snapshot);
  }

  /**
   * Resolves the guest-deck gate in `initializeGame` so the engine starts
   * with whatever guests have connected so far. For 2p rooms this is
   * functionally "start now that the one guest is here"; for 3-4p rooms
   * it starts with fewer seats than configured — callers are responsible
   * for their own AI-seat-synthesis follow-up.
   *
   * Does NOT itself talk to the broker — the unregister call cascades
   * through `initializeGame`, which is the single authority for the
   * broker-side lifecycle (per CLAUDE.md's "single authority" rule).
   */
  startNow(): void {
    this.allowPartialStart = true;
    const resolvers = this.guestDeckResolvers.splice(0);
    for (const r of resolvers) r();
  }

  private enqueuePregameOp<T>(work: () => Promise<T>): Promise<T> {
    const next = this.pregameOpQueue.then(work, work);
    this.pregameOpQueue = next.then(() => undefined, () => undefined);
    return next;
  }

  private firstWaitingSeat(): PlayerId | null {
    for (let seat = 1; seat < this.pregameSeatState.seats.length; seat++) {
      if (this.pregameSeatState.seats[seat]?.type === "WaitingHuman") {
        return seat;
      }
    }
    return null;
  }

  private remapSeatMap<T>(source: Map<PlayerId, T>, remapping: Array<[number, number]>): Map<PlayerId, T> {
    const remapped = new Map<PlayerId, T>();
    for (const [pid, value] of source.entries()) {
      const mapped = remapping.find(([oldPid]) => oldPid === pid)?.[1] ?? pid;
      remapped.set(mapped, value);
    }
    return remapped;
  }

  private remapSeatSet(source: Set<PlayerId>, remapping: Array<[number, number]>): Set<PlayerId> {
    const remapped = new Set<PlayerId>();
    for (const pid of source.values()) {
      remapped.add(remapping.find(([oldPid]) => oldPid === pid)?.[1] ?? pid);
    }
    return remapped;
  }

  private broadcastSeatSnapshot(): void {
    const view = seatStateToView(this.pregameSeatState);
    for (const session of this.guestSessions.values()) {
      session.send({ type: "seat_snapshot", view });
    }
    this.emit({ type: "playerSlotsUpdated", slots: this.getPlayerSlots() });
  }

  private syncLobbyMetadata(): void {
    const currentPlayers = occupiedSeatCount(this.pregameSeatState);
    const maxPlayers = this.pregameSeatState.seats.length;
    this.emit({ type: "lobbyProgress", joined: currentPlayers, total: maxPlayers });
    if (this.broker && this.brokerGameCode) {
      this.broker.updateMetadata(this.brokerGameCode, currentPlayers, maxPlayers);
    }
  }

  async applySeatMutation(mutation: SeatMutation): Promise<void> {
    await this.enqueuePregameOp(async () => {
      if (this.gameStarted) {
        throw new AdapterError("P2P_ERROR", "Pregame seats can no longer be edited", false);
      }
      if (mutation.type === "Start") {
        throw new AdapterError("P2P_ERROR", "Use startPregameGame() for Start mutations", false);
      return;
      }

      const result = await this.wasm.applySeatMutation(
        JSON.stringify(this.pregameSeatState),
        JSON.stringify(mutation),
      ) as SeatMutationResult;

      for (const token of result.delta.invalidatedTokens) {
        for (const [pid, seatToken] of this.playerTokens.entries()) {
          if (seatToken !== token) continue;
          const session = this.guestSessions.get(pid);
          if (session) {
            session.send({ type: "kick", reason: "Removed from the room by the host" });
            try {
              session.close("Removed by host");
            } catch {
              /* best-effort */
            }
          }
          this.guestSessions.delete(pid);
          this.playerTokens.delete(pid);
          this.guestDecks.delete(pid);
          this.guestNames.delete(pid);
          break;
        }
      }

      for (const seatIndex of result.delta.removedAi) {
        this.aiDecks.delete(seatIndex);
      }
      for (const [seatIndex, _difficulty, deck] of result.delta.newAi) {
        this.aiDecks.set(seatIndex, deck as DeckListPayload["player"]);
      }

      if (result.delta.renumbering) {
        const { remapping } = result.delta.renumbering;
        this.guestSessions = this.remapSeatMap(this.guestSessions, remapping);
        this.guestDecks = this.remapSeatMap(this.guestDecks, remapping);
        this.aiDecks = this.remapSeatMap(this.aiDecks, remapping);
        this.playerTokens = this.remapSeatMap(this.playerTokens, remapping);
        this.guestNames = this.remapSeatMap(this.guestNames, remapping);
        this.disconnectedSeats = this.remapSeatMap(this.disconnectedSeats, remapping);
        this.eliminatedSeats = this.remapSeatSet(this.eliminatedSeats, remapping);
      }

      this.pregameSeatState = result.state;
      this.saveSession();
      for (const session of this.guestSessions.values()) {
        session.send({ type: "seat_mutate", mutation });
      }
      this.broadcastSeatSnapshot();
      this.syncLobbyMetadata();
    });
  }

  private async runAiLoop(): Promise<void> {
    if (!this.gameStarted) return;

    for (;;) {
      const state = await this.wasm.getState();
      if (!state || typeof state !== "object" || !("waiting_for" in state)) {
        return;
      }
      const waitingFor = state.waiting_for;
      if (!waitingFor || typeof waitingFor !== "object") {
        return;
      }
      if (!("data" in waitingFor) || !waitingFor.data || !("player" in waitingFor.data)) {
        return;
      }
      const aiSeat = this.pregameSeatState.seats[waitingFor.data.player];
      if (!aiSeat || aiSeat.type !== "Ai") {
        return;
      }
      const action = await this.wasm.getAiAction(aiSeat.data.difficulty, waitingFor.data.player);
      if (!action) {
        return;
      }
      const result = await this.wasm.submitAction(action, waitingFor.data.player);
      await this.broadcastStateUpdate(result.events);
      const nextState = await this.wasm.getState();
      const legalResult = await this.wasm.getLegalActions();
      this.emit({
        type: "stateChanged",
        state: nextState,
        events: result.events,
        legalResult,
      });
    }
  }

  onEvent(listener: P2PAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: P2PAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  async initialize(): Promise<void> {
    traceAdapter("Host", "initialize-start", { isResume: this.isResume });
    // Subscribe SYNCHRONOUSLY before any `await`. `hostRoom()` buffers
    // inbound guest connections that arrived between peer-open and the
    // first `onGuestConnected` subscribe, and flushes them into this
    // handler on subscribe — so no guest is dropped, even if the broker
    // registration + adapter construction held this call off for hundreds
    // of ms while `wasm.initialize()` was cold-loading.
    this.hostConnectionUnsub = this.onGuestConnected((conn) => {
      traceAdapter("Host", "handle-connection-event", { connOpen: conn.open });
      this.handleNewConnection(conn);
    });

    await this.wasm.initialize();
    // Resume path: load the persisted GameState with a fresh RNG seed
    // and atomic multiplayer-flag flip. `resumeMultiplayerHostState`
    // mirrors server-core's `from_persisted` pattern. Fresh-host path:
    // just flip the flag; engine state is populated by the guests
    // joining + `initializeGame`.
    if (this.isResume && this.resumeGameState) {
      await this.wasm.resumeMultiplayerHostState(this.resumeGameState);
      this.resumeGameState = null;
      traceAdapter("Host", "initialize-resume", {
        tokens: this.playerTokens.size,
        gameStarted: this.gameStarted,
      });
    } else {
      await this.wasm.setMultiplayerMode(true);
    }
    if (!this.gameStarted) {
      this.broadcastSeatSnapshot();
      this.syncLobbyMetadata();
    }
    traceAdapter("Host", "initialize-complete", {});
  }

  private handleNewConnection(conn: DataConnection): void {
    traceAdapter("Host", "handle-new-connection", { connOpen: conn.open });
    // Reconnect path: the first message determines whether this is a fresh
    // join or a reconnect. We attach a one-shot pre-handler to peek at the
    // first message before wrapping in a PeerSession with full handlers.
    const session = createPeerSession(conn, {
      onSessionEnd: () => {
        // Find which seat this session belonged to (if any) and route to the
        // appropriate disconnect handler.
        for (const [pid, s] of this.guestSessions.entries()) {
          if (s === session) {
            this.handleGuestDisconnect(pid);
            return;
          }
        }
      },
    });

    let identified = false;
    const unsub = session.onMessage((msg) => {
      if (identified) return;
      identified = true;
      unsub();

      if (msg.type === "reconnect") {
        traceAdapter("Host", "first-message", { type: msg.type });
        this.handleReconnect(session, msg.playerToken);
      } else if (msg.type === "guest_deck") {
        traceAdapter("Host", "first-message", { type: msg.type });
        this.handleNewGuest(session, msg.deckData, msg.displayName);
      } else {
        traceAdapter("Host", "first-message", { type: msg.type });
        // Unexpected first message — reject.
        session.send({
          type: "reconnect_rejected",
          reason: "Expected guest_deck or reconnect as first message",
        });
        session.close("Protocol violation");
      }
    });
  }

  private handleNewGuest(session: PeerSession, deckData: unknown, displayName?: string): void {
    if (this.gameStarted) {
      // Lobby is closed; reject. (Reconnect should have been used instead.)
      session.send({ type: "kick", reason: "Game already in progress" });
      session.close("Game in progress");
      return;
    }
    const pid = this.firstWaitingSeat();
    if (pid === null) {
      session.send({ type: "kick", reason: "Lobby full" });
      session.close("Lobby full");
      return;
    }

    // Issue a token immediately on join. The guest needs it for reconnect
    // even during the deck-collection window.
    const token = crypto.randomUUID();
    this.playerTokens.set(pid, token);
    this.guestSessions.set(pid, session);
    this.guestDecks.set(pid, (deckData as DeckListPayload).player);
    if (displayName) this.guestNames.set(pid, displayName);
    this.pregameSeatState.seats[pid] = { type: "JoinedHuman" };
    this.pregameSeatState.tokens[pid] = token;
    this.saveSession();

    // Route subsequent messages from this guest.
    session.onMessage((msg) => this.handleGuestMessage(pid, msg));

    this.broadcastSeatSnapshot();
    this.syncLobbyMetadata();
  }

  async initializeGame(): Promise<SubmitResult> {
    return this.startPregameGame();
  }

  async startPregameGame(): Promise<SubmitResult> {
    return this.enqueuePregameOp(() => this.startPregameGameInner());
  }

  private async startPregameGameInner(): Promise<SubmitResult> {
      if (this.gameStarted) {
        return { events: [] };
      }
      const allowPartialStart = this.allowPartialStart;
      this.allowPartialStart = false;
      const hasWaitingSeats = this.pregameSeatState.seats.some((seat) => seat.type === "WaitingHuman");
      if (hasWaitingSeats && !allowPartialStart) {
        throw new AdapterError("P2P_ERROR", "Fill or remove all open seats before starting", false);
      }

      const hostDeck = this.hostDeckData as DeckListPayload;
      const orderedOpponents: DeckListPayload["player"][] = [];
      for (let seat = 1; seat < this.pregameSeatState.seats.length; seat++) {
        const kind = this.pregameSeatState.seats[seat];
        if (kind.type === "JoinedHuman") {
          const deck = this.guestDecks.get(seat);
          if (!deck) {
            throw new AdapterError("P2P_ERROR", `Seat ${seat} has no submitted deck`, false);
          }
          orderedOpponents.push(deck);
          continue;
        }
        if (kind.type === "Ai") {
          const deck = this.aiDecks.get(seat);
          if (!deck) {
            throw new AdapterError("P2P_ERROR", `AI seat ${seat} is missing a resolved deck`, false);
          }
          orderedOpponents.push(deck);
        }
      }
      if (orderedOpponents.length === 0) {
        throw new AdapterError("P2P_ERROR", "Cannot start P2P game with zero opponents", false);
      }

      const deckPayload: DeckListPayload = {
        player: hostDeck.player,
        opponent: orderedOpponents[0],
        ai_decks: orderedOpponents.slice(1),
      };
      const playerCount = allowPartialStart
        ? orderedOpponents.length + 1
        : this.pregameSeatState.seats.length;
      const result = await this.wasm.initializeGame(
        deckPayload,
        this.formatConfig,
        playerCount,
        this.matchConfig,
        undefined,
      );
      this.gameStarted = true;
      this.pregameSeatState.gameStarted = true;
      this.saveSession();

      const allNames: Record<number, string> = {};
      if (this.hostDisplayName) allNames[0] = this.hostDisplayName;
      for (const [pid, name] of this.guestNames) allNames[pid] = name;
      this.emit({ type: "playerIdentity", playerId: 0, playerNames: allNames });

      if (this.broker && this.brokerGameCode) {
        void this.broker.unregister(this.brokerGameCode).catch(() => {
          /* best-effort */
        });
      }

      const legal = await this.wasm.getLegalActions();
      for (const [pid, session] of this.guestSessions) {
        const token = this.playerTokens.get(pid)!;
        const filteredState = await this.wasm.getFilteredState(pid);
        session.send({
          type: "game_setup",
          assignedPlayerId: pid,
          playerToken: token,
          state: filteredState,
          events: result.events,
          legalActions: legal.actions,
          autoPassRecommended: legal.autoPassRecommended,
          playerNames: allNames,
        });
      }

      await this.runAiLoop();
      return result;
  }

  async submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult> {
    // Host's own UI submissions: `actor` is the host's local PlayerId (the
    // caller — gameStore — derived it from `getPlayerId()`). The host is
    // the trust boundary for its own actions; the engine's guard still
    // verifies the actor against `authorized_submitter(state)`.
    if (this.gameRunState !== "running") {
      throw new AdapterError(
        "P2P_PAUSED",
        `Cannot submit action while game state is ${this.gameRunState}`,
        true,
      );
    }
    const result = await this.wasm.submitAction(action, actor);
    await this.broadcastStateUpdate(result.events);
    await this.runAiLoop();
    return result;
  }

  /**
   * Fan out a state update to every connected guest. Each guest gets a
   * separately-filtered snapshot via `wasm.getFilteredState(pid)`. Skips
   * disconnected seats (their state is delivered via `reconnect_ack`).
   */
  private async broadcastStateUpdate(events: GameEvent[]): Promise<void> {
    const legal = await this.wasm.getLegalActions();
    for (const [pid, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(pid)) continue;
      const filteredState = await this.wasm.getFilteredState(pid);
      session.send({
        type: "state_update",
        state: filteredState,
        events,
        legalActions: legal.actions,
        autoPassRecommended: legal.autoPassRecommended,
      });
    }
  }

  async getState(): Promise<GameState> {
    return this.wasm.getState();
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.wasm.getLegalActions();
  }

  getAiAction(_difficulty: string): GameAction | null {
    return null;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  /**
   * Release all transport + engine resources. PRESERVES the persisted
   * resume record so a subsequent reload can pick up the game. Called
   * on React unmount (navigation, StrictMode remount, tab close).
   *
   * Explicit user quit goes through `terminateGame()` instead, which
   * clears the persistence before disposing.
   */
  dispose(): void {
    if (this.hostConnectionUnsub) this.hostConnectionUnsub();
    for (const { timer } of this.disconnectedSeats.values()) {
      if (timer !== null) clearTimeout(timer);
    }
    this.disconnectedSeats.clear();
    for (const session of this.guestSessions.values()) {
      session.close();
    }
    this.guestSessions.clear();
    this.kickedTokens.clear();
    this.playerTokens.clear();
    this.guestDecks.clear();
    this.aiDecks.clear();
    try {
      this.hostPeer.destroy();
    } catch {
      /* best-effort */
    }
    this.wasm.dispose();
    // Close the broker only when the adapter owns it. When the multiplayer
    // store owns the broker (externally managed), it survives adapter disposal
    // so the lobby entry stays alive across page navigations.
    if (this.ownsBroker) {
      this.broker?.close();
    }
    this.listeners = [];
  }

  /**
   * Explicit user quit — clears the persisted resume record so the
   * menu's Resume button won't surface this game next session, then
   * delegates to `dispose()` for teardown.
   *
   * Callers: "Leave game" affordance, game-over cleanup, concede flows
   * that should end the session permanently. Should NOT be called from
   * component unmount / tab close / StrictMode remount — those need
   * persistence preserved and go through `dispose()`.
   */
  terminateGame(): void {
    // Notify every live guest session BEFORE dispose tears the sessions down.
    // Without this, guests interpret the ensuing DataConnection close as a
    // transient network drop and burn through the full reconnect backoff
    // (minutes of doomed retries against a Peer that was just destroyed).
    // The wire message is sent synchronously while the sessions are still
    // open; PeerJS buffers the RTCDataChannel write, and `dispose()` below
    // runs on the next line so the message flushes before the channel tears
    // down. This broadcast is intentionally skipped on `dispose()` — plain
    // unmounts (StrictMode remount, tab close, navigation) may be transient
    // and the guest's reconnect loop is the correct behavior there.
    for (const session of this.guestSessions.values()) {
      session.send({ type: "host_left", reason: "Host left the game" });
    }
    if (this.gameId) {
      void clearP2PHostSession(this.gameId);
    }
    this.dispose();
  }

  private async handleGuestMessage(
    pid: PlayerId,
    msg: P2PMessage,
  ): Promise<void> {
    switch (msg.type) {
      case "action": {
        // Verify sender identity to prevent guest 2 spoofing as guest 3.
        if (msg.senderPlayerId !== pid) {
          const session = this.guestSessions.get(pid);
          if (session) {
            session.send({
              type: "action_rejected",
              reason: `senderPlayerId mismatch (declared ${msg.senderPlayerId}, session owns ${pid})`,
            });
          }
          console.warn(
            `[P2PHost] rejected action from seat ${pid} with declared sender ${msg.senderPlayerId}`,
          );
          return;
        }
        // Short-circuit: an eliminated seat (post-concede) has no legal
        // actions in the engine. Reject at the adapter so the wire log is
        // clear and the WASM round-trip is skipped.
        if (this.eliminatedSeats.has(pid)) {
          const session = this.guestSessions.get(pid);
          if (session) {
            session.send({
              type: "action_rejected",
              reason: "Player has conceded and can no longer act",
            });
          }
          return;
        }
        if (this.gameRunState !== "running") {
          const session = this.guestSessions.get(pid);
          if (session) {
            session.send({
              type: "action_rejected",
              reason: `Game ${this.gameRunState}`,
            });
          }
          return;
        }
        try {
          // CRITICAL: pass `pid` (the session-bound PlayerId), NEVER
          // `msg.senderPlayerId`. The envelope check above already guarantees
          // they match, but if we ever regressed that check we must still
          // tag with the authenticated session identity — the wire payload
          // is untrusted. This is the defense-in-depth that makes the engine
          // guard meaningful for P2P.
          const result = await this.wasm.submitAction(msg.action, pid);
          await this.broadcastStateUpdate(result.events);
          // Emit local stateChanged so host UI updates for opponent actions.
          const state = await this.wasm.getState();
          const legalResult = await this.wasm.getLegalActions();
          this.emit({
            type: "stateChanged",
            state,
            events: result.events,
            legalResult,
          });
        } catch (err) {
          const reason = err instanceof Error ? err.message : String(err);
          const session = this.guestSessions.get(pid);
          if (session) session.send({ type: "action_rejected", reason });
        }
        break;
      }
      case "concede": {
        // CR 104.3a: Any player may concede at any time. Route through the
        // engine action so the seat is properly eliminated (CR 800.4a).
        await this.concedePlayer(pid, "Player conceded", "conceded");
        // Notify remaining guests with the "conceded" wire variant (not
        // "kicked") so their log entries read correctly.
        for (const [otherPid, s] of this.guestSessions) {
          if (otherPid === pid) continue;
          s.send({
            type: "player_conceded",
            playerId: pid,
            reason: "Player conceded",
          });
        }
        break;
      }
      default:
        break;
    }
  }

  private handleGuestDisconnect(pid: PlayerId): void {
    if (!this.guestSessions.has(pid)) return;
    if (this.disconnectedSeats.has(pid)) return;

    this.guestSessions.delete(pid);

    if (!this.gameStarted) {
      // Pre-game disconnect: free the seat back to the lobby. Drop the token
      // (no reconnect path before game start). The seat number is reused via
      // `nextSeat` rewind so the next joiner takes the same slot.
      this.playerTokens.delete(pid);
      this.guestDecks.delete(pid);
      this.guestNames.delete(pid);
      this.pregameSeatState.seats[pid] = { type: "WaitingHuman" };
      this.pregameSeatState.tokens[pid] = "";
      this.saveSession();
      this.broadcastSeatSnapshot();
      this.syncLobbyMetadata();
      return;
    }

    // Mid-game disconnect: enter grace window.
    const timer = setTimeout(() => {
      // Grace expired without reconnect AND without host decision — auto-concede.
      if (this.gameRunState === "paused-disconnect") {
        this.concedePlayer(pid, "Disconnect grace expired", "conceded")
          .then(() => {
            // Notify remaining guests that the seat was conceded past (not kicked).
            for (const [otherPid, s] of this.guestSessions) {
              if (otherPid === pid) continue;
              s.send({
                type: "player_conceded",
                playerId: pid,
                reason: "Disconnect grace expired",
              });
            }
          })
          .catch((err) => {
            console.error("[P2PHost] auto-concede failed:", err);
          });
      }
    }, this.gracePeriodMs);

    this.disconnectedSeats.set(pid, {
      disconnectedAt: Date.now(),
      timer,
    });
    this.gameRunState = "paused-disconnect";

    // Notify remaining guests.
    for (const [otherPid, session] of this.guestSessions) {
      if (otherPid === pid) continue;
      session.send({ type: "player_disconnected", playerId: pid });
      session.send({ type: "game_paused", reason: "Player disconnected" });
    }

    this.emit({
      type: "opponentDisconnectedWithChoice",
      playerId: pid,
      gracePeriodMs: this.gracePeriodMs,
    });
    this.emit({ type: "gamePaused", reason: "Player disconnected" });
  }

  private handleReconnect(session: PeerSession, playerToken: string): void {
    if (this.kickedTokens.has(playerToken)) {
      session.send({ type: "reconnect_rejected", reason: "Player kicked" });
      session.close("Kicked");
      return;
    }
    let pid: PlayerId | null = null;
    for (const [seat, token] of this.playerTokens) {
      if (token === playerToken) {
        pid = seat;
        break;
      }
    }
    if (pid === null) {
      session.send({ type: "reconnect_rejected", reason: "Unknown token" });
      session.close("Unknown token");
      return;
    }
    if (!this.disconnectedSeats.has(pid)) {
      session.send({
        type: "reconnect_rejected",
        reason: "No grace window active for this seat",
      });
      session.close("Not in grace");
      return;
    }

    const grace = this.disconnectedSeats.get(pid)!;
    if (grace.timer !== null) clearTimeout(grace.timer);
    this.disconnectedSeats.delete(pid);
    this.guestSessions.set(pid, session);

    // Wire subsequent messages from this guest.
    session.onMessage((msg) => this.handleGuestMessage(pid as PlayerId, msg));

    // Send fresh state to the reconnecting guest.
    void (async () => {
      const filteredState = await this.wasm.getFilteredState(pid as PlayerId);
      const legal = await this.wasm.getLegalActions();
      session.send({
        type: "reconnect_ack",
        assignedPlayerId: pid as PlayerId,
        state: filteredState,
        legalActions: legal.actions,
        autoPassRecommended: legal.autoPassRecommended,
      });
    })();

    // Notify other guests.
    for (const [otherPid, otherSession] of this.guestSessions) {
      if (otherPid === pid) continue;
      otherSession.send({ type: "player_reconnected", playerId: pid });
    }
    this.emit({ type: "playerReconnected", playerId: pid });

    // Resume if no other seats are paused.
    if (this.disconnectedSeats.size === 0 && this.gameRunState === "paused-disconnect") {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        s.send({ type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
  }

  /**
   * Concede origin. Distinguishes the three paths that all end at
   * `eliminate_player` so wire broadcasts and local adapter events carry the
   * correct semantic label. CR 104.3a applies uniformly, but UIs need to
   * differentiate "kicked by host" from "left voluntarily" from "host
   * continued past disconnect".
   */
  private async concedePlayer(
    pid: PlayerId,
    reason: string,
    origin: "kick" | "conceded",
  ): Promise<void> {
    // Cancel any active grace timer for this seat. `timer` may be null if the
    // host already called `holdForReconnect`.
    const grace = this.disconnectedSeats.get(pid);
    if (grace) {
      if (grace.timer !== null) clearTimeout(grace.timer);
      this.disconnectedSeats.delete(pid);
    }
    // Remove the session for self-concede / grace-expiry paths. (The kick
    // path removes its own session before calling concedePlayer so it can
    // send the `kick` wire message first; double-deletion is a no-op here.)
    const session = this.guestSessions.get(pid);
    if (session) {
      this.guestSessions.delete(pid);
      try { session.close("Player conceded"); } catch { /* best-effort */ }
    }
    this.eliminatedSeats.add(pid);
    this.saveSession();
    try {
      const concedeAction = {
        type: "Concede",
        data: { player_id: pid },
      } as unknown as GameAction;
      // Concede's engine guard requires `actor === player_id`. `pid` is both
      // the seat being conceded and the authenticated identity we're acting
      // on behalf of (e.g. grace-expiry or kick).
      const result = await this.wasm.submitAction(concedeAction, pid);
      await this.broadcastStateUpdate(result.events);
      await this.runAiLoop();
      const state = await this.wasm.getState();
      const legalResult = await this.wasm.getLegalActions();
      this.emit({
        type: "stateChanged",
        state,
        events: result.events,
        legalResult,
      });
      this.emit(
        origin === "kick"
          ? { type: "playerKicked", playerId: pid, reason }
          : { type: "playerConceded", playerId: pid, reason },
      );
    } catch (err) {
      console.error("[P2PHost] concedePlayer failed:", err);
    }
    // Resume game state if this concede unblocked the pause.
    if (
      this.disconnectedSeats.size === 0 &&
      this.gameRunState === "paused-disconnect"
    ) {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        s.send({ type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
  }

  // ────────────────────────────────────────────────────────────────────────
  // Public host-only controls (called by UI components).
  // ────────────────────────────────────────────────────────────────────────

  /**
   * Forcibly remove a player from the game. CR 104.3a: kicked players forfeit.
   * Adds the seat's token to the denylist so they cannot reconnect.
   */
  async kickPlayer(pid: PlayerId, reason: string = "Kicked by host"): Promise<void> {
    const token = this.playerTokens.get(pid);
    if (token) this.kickedTokens.add(token);
    // Persist the kick before the session close — the kickedTokens set
    // survives host reload so a kicked guest can't sneak back in on
    // resume.
    this.saveSession();
    // Remove session BEFORE concedePlayer so we can send the `kick` wire
    // message on the way out; concedePlayer's own session-cleanup is a no-op
    // for an already-removed seat.
    const session = this.guestSessions.get(pid);
    if (session) {
      session.send({ type: "kick", reason });
      try { session.close("Kicked"); } catch { /* best-effort */ }
      this.guestSessions.delete(pid);
    }
    await this.concedePlayer(pid, reason, "kick");
    // Broadcast kick to remaining guests (concedePlayer emits playerKicked
    // locally; remaining peers need the wire message).
    for (const [otherPid, s] of this.guestSessions) {
      if (otherPid === pid) continue;
      s.send({ type: "player_kicked", playerId: pid, reason });
    }
  }

  /**
   * Continue the game without the disconnected player (auto-concede).
   * Cancels their grace timer and routes to `concedePlayer`.
   */
  async concedeDisconnected(pid: PlayerId): Promise<void> {
    const reason = "Host continued without reconnecting player";
    await this.concedePlayer(pid, reason, "conceded");
    for (const [otherPid, s] of this.guestSessions) {
      if (otherPid === pid) continue;
      s.send({ type: "player_conceded", playerId: pid, reason });
    }
  }

  /**
   * Convert an active "paused-disconnect" into "paused-manual" — cancels the
   * grace timer so the game waits indefinitely for the player to reconnect.
   * The `disconnectedSeats` entry is preserved so the reconnect path still
   * fires; only the auto-concede timer is cancelled.
   */
  holdForReconnect(pid: PlayerId): void {
    const grace = this.disconnectedSeats.get(pid);
    if (grace) {
      if (grace.timer !== null) clearTimeout(grace.timer);
      // Null out the timer field (typed `Timer | null`). The reconnect handler
      // branches on null-or-not before calling `clearTimeout`.
      this.disconnectedSeats.set(pid, {
        disconnectedAt: grace.disconnectedAt,
        timer: null,
      });
    }
    this.gameRunState = "paused-manual";
  }

  /** Manually pause (host UI). */
  requestPause(): void {
    if (this.gameRunState === "running") {
      this.gameRunState = "paused-manual";
      for (const [, s] of this.guestSessions) {
        s.send({ type: "game_paused", reason: "Paused by host" });
      }
      this.emit({ type: "gamePaused", reason: "Paused by host" });
    }
  }

  /** Manually resume (host UI). Only resumes if no seats are still disconnected. */
  requestResume(): void {
    if (
      this.gameRunState === "paused-manual" &&
      this.disconnectedSeats.size === 0
    ) {
      this.gameRunState = "running";
      for (const [, s] of this.guestSessions) {
        s.send({ type: "game_resumed" });
      }
      this.emit({ type: "gameResumed" });
    }
  }
}

/**
 * Guest-side P2P adapter. Maintains the `Peer` reference for auto-reconnect,
 * persists session token to `sessionStorage` (via `p2pSession` service), and
 * applies host-broadcasted state updates locally.
 */
export class P2PGuestAdapter implements EngineAdapter {
  private gameState: GameState | null = null;
  private legalActions: LegalActionsResult = {
    actions: [],
    autoPassRecommended: false,
  };
  private listeners: P2PAdapterEventListener[] = [];
  private pendingResolve: ((result: SubmitResult) => void) | null = null;
  private pendingReject: ((error: Error) => void) | null = null;
  private session: PeerSession | null = null;
  private playerToken: string | null = null;
  private assignedPlayerId: PlayerId | null = null;
  /**
   * Once true, the adapter is in a terminal state (kicked, reconnect rejected,
   * or disposed). `handleHostDisconnect` bails out so the auto-reconnect loop
   * does NOT fire — preventing a kicked guest from spinning ~30s of backoff
   * attempts against a token they'll never be accepted with.
   */
  private terminated = false;

  // Promise resolved on game_setup OR reconnect_ack, whichever arrives first.
  // Reconnecting guests take the `reconnect_ack` path, so `initializeGame()`
  // must resolve there too or it will hang indefinitely.
  private gameSetupPromise: Promise<SubmitResult>;
  private gameSetupResolve!: (result: SubmitResult) => void;
  private gameSetupSettled = false;

  constructor(
    private readonly deckData: unknown,
    private readonly hostPeer: Peer,
    private readonly hostPeerId: string,
    private readonly initialConn: DataConnection,
    existingPlayerToken?: string,
    private readonly displayName?: string,
  ) {
    if (existingPlayerToken) {
      this.playerToken = existingPlayerToken;
    }
    this.gameSetupPromise = new Promise<SubmitResult>((resolve) => {
      this.gameSetupResolve = resolve;
    });
  }

  onEvent(listener: P2PAdapterEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: P2PAdapterEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  async initialize(): Promise<void> {
    traceAdapter("Guest", "initialize-start", { hasPlayerToken: Boolean(this.playerToken) });
    this.attachSession(this.initialConn);
    if (this.playerToken) {
      traceAdapter("Guest", "send-reconnect", { hostPeerId: this.hostPeerId });
      this.session!.send({ type: "reconnect", playerToken: this.playerToken });
    } else {
      traceAdapter("Guest", "send-guest-deck", { hostPeerId: this.hostPeerId });
      this.session!.send({ type: "guest_deck", deckData: this.deckData, displayName: this.displayName });
    }
  }

  private attachSession(conn: DataConnection): void {
    traceAdapter("Guest", "attach-session", { connOpen: conn.open });
    const session = createPeerSession(conn, {
      onSessionEnd: () => {
        this.handleHostDisconnect();
      },
    });
    this.session = session;
    session.onMessage((msg) => this.handleHostMessage(msg));
  }

  async initializeGame(): Promise<SubmitResult> {
    return this.gameSetupPromise;
  }

  async submitAction(action: GameAction, _actor: PlayerId): Promise<SubmitResult> {
    // `_actor` is unused: the host re-tags the incoming action with the
    // PlayerId bound to this WebRTC session at join time. `senderPlayerId`
    // on the wire is kept for the host's envelope-level sanity check
    // (rejects early with a clear diagnostic) but is NEVER used by the host
    // as the engine `actor`. If this client were malicious and claimed
    // another identity, the host would detect the mismatch and drop the
    // action before touching the engine.
    if (!this.session) {
      throw new AdapterError(
        "P2P_ERROR",
        "Not connected to host",
        true,
      );
    }
    if (this.assignedPlayerId === null) {
      throw new AdapterError(
        "P2P_ERROR",
        "Not yet assigned a player ID",
        true,
      );
    }
    return new Promise<SubmitResult>((resolve, reject) => {
      this.pendingResolve = resolve;
      this.pendingReject = reject;
      this.session!.send({
        type: "action",
        senderPlayerId: this.assignedPlayerId!,
        action,
      });
    });
  }

  async getState(): Promise<GameState> {
    if (!this.gameState) {
      throw new AdapterError("P2P_ERROR", "No game state available", false);
    }
    return this.gameState;
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return this.legalActions;
  }

  getAiAction(_difficulty: string): GameAction | null {
    return null;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError("P2P_ERROR", "Undo not supported in P2P games", false);
  }

  dispose(): void {
    // Mark terminal BEFORE closing the session so the session's
    // `onSessionEnd` → `handleHostDisconnect` short-circuit fires and skips
    // the auto-reconnect loop.
    this.terminated = true;
    if (this.session) {
      this.session.close();
      this.session = null;
    }
    try {
      this.hostPeer.destroy();
    } catch {
      /* best-effort */
    }
    this.gameState = null;
    this.legalActions = { actions: [], autoPassRecommended: false };
    this.pendingResolve = null;
    this.pendingReject = null;
    this.listeners = [];
  }

  private handleHostMessage(msg: P2PMessage): void {
    traceAdapter("Guest", "host-message", { type: msg.type });
    switch (msg.type) {
      case "game_setup": {
        this.assignedPlayerId = msg.assignedPlayerId;
        this.playerToken = msg.playerToken;
        void saveP2PSession(this.hostPeerId, {
          playerToken: msg.playerToken,
          playerId: msg.assignedPlayerId,
        });
        this.gameState = msg.state;
        this.legalActions = {
          actions: msg.legalActions,
          autoPassRecommended: msg.autoPassRecommended ?? false,
        };
        this.emit({ type: "playerIdentity", playerId: msg.assignedPlayerId, playerNames: msg.playerNames });
        this.settleGameSetup({ events: msg.events });
        break;
      }
      case "reconnect_ack": {
        this.assignedPlayerId = msg.assignedPlayerId;
        if (this.playerToken) {
          void saveP2PSession(this.hostPeerId, {
            playerToken: this.playerToken,
            playerId: msg.assignedPlayerId,
          });
        }
        this.gameState = msg.state;
        this.legalActions = {
          actions: msg.legalActions,
          autoPassRecommended: msg.autoPassRecommended,
        };
        this.emit({ type: "playerIdentity", playerId: msg.assignedPlayerId });
        this.emit({
          type: "stateChanged",
          state: msg.state,
          events: [],
          legalResult: this.legalActions,
        });
        // Resolve `initializeGame()` for the reconnect path too. Reconnecting
        // guests never receive `game_setup`; without this they would hang.
        // Post-reconnect `reconnect_ack` messages (guest briefly disconnects
        // a second time) are idempotent — the `gameSetupSettled` guard
        // prevents double-resolution.
        this.settleGameSetup({ events: [] });
        break;
      }
      case "reconnect_rejected": {
        this.terminated = true;
        this.emit({ type: "reconnectFailed", reason: msg.reason });
        this.emit({ type: "gameOver", winner: null, reason: msg.reason });
        break;
      }
      case "kick": {
        this.terminated = true;
        this.emit({ type: "gameOver", winner: null, reason: msg.reason });
        break;
      }
      case "host_left": {
        // Host explicitly quit — same terminal handling as `kick`: flip
        // `terminated` so the post-disconnect `handleHostDisconnect` path
        // skips `attemptReconnect`. Without this flag, the guest would
        // spin the reconnect backoff indefinitely against a Peer the
        // host already destroyed.
        this.terminated = true;
        this.emit({ type: "gameOver", winner: null, reason: msg.reason });
        break;
      }
      case "state_update": {
        this.gameState = msg.state;
        this.legalActions = {
          actions: msg.legalActions,
          autoPassRecommended: msg.autoPassRecommended ?? false,
        };
        if (this.pendingResolve) {
          this.pendingResolve({ events: msg.events });
          this.pendingResolve = null;
          this.pendingReject = null;
        } else {
          this.emit({
            type: "stateChanged",
            state: msg.state,
            events: msg.events,
            legalResult: this.legalActions,
          });
        }
        break;
      }
      case "action_rejected": {
        if (this.pendingReject) {
          this.pendingReject(
            new AdapterError("ACTION_REJECTED", msg.reason, true),
          );
          this.pendingResolve = null;
          this.pendingReject = null;
        }
        break;
      }
      case "player_disconnected": {
        this.emit({
          type: "opponentDisconnected",
          reason: `Player ${msg.playerId + 1} disconnected`,
        });
        break;
      }
      case "player_reconnected": {
        this.emit({ type: "playerReconnected", playerId: msg.playerId });
        break;
      }
      case "player_kicked": {
        this.emit({
          type: "playerKicked",
          playerId: msg.playerId,
          reason: msg.reason,
        });
        break;
      }
      case "player_conceded": {
        this.emit({
          type: "playerConceded",
          playerId: msg.playerId,
          reason: msg.reason,
        });
        break;
      }
      case "game_paused": {
        this.emit({ type: "gamePaused", reason: msg.reason });
        break;
      }
      case "game_resumed": {
        this.emit({ type: "gameResumed" });
        break;
      }
      case "lobby_progress": {
        this.emit({
          type: "lobbyProgress",
          joined: msg.joined,
          total: msg.total,
        });
        break;
      }
      case "seat_snapshot": {
        this.emit({
          type: "playerSlotsUpdated",
          slots: playerSlotsFromSeatView(msg.view),
        });
        break;
      }
      case "seat_mutate": {
        break;
      }
      default:
        break;
    }
  }

  /**
   * Resolve `initializeGame()` exactly once. Called from both `game_setup`
   * (fresh join) and `reconnect_ack` (rejoining mid-game) paths; later
   * messages are ignored so the promise stays stable if the guest briefly
   * disconnects again after `initializeGame()` returns.
   */
  private settleGameSetup(result: SubmitResult): void {
    if (this.gameSetupSettled) return;
    this.gameSetupSettled = true;
    this.gameSetupResolve(result);
  }

  private handleHostDisconnect(): void {
    this.session = null;
    // Suppress auto-reconnect in terminal states (kicked, explicitly rejected,
    // or adapter disposed). Without this, a kicked guest would spin the
    // backoff schedule (~30s total) hammering the host with a blacklisted
    // token.
    if (this.terminated) return;
    void this.attemptReconnect(0);
  }

  private async attemptReconnect(attemptIndex: number): Promise<void> {
    if (this.terminated) return;
    // After the escalating schedule, retry at a steady 60s cadence until
    // the user explicitly leaves. This is the "host-is-taking-a-while-
    // to-come-back" case (browser crash + reopen + tab-warmup can easily
    // take 2-3 minutes). `reconnectFailed` is NOT emitted here — the UI
    // keeps the reconnecting indicator up and the user decides when to
    // give up.
    const delay = attemptIndex < RECONNECT_BACKOFF_MS.length
      ? RECONNECT_BACKOFF_MS[attemptIndex]
      : RECONNECT_STEADY_STATE_MS;
    this.emit({ type: "reconnecting", attempt: attemptIndex + 1 });
    await new Promise((r) => setTimeout(r, delay));

    try {
      const conn = this.hostPeer.connect(this.hostPeerId);
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("connect timed out")), 10_000);
        conn.on("open", () => {
          clearTimeout(timeout);
          resolve();
        });
        conn.on("error", (err) => {
          clearTimeout(timeout);
          reject(err);
        });
      });
      this.attachSession(conn);
      if (this.playerToken) {
        this.session!.send({ type: "reconnect", playerToken: this.playerToken });
      }
    } catch (err) {
      console.warn(
        `[P2PGuest] reconnect attempt ${attemptIndex + 1} failed:`,
        err,
      );
      void this.attemptReconnect(attemptIndex + 1);
    }
  }
}
