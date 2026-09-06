/**
 * P2P Draft Tournament Host.
 *
 * Runs the authoritative DraftSession via draft-wasm and coordinates
 * an 8-player draft pod over PeerJS DataChannels. Follows the same
 * hub-and-spoke topology as `P2PHostAdapter` (game host), but speaks
 * the `DraftP2PMessage` protocol instead of `P2PMessage`.
 *
 * Requirements: P2P-01, P2P-03, P2P-05, P2P-06, P2P-07.
 */

import type Peer from "peerjs";
import type { DataConnection } from "peerjs";

import { DraftAdapter, EMPTY_DRAFT_POOL_GROUPS } from "./draft-adapter";
import type { DraftCardInstance, DraftPlayerView, MultiplayerSeatDescriptor, PairingView, PoolInput, SeatPublicView } from "./draft-adapter";
import type { DraftKind, DraftProcedure, PodPolicy, TournamentFormat } from "./draft-adapter";
import {
  createDraftPeerSession,
  type DraftPeerSession,
} from "../network/draftPeerSession";
import { parseRoomCode } from "../network/connection";
import {
  deckSubmissionFingerprint,
  DRAFT_PROTOCOL_VERSION,
  DraftPauseReason,
} from "../network/draftProtocol";
import type {
  DraftDeckPayload,
  DraftMatchBinding,
  DraftMatchDeckPayload,
  DraftMatchLaunch,
  DraftMatchSettlement,
  DraftP2PMessage,
  DraftReconnectRejectionKind,
} from "../network/draftProtocol";
import type { DeckCardCount, MatchConfig, MatchScore } from "./types";
import {
  saveDraftHostSession,
  clearDraftHostSession,
  type PersistedDraftHostSession,
} from "../services/draftPersistence";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  validateWorkspaceState,
  type DraftWorkspaceState,
} from "../components/draft/workspace/types";
import { reconcileWorkspaceState } from "../components/draft/workspace/workspacePlacement";
import { assertNever } from "../utils/assertNever";

function matchConfigForView(view: DraftPlayerView): MatchConfig {
  return view.match_config;
}
import {
  commandAcknowledgement,
  draftIntergameDigest,
  IntergameCommandController,
  matchesCommandAcknowledgement,
  type DraftIntergameCommand,
  type DraftIntergameCommandAck,
} from "../services/intergameCommandLedger";
import { assignAvatarForSeat } from "../services/playerAvatars";

/**
 * Prepare a host snapshot for the publicly retrievable P2P backup endpoint.
 *
 * IndexedDB keeps the full host snapshot and is the only durable location from
 * which a Chaos draft can resume. The HTTP backup is reachable by a derivable
 * host peer id, so it may retain the candidate intent but must never upload the
 * per-seat Chaos assignment matrix. The server repeats this redaction at its
 * trust boundary.
 */
function redactChaosAssignmentsFromPublicBackup(
  snapshot: PersistedDraftHostSession,
): PersistedDraftHostSession {
  if (snapshot.draftSessionJson === null) return snapshot;

  try {
    const session: unknown = JSON.parse(snapshot.draftSessionJson);
    if (!isJsonRecord(session) || !isJsonRecord(session.config)) return snapshot;
    if (!redactChaosAssignmentsFromSource(session.config.source)) return snapshot;

    return { ...snapshot, draftSessionJson: JSON.stringify(session) };
  } catch {
    // The server also redacts at the trust boundary. Keeping an unexpected
    // opaque payload intact preserves the existing best-effort backup behavior.
    return snapshot;
  }
}

function redactChaosAssignmentsFromSource(source: unknown): boolean {
  if (!isJsonRecord(source)) return false;

  let redacted = false;
  const redactSetLayout = (layout: unknown): void => {
    if (!isJsonRecord(layout) || !("candidate_codes" in layout) || !("assignments" in layout)) {
      return;
    }
    delete layout.assignments;
    redacted = true;
  };

  // `DraftSource`'s canonical serde output is adjacent-tagged. The older
  // externally tagged form is accepted too so a legacy transport shape cannot
  // bypass this client defense before the server applies its matching redaction.
  redactSetLayout(source.Set);
  if (source.type === "Set") redactSetLayout(source.data);
  redactSetLayout(source);
  return redacted;
}

function isJsonRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// ── Types ──────────────────────────────────────────────────────────────

/** Tracks Bo3 match state between games for a single pairing. */
interface Bo3MatchState {
  seatA: number;
  seatB: number;
  submittedA: boolean;
  submittedB: boolean;
  loserSeat: number | null;
  gameNumber: number;
  score: MatchScore;
  decks: Array<{ seat: number; main: DeckCardCount[]; sideboard: DeckCardCount[] }>;
}

export type DraftHostEvent =
  | { type: "seatJoined"; seatIndex: number; displayName: string }
  | { type: "seatReconnected"; seatIndex: number }
  | { type: "seatDisconnected"; seatIndex: number }
  | { type: "seatKicked"; seatIndex: number; reason: DraftPauseReason | string }
  | { type: "lobbyUpdate"; seats: SeatPublicView[]; joined: number; total: number }
  | { type: "lobbyFull" }
  | { type: "draftStarted"; view: DraftPlayerView }
  /** `cardInstanceIds` = the cards this seat drafted in this step, on both the normal and the draft-effect path. */
  | { type: "pickReceived"; seatIndex: number; cardInstanceIds: string[] }
  | { type: "roundComplete" }
  | { type: "draftComplete" }
  | { type: "deckSubmitted"; seatIndex: number }
  | { type: "allDecksSubmitted" }
  | { type: "draftPaused"; reason: DraftPauseReason }
  | { type: "draftResumed" }
  | { type: "error"; message: string }
  | { type: "viewUpdated"; view: DraftPlayerView }
  | { type: "pairingsGenerated"; round: number; pairings: PairingView[] }
  | { type: "matchStart"; launch: DraftMatchLaunch }
  | { type: "matchResultReceived"; matchId: string; winnerSeat: number | null }
  | { type: "roundAdvanced" }
  | { type: "timerExpired" }
  | {
      type: "bo3SideboardPrompt";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      loserSeat: number | null;
      timerMs: number;
    }
  | {
      type: "bo3ChoosePlayDraw";
      matchId: string;
      gameNumber: number;
      score: MatchScore;
      timerMs: number;
    }
  | { type: "bo3GameStart"; matchId: string; gameNumber: number; firstPlayerSeat: number }
  | { type: "bo3SideboardPromptSent"; matchId: string }
  | { type: "bo3BothSideboardsSubmitted"; matchId: string }
  | { type: "bo3GameStarted"; matchId: string; gameNumber: number }
  | { type: "bo3AuthorizedCommand"; command: DraftIntergameCommand; acknowledgement: DraftIntergameCommandAck };

type DraftHostEventListener = (event: DraftHostEvent) => void;

/** Default grace window for guest reconnect during draft. */
const DRAFT_GRACE_PERIOD_MS = 60_000;

/**
 * Booster size the lobby view advertises before any pack is opened. A POOL
 * property, not a `DraftProcedure` axis — the host has no session to read one
 * from, and the authoritative view replaces it the moment the draft starts.
 * Named so `cards_per_pack` and the `pack_sizes` array it fills cannot drift
 * apart. See `buildLobbyView`.
 */
const LOBBY_PLACEHOLDER_CARDS_PER_PACK = 14;

/** Arena-style escalating pick timer durations (ms). Index = pick number (0-based). */
const PICK_TIMER_DURATIONS_MS: readonly number[] = [
  75_000, 70_000, 65_000, 58_000, 52_000, 46_000,
  40_000, 34_000, 28_000, 23_000, 20_000, 18_000, 16_000, 15_000,
];

function pickTimerDurationMs(pickNumber: number): number {
  return PICK_TIMER_DURATIONS_MS[Math.min(pickNumber, PICK_TIMER_DURATIONS_MS.length - 1)];
}

/** A host seed controls both card collation and private Chaos assignments. */
function hostDraftSeed(): number {
  const values = new Uint32Array(1);
  crypto.getRandomValues(values);
  return values[0]!;
}

/**
 * `count` distinct random cards from `pack`, or the whole pack if it is shorter.
 * Distinctness is required: `apply_pick_inner` refuses a repeated id with
 * `DuplicatePickCardId`.
 *
 * D-02: random selection in TypeScript is a display-layer violation and is
 * PRE-EXISTING here. This generalises it from one card to N so a Commander pod
 * does not deadlock; it does not fix it.
 *
 * `count` comes from `DraftPlayerView.required_pick_count`, a REQUIRED field that
 * production always publishes. The shape guard below exists because
 * `slice(0, count)` with an `undefined` count returns the WHOLE pack: a future
 * hand-built stub view that omits the field would silently submit every card in
 * the pack instead of failing loudly. It gates on the SHAPE only — a legitimate
 * `0` (an emptied pack) still returns `[]`, which the engine refuses on its own
 * terms with `WrongPickCardCount`.
 */
function randomDistinctCards(pack: DraftCardInstance[], count: number): string[] {
  if (!Number.isInteger(count)) {
    throw new Error(`randomDistinctCards: required_pick_count must be an integer, got ${count}`);
  }
  const indices = pack.map((_, i) => i);
  for (let i = indices.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [indices[i], indices[j]] = [indices[j], indices[i]];
  }
  return indices.slice(0, count).map((i) => pack[i].instance_id);
}

interface PickOptions {
  acknowledge?: boolean;
  emit?: boolean;
  persist?: boolean;
  resolveBots?: boolean;
}

interface ExportedDraftSession {
  pools?: Array<Array<{ name: string }>>;
  submitted_decks?: Record<
    string,
    {
      seat: number;
      main_deck: string[];
      /**
       * CR 903.3: the seat's designated commander(s). Optional because the Rust
       * field carries `#[serde(default)]` and a session exported before the
       * plural-submission wire landed has none.
       */
      commanders?: string[];
    }
  >;
}

/**
 * The single constructor of a `DraftDeckPayload`.
 *
 * `commander` defaults to `[]`, so every existing caller is byte-identical in
 * behaviour and the four CR 905.1a kinds are untouched.
 */
function deckPayload(
  mainDeck: string[],
  sideboard: string[],
  commander: string[] = [],
): DraftDeckPayload {
  return { main_deck: mainDeck, sideboard, commander };
}

function deckCardCounts(cards: readonly string[]): DeckCardCount[] {
  const counts = new Map<string, number>();
  for (const card of cards) counts.set(card, (counts.get(card) ?? 0) + 1);
  return [...counts].map(([name, count]) => ({ name, count }));
}

function deckSubmission(deck: DraftDeckPayload): { main: DeckCardCount[]; sideboard: DeckCardCount[] } {
  return {
    main: deckCardCounts(deck.main_deck),
    sideboard: deckCardCounts(deck.sideboard),
  };
}

function workspaceStatesEqual(
  left: DraftWorkspaceState,
  right: DraftWorkspaceState,
): boolean {
  const leftPlacements = Object.entries(left.placements);
  const rightPlacements = Object.entries(right.placements);
  return left.schemaVersion === right.schemaVersion
    && leftPlacements.length === rightPlacements.length
    && leftPlacements.every(([instanceId, placement]) => {
      const candidate = right.placements[instanceId];
      return candidate !== undefined
        && candidate.zone === placement.zone
        && candidate.row === placement.row
        && candidate.column === placement.column
        && candidate.order === placement.order;
    })
    && left.virtualBasics.length === right.virtualBasics.length
    && left.virtualBasics.every((basic, index) => {
      const candidate = right.virtualBasics[index];
      return candidate.instanceId === basic.instanceId && candidate.name === basic.name;
    });
}

/** Sideboarding may move cards between zones, but cannot change a player's pool. */
function preservesDeckPool(
  deck: DraftDeckPayload,
  main: readonly DeckCardCount[],
  sideboard: readonly DeckCardCount[],
): boolean {
  const submitted = new Map<string, number>();
  for (const card of [...main, ...sideboard]) {
    if (!Number.isSafeInteger(card.count) || card.count < 0) return false;
    submitted.set(card.name, (submitted.get(card.name) ?? 0) + card.count);
  }
  const original = new Map<string, number>();
  for (const name of [...deck.main_deck, ...deck.sideboard]) {
    original.set(name, (original.get(name) ?? 0) + 1);
  }
  return submitted.size === original.size
    && [...submitted].every(([name, count]) => original.get(name) === count);
}

function hashStringToSeed(value: string): number {
  let hash = 5381;
  for (let i = 0; i < value.length; i++) {
    hash = ((hash * 33) ^ value.charCodeAt(i)) | 0;
  }
  return hash >>> 0;
}

function sideboardFromPool(
  session: ExportedDraftSession,
  seat: number,
  mainDeck: string[],
): string[] {
  const counts = new Map<string, number>();
  for (const card of session.pools?.[seat] ?? []) {
    counts.set(card.name, (counts.get(card.name) ?? 0) + 1);
  }
  for (const name of mainDeck) {
    const count = counts.get(name);
    if (count === undefined) continue;
    if (count <= 1) counts.delete(name);
    else counts.set(name, count - 1);
  }
  return [...counts.entries()].flatMap(([name, count]) =>
    Array<string>(count).fill(name),
  );
}

// ── P2PDraftHost ───────────────────────────────────────────────────────

export class P2PDraftHost {
  private adapter = new DraftAdapter();

  /**
   * The engine-owned per-kind axes, fetched once in `initialize()`. Snapshotted
   * rather than read live, and correctly so: `procedure()` is a pure function
   * of the kind, and the kind is immutable for this host's lifetime, so a live
   * read could not differ.
   */
  private procedure: DraftProcedure | null = null;
  private listeners: DraftHostEventListener[] = [];

  private guestSessions = new Map<number, DraftPeerSession>();
  private seatTokens = new Map<number, string>();
  private seatNames = new Map<number, string>();
  private kickedTokens = new Set<string>();
  /**
   * The absolute end of a guest's current reconnect episode. It survives a
   * successful tentative reconnect so a later drop cannot grant a new window.
   */
  private reconnectDeadlines = new Map<number, number>();
  private disconnectedSeats = new Map<
    number,
    { deadlineAt: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  private expiredDisconnectedSeats = new Set<number>();
  private picksThisRound = new Set<number>();

  private draftStarted = false;
  private draftCode = "";
  private draftSeed: number | null = null;
  private activePodSize: number;
  private hostConnectionUnsub: (() => void) | null = null;
  /** Explicit host intent, independent from transient disconnected seats. */
  private manualPause = false;
  private paused = false;
  private timerInterval: ReturnType<typeof setInterval> | null = null;
  private timerRemainingMs = 0;
  private timerEndAt = 0;
  private timerContext: "pick" | "sideboard" | "playdraw" | null = null;
  private timerTargetMatchId: string | null = null;
  private frozenTimer: { context: "pick" | "sideboard" | "playdraw"; remainingMs: number; matchId: string | null } | null = null;
  private bo3State = new Map<string, Bo3MatchState>();
  /** Registered decks are captured at match launch and become the first
   * authority-owned default for an unchanged sideboard submission. */
  private matchDecks = new Map<string, Map<number, DraftDeckPayload>>();
  /** Full launch records let the host mint a timeout command under the same
   * immutable launch digest the participant originally received. */
  private matchLaunches = new Map<string, Map<number, DraftMatchLaunch>>();
  /** Private issuer for the durable Pending → Authorized → Executing → Receipted ledger. */
  private intergameCommands = new IntergameCommandController();
  private launchDigests = new Map<string, Map<number, string>>();
  /** Durable pod-issued authority records, keyed by match ID. */
  private matchBindings = new Map<string, DraftMatchBinding>();
  /** Write-ahead settlement records; retained until the reducer accepts them. */
  private settlementOutbox = new Map<string, DraftMatchSettlement>();
  /** Immutable receipt per match makes retries idempotent. */
  private settlementReceipts = new Map<string, { receiptId: string; revision: number }>();
  /** Submission id → immutable payload receipt. Persisted before its acknowledgement. */
  private deckSubmissionReceipts = new Map<string, { seat: number; payloadFingerprint: string }>();
  /** Prevent duplicate local visibility while a connected guest retries its receipt. */
  private publishedDeckSubmissions = new Set<string>();

  // Server backup upload state (D-08)
  private backupEndpoint: string | null = null;
  private picksSinceLastBackup = 0;
  private persistQueue = Promise.resolve();
  /** Failed post-reducer snapshot retried verbatim before any later state. */
  private pendingDraftSnapshot: PersistedDraftHostSession | null = null;
  /** Authoritative guest actions cannot race a snapshot/export boundary. */
  private mutationQueue = Promise.resolve();
  private pendingMutations = 0;
  /** Admissions mutate token state before their durability fence, so serialize them. */
  private admissionQueue = Promise.resolve();
  private perSeatWorkspaceSnapshots = new Map<number, DraftWorkspaceState>();
  private persistenceClosed = false;
  private static readonly BACKUP_INTERVAL_PICKS = 5;

  constructor(
    private readonly hostPeer: Peer,
    private readonly onGuestConnected: (
      handler: (conn: DataConnection) => void,
    ) => () => void,
    private readonly poolInput: PoolInput,
    private readonly kind: Exclude<DraftKind, "Quick">,
    private readonly podSize: number,
    private readonly hostDisplayName: string,
    private readonly tournamentFormat: TournamentFormat,
    private readonly podPolicy: PodPolicy,
    private readonly gracePeriodMs: number = DRAFT_GRACE_PERIOD_MS,
    private readonly persistenceId?: string,
    private readonly roomCode?: string,
    backupEndpoint?: string,
  ) {
    if (persistenceId && (!roomCode || parseRoomCode(roomCode) !== roomCode)) {
      throw new Error("Persistent draft hosts require a canonical room code");
    }
    // Host is always seat 0
    this.seatNames.set(0, hostDisplayName);
    this.activePodSize = podSize;
    this.backupEndpoint = backupEndpoint ?? null;
  }

  // ── Event emitter ──────────────────────────────────────────────────

  onEvent(listener: DraftHostEventListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  private emit(event: DraftHostEvent): void {
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  /**
   * One host owns one reducer and one durable timeline.  Guest DataChannels
   * may deliver concurrently, but their authoritative operations may not
   * interleave between reducer application, immutable snapshot capture, and
   * the visibility fence that follows it.
   */
  private enqueueAuthoritativeMutation<T>(operation: () => Promise<T>): Promise<T> {
    // Start the first mutation synchronously.  Timer expiry is observable at
    // the same tick it reaches zero, while later DataChannel messages still
    // serialize behind its durable fence.
    if (this.pendingMutations === 0) {
      this.pendingMutations++;
      let task: Promise<T>;
      try {
        task = operation();
      } catch (error) {
        task = Promise.reject(error);
      }
      this.mutationQueue = task.then(() => undefined, () => undefined).finally(() => {
        this.pendingMutations--;
      });
      return task;
    }
    this.pendingMutations++;
    const task = this.mutationQueue.then(operation);
    this.mutationQueue = task.then(() => undefined, () => undefined).finally(() => {
      this.pendingMutations--;
    });
    return task;
  }

  /** Reports a failed detached mutation instead of leaking an unhandled rejection. */
  private reportDetachedMutationFailure(label: string, error: unknown): void {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`[P2PDraftHost] ${label} failed:`, error);
    this.emit({ type: "error", message: `${label} did not commit: ${message}` });
  }

  private runDetachedMutation(label: string, operation: () => Promise<unknown>): void {
    void this.enqueueAuthoritativeMutation(operation).catch((error: unknown) => {
      this.reportDetachedMutationFailure(label, error);
    });
  }

  // ── Initialization ─────────────────────────────────────────────────

  async initialize(): Promise<void> {
    // FIRST, before `onGuestConnected` registers the handler behind two of
    // `buildLobbyView()`'s three callers (the welcome view and the reconnect
    // ack). That ordering is the binding-time guarantee for those two paths.
    // The third caller, the public `getHostView()`, is NOT covered by this
    // ordering — it is covered by `buildLobbyView`'s throw-on-null, which is
    // why that rule is load-bearing rather than defensive.
    this.procedure = await this.adapter.draftProcedure(this.kind, this.tournamentFormat);

    this.hostConnectionUnsub = this.onGuestConnected((conn) => {
      this.handleNewConnection(conn);
    });
    this.syncLobbyToGuests();
    this.persistSession();
  }

  // ── Connection handling ────────────────────────────────────────────

  private handleNewConnection(conn: DataConnection): void {
    const session = createDraftPeerSession(conn, {
      onSessionEnd: () => {
        for (const [seat, s] of this.guestSessions.entries()) {
          if (s === session) {
            this.handleGuestDisconnect(seat);
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

      if (msg.type !== "draft_join" && msg.type !== "draft_reconnect") {
        void this.rejectAndClose(
          session,
          "ProtocolMismatch",
          "Expected draft_join or draft_reconnect as first message",
          "Protocol violation",
        ).catch((error: unknown) => this.reportDetachedMutationFailure("first-contact rejection", error));
      } else if (msg.draftProtocolVersion !== DRAFT_PROTOCOL_VERSION) {
        // First-contact versioning is a hard gate. Do not allocate a seat,
        // consume reconnect grace, or attach the session before it passes.
        void this.rejectAndClose(
          session,
          "ProtocolMismatch",
          `Draft protocol mismatch: host v${DRAFT_PROTOCOL_VERSION}, client v${String(msg.draftProtocolVersion)}. Refresh both windows.`,
          "Draft protocol mismatch",
        ).catch((error: unknown) => this.reportDetachedMutationFailure("first-contact rejection", error));
      } else if (msg.type === "draft_join") {
        this.runDetachedMutation("guest admission", () => this.handleNewGuest(session, msg.displayName));
      } else {
        this.runDetachedMutation("guest reconnect", () => this.handleReconnect(session, msg.draftToken));
      }
    });
  }

  /** Flush a typed rejection before closing its DataConnection. */
  private async rejectAndClose(
    session: DraftPeerSession,
    kind: DraftReconnectRejectionKind,
    reason: string,
    closeReason: string,
  ): Promise<void> {
    await session.send({
      type: "draft_reconnect_rejected",
      kind,
      reason,
    });
    session.close(closeReason);
  }

  private async handleNewGuest(session: DraftPeerSession, displayName: string): Promise<void> {
    // This is intentionally outside `admissionQueue`: a connection can close
    // while waiting behind another guest, before its admission transaction
    // starts. In that case it must never allocate a provisional token.
    let firstContactLive = true;
    const stopWatchingFirstContact = session.onDisconnect(() => {
      firstContactLive = false;
    });
    try {
      const admission = this.admissionQueue.then(() =>
        this.admitNewGuest(session, displayName, () => firstContactLive),
      );
      this.admissionQueue = admission.catch(() => {});
      await admission;
    } finally {
      stopWatchingFirstContact();
    }
  }

  private async admitNewGuest(
    session: DraftPeerSession,
    displayName: string,
    isFirstContactLive: () => boolean,
  ): Promise<void> {
    if (!isFirstContactLive()) return;
    if (this.draftStarted) {
      try {
        await session.send({ type: "draft_kicked", reason: "Draft already in progress" });
      } finally {
        session.close("Draft in progress");
      }
      return;
    }

    const seat = this.firstOpenSeat();
    if (seat === null) {
      try {
        await session.send({ type: "draft_kicked", reason: "Pod is full" });
      } finally {
        session.close("Pod full");
      }
      return;
    }

    const token = crypto.randomUUID();
    this.seatTokens.set(seat, token);
    this.seatNames.set(seat, displayName);

    try {
      // The guest receives the capability only after this host can recover the
      // matching token and seat. Publishing either welcome or lobby state
      // first would leave a reloaded host unable to honour that capability.
      await this.persistSessionStrict();
    } catch (err) {
      this.seatTokens.delete(seat);
      this.seatNames.delete(seat);
      console.warn("[P2PDraftHost] guest admission persistence failed:", err);
      session.close("Guest admission persistence failed");
      return;
    }

    if (!isFirstContactLive()) {
      await this.rollbackDisconnectedAdmission(seat);
      return;
    }

    this.guestSessions.set(seat, session);
    // A synchronous transport close can occur during registration. The
    // first-contact watcher remains active until this transaction returns, so
    // roll back before installing a guest handler or announcing the seat.
    if (!isFirstContactLive()) {
      await this.rollbackDisconnectedAdmission(seat, session);
      return;
    }
    session.onMessage((msg) => {
      this.runDetachedMutation("guest message", () => this.handleGuestMessage(seat, msg, session));
    });

    // Send welcome with empty view (draft hasn't started)
    const emptyView: DraftPlayerView = this.buildLobbyView();

    await session.send({
      type: "draft_welcome",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: token,
      seatIndex: seat,
      view: emptyView,
      draftCode: this.draftCode || "pending",
      workspaceState: null,
    });

    // `send` yields for wire encoding. A close during that await runs the
    // registered session-end handler, which removes the seat and persists the
    // disconnect; do not announce a guest that no longer exists.
    if (this.guestSessions.get(seat) !== session) return;

    this.emit({ type: "seatJoined", seatIndex: seat, displayName });
    this.syncLobbyToGuests();

    if (this.firstOpenSeat() === null) {
      this.emit({ type: "lobbyFull" });
    }
  }

  /** Removes a post-fence provisional admission and commits that removal. */
  private async rollbackDisconnectedAdmission(seat: number, session?: DraftPeerSession): Promise<void> {
    if (session && this.guestSessions.get(seat) === session) {
      this.guestSessions.delete(seat);
    }
    this.seatTokens.delete(seat);
    this.seatNames.delete(seat);
    try {
      // The admission snapshot already committed this provisional token, so
      // make the rollback durable before another admission can begin.
      await this.persistSessionStrict();
    } catch (err) {
      console.warn("[P2PDraftHost] disconnected admission rollback failed:", err);
    }
  }

  private async handleReconnect(session: DraftPeerSession, draftToken: string): Promise<void> {
    if (this.kickedTokens.has(draftToken)) {
      await this.rejectAndClose(session, "Kicked", "Player kicked", "Kicked");
      return;
    }

    let seat: number | null = null;
    for (const [s, token] of this.seatTokens) {
      if (token === draftToken) {
        seat = s;
        break;
      }
    }

    if (seat === null) {
      await this.rejectAndClose(session, "UnknownToken", "Unknown token", "Unknown token");
      return;
    }

    if (!this.disconnectedSeats.has(seat)) {
      await this.rejectAndClose(
        session,
        "NoReconnectWindow",
        "No grace window active for this seat",
        "Not in grace",
      );
      return;
    }

    const reconnectSeat = seat;
    let reconnectDeadlineAt: number | null = null;
    let live = true;
    const stopWatching = session.onDisconnect?.(() => { live = false; }) ?? (() => {});
    try {
      // Keep the old grace record and do not install an action handler while
      // the recovered connection is merely tentative. A close between the
      // engine update and durable save is rolled back below rather than
      // producing a connected-looking, unrecoverable seat.
      if (this.draftStarted) await this.adapter.setSeatConnected(reconnectSeat, true);
      await this.persistSessionStrict();
      if (!live) {
        if (this.draftStarted) {
          await this.adapter.setSeatConnected(reconnectSeat, false);
          await this.persistSessionStrict();
        }
        return;
      }
      const grace = this.disconnectedSeats.get(reconnectSeat);
      if (!grace) {
        if (this.draftStarted) {
          await this.adapter.setSeatConnected(reconnectSeat, false);
          await this.persistSessionStrict();
        }
        await this.rejectAndClose(session, "NoReconnectWindow", "Reconnect window expired", "Reconnect window expired");
        return;
      }
      reconnectDeadlineAt = grace.deadlineAt;
      this.clearReconnectGrace(reconnectSeat);
      this.guestSessions.set(reconnectSeat, session);
      session.onMessage((msg) => {
        this.runDetachedMutation("guest message", () => this.handleGuestMessage(reconnectSeat, msg, session));
      });

      // The prior fence makes the engine's connected bitmap recoverable while
      // this reconnect is tentative. This fence records the completed handoff
      // while retaining its absolute deadline, so a later recovery or drop
      // can use only the remaining grace window.
      await this.persistSessionStrict();

      const view = this.draftStarted
        ? await this.adapter.getViewForSeat(reconnectSeat)
        : this.buildLobbyView();
      // Fetching a view yields to the transport. A close or grace expiry that
      // occurs meanwhile must not publish a recovered workspace or ack.
      if (!live || this.guestSessions.get(reconnectSeat) !== session) {
        throw new Error("Reconnect session ended before acknowledgement");
      }
      const reconciliation = this.reconcileRetainedWorkspace(reconnectSeat, view.pool);
      if (reconciliation.changed) await this.persistSessionStrict();
      await session.send({
        type: "draft_reconnect_ack",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        seatIndex: reconnectSeat,
        view,
        draftCode: this.draftCode,
        workspaceState: reconciliation.workspaceState,
      });
      if (this.draftStarted) await this.broadcastViews();
      if (view.status === "MatchInProgress") await this.dispatchMatchLaunchesForSeat(view, reconnectSeat);
    } catch (err) {
      console.error("[P2PDraftHost] reconnect view failed:", err);
      if (this.guestSessions.get(reconnectSeat) === session) {
        this.guestSessions.delete(reconnectSeat);
      }
      if (this.draftStarted) {
        try { await this.adapter.setSeatConnected(reconnectSeat, false); } catch { /* best-effort rollback */ }
      }
      if (!this.disconnectedSeats.has(reconnectSeat) && reconnectDeadlineAt !== null) {
        if (reconnectDeadlineAt > Date.now()) {
          this.scheduleReconnectGrace(reconnectSeat, reconnectDeadlineAt);
        } else {
          this.reconnectDeadlines.delete(reconnectSeat);
          this.expiredDisconnectedSeats.add(reconnectSeat);
        }
      }
      try {
        await this.persistSessionStrict();
      } catch (persistError) {
        this.reportDetachedMutationFailure("reconnect rollback", persistError);
      }
      session.close("Reconnect failed");
      return;
    } finally {
      stopWatching();
    }

    for (const [otherSeat, otherSession] of this.guestSessions) {
      if (otherSeat === reconnectSeat) continue;
      otherSession.send({
        type: "draft_lobby_update",
        seats: this.buildSeatPublicViews(),
        joined: this.occupiedSeatCount(),
        total: this.podSize,
      });
    }

    if (!this.guestSessions.has(reconnectSeat)) return;
    this.emit({ type: "seatReconnected", seatIndex: reconnectSeat });

    // Resume if no other seats disconnected
    this.reconcileEffectivePause();
  }

  // ── Message handling ───────────────────────────────────────────────

  private async handleGuestMessage(
    seat: number,
    msg: DraftP2PMessage,
    originatingSession = this.guestSessions.get(seat),
  ): Promise<void> {
    // A queued message from a replaced DataChannel must never act on the seat
    // its successor now owns.
    if (originatingSession && this.guestSessions.get(seat) !== originatingSession) return;
    switch (msg.type) {
      case "draft_leave": {
        await this.handleGuestLeave(seat, msg.draftToken, originatingSession);
        break;
      }
      case "draft_pick": {
        if (!this.canGuestPick(seat)) return;
        await this.handlePick(seat, msg.cardInstanceIds);
        break;
      }
      case "draft_pick_with_draft_effect": {
        if (!this.canGuestPick(seat)) return;
        await this.handlePickWithDraftEffect(
          seat,
          msg.effectCardInstanceId,
          msg.cardInstanceIds,
        );
        break;
      }
      case "draft_submit_deck": {
        if (!this.draftStarted) {
          this.guestSessions.get(seat)?.send({
            type: "draft_error",
            reason: "Draft not started",
            submissionId: msg.submissionId,
            submissionDisposition: "Rejected",
          });
          return;
        }
        await this.handleDeckSubmission(seat, msg.mainDeck, msg.commanders, msg.submissionId);
        break;
      }
      case "draft_workspace_update": {
        try {
          await this.applyWorkspaceUpdate(seat, msg.workspaceState, originatingSession);
        } catch (err) {
          if (this.guestSessions.get(seat) !== originatingSession) return;
          const reason = err instanceof Error ? err.message : String(err);
          const errorSend = originatingSession?.send({ type: "draft_error", reason });
          if (errorSend) void errorSend.catch(() => undefined);
        }
        break;
      }
      case "draft_match_result": {
        // A raw match ID is forgeable by any connected seat. Keep the legacy
        // shape decodable for an in-flight old client, but never settle it.
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound match result" });
        break;
      }
      case "draft_match_settlement": {
        await this.acceptMatchSettlement(seat, msg.settlement);
        break;
      }
      case "draft_bo3_between_games": {
        await this.handleGuestBetweenGames(seat, msg);
        break;
      }
      case "draft_request_advance": {
        // T-57-07: ignore from guests — only host UI triggers round advance
        break;
      }
      case "draft_bo3_sideboard_submit": {
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound intergame command" });
        break;
      }
      case "draft_bo3_play_draw_choice": {
        this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unbound intergame command" });
        break;
      }
      case "draft_bo3_intergame_command": {
        await this.holdIntergameCommand(seat, msg.command);
        break;
      }
      case "draft_bo3_intergame_receipt": {
        await this.receiptIntergameCommand(seat, msg.acknowledgement, msg.receiptId);
        break;
      }
      default:
        break;
    }
  }

  // ── Draft operations ───────────────────────────────────────────────

  /**
   * Start the draft. Called by the host UI once the pod is full
   * (or the host decides to start with fewer players).
   */
  async startDraft(botFillEmptySeats = true): Promise<void> {
    return this.enqueueAuthoritativeMutation(() => this.startDraftInner(botFillEmptySeats));
  }

  private async startDraftInner(botFillEmptySeats: boolean): Promise<void> {
    if (this.draftStarted) return;
    if (this.disconnectedSeats.size > 0) {
      throw new Error("Cannot start draft while a player is reconnecting");
    }

    const seed = hostDraftSeed();
    this.draftSeed = seed;
    const draftCode = `draft-${seed.toString(16).padStart(8, "0")}`;
    const seats: MultiplayerSeatDescriptor[] = [];
    for (let i = 0; i < this.podSize; i++) {
      const displayName = this.seatNames.get(i);
      if (displayName) {
        seats.push({
          type: "Human",
          player_id: i,
          display_name: displayName,
        });
      } else if (botFillEmptySeats) {
        seats.push({ type: "Bot", name: this.botNameForSeat(i, seed) });
      }
    }
    await this.adapter.createMultiplayerDraft(
      this.poolInput,
      seats,
      this.kind,
      seed,
      draftCode,
      this.tournamentFormat,
      this.podPolicy,
    );

    this.draftStarted = true;
    this.draftCode = draftCode;
    this.activePodSize = seats.length;
    this.picksThisRound.clear();
    const startView = await this.adapter.getViewForSeat(0);
    if (startView.status === "Drafting") {
      await this.resolveBotPicks({ emit: false, persist: false });
    }

    // No client may observe the started draft until the recoverable snapshot
    // exists.  A refresh between a state update and this fence was the root
    // cause of the original missing-pod incident.
    await this.persistSessionStrict();

    // Send each guest their filtered view
    for (const [seat, session] of this.guestSessions) {
      try {
        const view = await this.adapter.getViewForSeat(seat);
        session.send({ type: "draft_state_update", view });
      } catch (err) {
        console.error(`[P2PDraftHost] Failed to send start view to seat ${seat}:`, err);
      }
    }

    const freshHostView = await this.adapter.getViewForSeat(0);
    this.emit({ type: "draftStarted", view: freshHostView });
    if (freshHostView.status === "Drafting") {
      this.startPickTimer(0);
    }
  }

  /**
   * Host submits their own pick (seat 0).
   */
  async submitHostPick(cardInstanceIds: string[]): Promise<DraftPlayerView> {
    return this.enqueueAuthoritativeMutation(() => this.handlePick(0, cardInstanceIds));
  }

  /** Host submits an effect pick for seat 0. */
  async submitHostPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    return this.enqueueAuthoritativeMutation(() =>
      this.handlePickWithDraftEffect(0, effectCardInstanceId, cardInstanceIds));
  }

  /**
   * Host submits their own deck (seat 0).
   */
  async submitHostDeck(mainDeck: string[], commanders: string[]): Promise<DraftPlayerView> {
    return this.enqueueAuthoritativeMutation(() => {
      if (!this.draftStarted) throw new Error("Draft not started");
      // CR 903.3: the designation is part of the payload's identity, so it
      // joins the fingerprint. Matching on `mainDeck` alone would let a
      // resubmit that changes only the commander reuse the prior receipt and
      // resolve straight to the recovered-receipt branch, never reaching the
      // reducer with the new designation.
      const payloadFingerprint = this.deckPayloadFingerprint(mainDeck, commanders);
      const priorSubmission = [...this.deckSubmissionReceipts.entries()].find(
        ([, receipt]) => receipt.seat === 0 && receipt.payloadFingerprint === payloadFingerprint,
      )?.[0];
      return this.handleDeckSubmission(0, mainDeck, commanders, priorSubmission ?? crypto.randomUUID());
    });
  }

  updateHostWorkspace(state: DraftWorkspaceState): Promise<void> {
    return this.enqueueAuthoritativeMutation(() => this.applyWorkspaceUpdate(0, state));
  }

  getHostWorkspaceState(): DraftWorkspaceState | null {
    return this.perSeatWorkspaceSnapshots.get(0) ?? null;
  }

  private async applyWorkspaceUpdate(
    seat: number,
    state: DraftWorkspaceState,
    sourceSession?: DraftPeerSession,
  ): Promise<void> {
    const view = await this.adapter.getViewForSeat(seat);
    // The adapter view await is a transport boundary: a superseded guest may
    // not mutate its replacement's retained state after it returns.
    if (sourceSession && this.guestSessions.get(seat) !== sourceSession) return;
    const validated = validateWorkspaceState(state, {
      maxPlacementCount: view.pool.length + MAX_MATERIALIZED_VIRTUAL_BASICS,
    });
    if ("error" in validated) throw new Error(validated.error);
    const reconciled = reconcileWorkspaceState(validated, view.pool);
    this.perSeatWorkspaceSnapshots.set(seat, reconciled);
    await this.persistSessionStrict();
  }

  private reconcileRetainedWorkspace(
    seat: number,
    pool: DraftPlayerView["pool"],
  ): { workspaceState: DraftWorkspaceState | null; changed: boolean } {
    const state = this.perSeatWorkspaceSnapshots.get(seat);
    if (!state) return { workspaceState: null, changed: false };
    const validated = validateWorkspaceState(state);
    if ("error" in validated) {
      this.perSeatWorkspaceSnapshots.delete(seat);
      return { workspaceState: null, changed: true };
    }
    const reconciled = reconcileWorkspaceState(validated, pool);
    if (!workspaceStatesEqual(reconciled, state)) {
      this.perSeatWorkspaceSnapshots.set(seat, reconciled);
      return { workspaceState: reconciled, changed: true };
    }
    return { workspaceState: reconciled, changed: false };
  }

  private assertPickAllowed(): void {
    if (!this.draftStarted) throw new Error("Draft not started");
    if (this.paused) throw new Error("Draft is paused");
  }

  private canGuestPick(seat: number): boolean {
    try {
      this.assertPickAllowed();
      return true;
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      this.guestSessions.get(seat)?.send({ type: "draft_error", reason });
      return false;
    }
  }

  private async handlePick(
    seat: number,
    cardInstanceIds: string[],
    resolveBots = true,
  ): Promise<DraftPlayerView> {
    this.assertPickAllowed();
    return this.applyPick(seat, cardInstanceIds, {
      acknowledge: true,
      emit: true,
      persist: true,
      resolveBots,
    });
  }

  private async handlePickWithDraftEffect(
    seat: number,
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    this.assertPickAllowed();
    return this.applyPick(
      seat,
      // The cards this seat drafted — NOT the effect card. This positional
      // slot carries one meaning on both paths; the `submitPick` override
      // below is what makes the effect path's submission different.
      cardInstanceIds,
      {
        acknowledge: true,
        emit: true,
        persist: true,
        resolveBots: true,
      },
      () => this.adapter.submitPickWithDraftEffectForSeat(
        seat,
        effectCardInstanceId,
        cardInstanceIds,
      ),
    );
  }

  private async applyPick(
    seat: number,
    cardInstanceIds: string[],
    options: PickOptions,
    // One whole CR 903.13b pick step: exactly `view.required_pick_count` ids,
    // the count `pick_pass::apply_pick_inner` enforces. On the draft-effect
    // path the caller overrides `submitPick`; this parameter still names the
    // cards the seat drafted, which is what `pickReceived` reports.
    submitPick = () => this.adapter.submitPickForSeat(seat, cardInstanceIds),
  ): Promise<DraftPlayerView> {
    try {
      const view = await submitPick();
      this.picksThisRound.add(seat);

      // A pick acknowledgement is externally authoritative: never publish it
      // until a host reload can restore the reducer result.  Bot sweeps during
      // initial start intentionally defer to StartDraft's one encompassing
      // fence (`persist: false`).
      if (options.persist) {
        await this.persistSessionStrict();
      }

      // Send pick acknowledgement to the picking player
      const session = this.guestSessions.get(seat);
      if (options.acknowledge && session) {
        session.send({ type: "draft_pick_ack", view });
      }

      if (options.emit) {
        this.emit({ type: "pickReceived", seatIndex: seat, cardInstanceIds });
      }
      if (options.resolveBots && !this.isBotSeat(seat)) {
        await this.resolveBotPicks({ emit: true, persist: true });
        await this.broadcastViews();
      }

      // Check if all picks for this round are in
      const allPicked = await this.adapter.allPicksSubmitted();
      if (allPicked) {
        this.picksThisRound.clear();
        this.clearActiveTimer();
        this.emit({ type: "roundComplete" });

        // Broadcast updated views to all players
        await this.broadcastViews();

        // Check if draft is complete (deckbuilding)
        const hostView = await this.adapter.getViewForSeat(0);
        if (hostView.status === "Deckbuilding") {
          this.clearActiveTimer();
          this.emit({ type: "draftComplete" });
        } else if (hostView.status === "Drafting") {
          this.startPickTimer(hostView.pick_number);
        }
      }

      // Return the host's updated view if this was the host's pick
      if (seat === 0) {
        return await this.adapter.getViewForSeat(0);
      }
      return await this.adapter.getViewForSeat(0);
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      const session = this.guestSessions.get(seat);
      if (session) {
        session.send({ type: "draft_error", reason });
      }
      throw err;
    }
  }

  private async handleDeckSubmission(
    seat: number,
    mainDeck: string[],
    commanders: string[],
    submissionId: string,
  ): Promise<DraftPlayerView> {
    let submissionAccepted = false;
    let receiptDurable = false;
    try {
      const payloadFingerprint = this.deckPayloadFingerprint(mainDeck, commanders);
      const previous = this.deckSubmissionReceipts.get(submissionId);
      let view: DraftPlayerView;
      if (previous) {
        if (previous.seat !== seat || previous.payloadFingerprint !== payloadFingerprint) {
          throw new Error("Deck submission id does not match its original payload");
        }
        submissionAccepted = true;
        view = await this.adapter.getViewForSeat(seat);
        // This is also the retry path after an IDB failure: it flushes the
        // immutable pending snapshot before issuing a receipt, without ever
        // submitting the deck to the reducer again.
        await this.persistSessionStrict();
        receiptDurable = true;
      } else {
        view = await this.adapter.submitDeckForSeat(seat, mainDeck, commanders);
        // Record before saving the post-reducer snapshot. A retry after a host
        // reload therefore sees the same result and cannot feed the reducer a
        // second submission.
        this.deckSubmissionReceipts.set(submissionId, { seat, payloadFingerprint });
        submissionAccepted = true;
        await this.persistSessionStrict();
        receiptDurable = true;
      }

      try {
        await this.sendDeckSubmissionAck(seat, submissionId, view);
      } catch (error) {
        // The receipt is already durable. Continue host progression; a later
        // retry will receive the same acknowledgement without re-reducing.
        console.warn("[P2PDraftHost] deck submission acknowledgement failed:", error);
      }
      await this.publishAcceptedDeckSubmission(seat, submissionId, previous !== undefined);

      return seat === 0 ? view : await this.adapter.getViewForSeat(0);
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      const session = this.guestSessions.get(seat);
      if (session && !submissionAccepted) {
        session.send({ type: "draft_error", reason, submissionId, submissionDisposition: "Rejected" });
      } else if (session && !receiptDurable) {
        session.send({ type: "draft_error", reason, submissionId, submissionDisposition: "Retryable" });
      }
      throw err;
    }
  }

  /** Runs delayed downstream deck visibility after a durable receipt retry. */
  /**
   * CR 903.3: a deck submission's identity is its main deck AND its commander
   * designation. Both call sites must agree, or a receipt lookup and a receipt
   * comparison can disagree about whether two submissions are the same.
   */
  private deckPayloadFingerprint(mainDeck: string[], commanders: string[]): string {
    return `${deckSubmissionFingerprint(mainDeck)}|${deckSubmissionFingerprint(commanders)}`;
  }

  private async publishAcceptedDeckSubmission(
    seat: number,
    submissionId: string,
    recovered: boolean,
  ): Promise<void> {
    if (this.publishedDeckSubmissions.has(submissionId)) return;
    const hostView = await this.adapter.getViewForSeat(0);
    // A terminal status means the downstream work already ran — but only for a
    // RECOVERED receipt. `publishedDeckSubmissions` is in-memory, so after a
    // host reload it cannot answer this and the status is the only signal
    // left. It must not answer for a freshly accepted submission: a Commander
    // Draft pod is PostDraftPlay::CompleteImmediately, so its LAST deck
    // submission lands on `Complete` on first visit, and treating that as
    // "already replayed" would swallow the pod's completion for every guest.
    if (recovered && (hostView.status === "MatchInProgress" || hostView.status === "Complete")) {
      this.publishedDeckSubmissions.add(submissionId);
      return;
    }
    this.emit({ type: "deckSubmitted", seatIndex: seat });
    if (hostView.seats.every((candidate) => candidate.has_submitted_deck || candidate.is_bot)) {
      this.emit({ type: "allDecksSubmitted" });
      // The reducer owns "may pairings be generated?" (`apply_generate_pairings`
      // admits only Deckbuilding | Pairing | RoundComplete). State what every
      // status does rather than sweeping the rest into a fallback.
      //
      // `Pairing` is PostDraftPlay::TournamentPairings, the only status this
      // funnel really reaches besides `Complete`; `generatePairingsInner`
      // republishes and overwrites it. The other seven are unreachable here —
      // `apply_submit_deck`'s last-deck arm assigns only Complete or Pairing,
      // and this branch runs only once every seat has submitted or is a bot —
      // so they route to the reducer, which refuses whatever it will not
      // accept.
      switch (hostView.status) {
        // PostDraftPlay::CompleteImmediately (Quick Draft, Commander Draft).
        // The reducer already assigned Complete and `apply_generate_pairings`
        // refuses it: republish the engine's own view and generate nothing.
        case "Complete":
          await this.broadcastViews();
          break;
        // PostDraftPlay::TournamentPairings. The reducer assigned Pairing, and
        // `apply_generate_pairings` immediately overwrites it with
        // MatchInProgress — so republish BEFORE generating, or nothing ever
        // publishes Pairing and the pod's trajectory skips a phase.
        case "Pairing":
          await this.broadcastViews();
          await this.generatePairingsInner();
          break;
        case "Lobby":
        case "Drafting":
        case "Paused":
        case "Deckbuilding":
        case "MatchInProgress":
        case "RoundComplete":
        case "Abandoned":
          await this.generatePairingsInner();
          break;
        // The exhaustiveness guard: `assertNever`'s parameter is `never`, so
        // THIS LINE is what fails to compile if a tenth DraftStatus is added
        // upstream and left unhandled.
        default:
          assertNever(hostView.status);
      }
    }
    this.publishedDeckSubmissions.add(submissionId);
  }

  private async sendDeckSubmissionAck(
    seat: number,
    submissionId: string,
    view: DraftPlayerView,
  ): Promise<void> {
    if (seat === 0) return;
    await this.guestSessions.get(seat)?.send({
      type: "draft_deck_submit_ack",
      submissionId,
      view,
    });
  }

  // ── Broadcast ──────────────────────────────────────────────────────

  private async broadcastViews(): Promise<void> {
    for (const [seat, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(seat)) continue;
      try {
        const view = await this.adapter.getViewForSeat(seat);
        await session.send({ type: "draft_state_update", view });
      } catch (err) {
        console.error(`[P2PDraftHost] broadcast view error seat ${seat}:`, err);
      }
    }
    // Update host's own view
    try {
      const hostView = await this.adapter.getViewForSeat(0);
      this.emit({ type: "viewUpdated", view: hostView });
    } catch { /* best-effort */ }
  }

  private broadcastToGuests(msg: DraftP2PMessage): void {
    for (const [seat, session] of this.guestSessions) {
      if (this.disconnectedSeats.has(seat)) continue;
      session.send(msg);
    }
  }

  private syncLobbyToGuests(): void {
    const joined = this.occupiedSeatCount();
    const total = this.podSize;
    const seats = this.buildSeatPublicViews();

    for (const session of this.guestSessions.values()) {
      session.send({
        type: "draft_lobby_update",
        seats,
        joined,
        total,
      });
    }

    this.emit({ type: "lobbyUpdate", seats, joined, total });
  }

  // ── Disconnect / Reconnect ─────────────────────────────────────────

  /**
   * A participant's explicit exit revokes the capability rather than opening
   * reconnect grace. Its acknowledgement is deliberately after the durable
   * mutation, so a guest never drops its recovery state for a leave the host
   * could lose on refresh.
   */
  private async handleGuestLeave(
    seat: number,
    draftToken: string,
    originatingSession?: DraftPeerSession,
  ): Promise<void> {
    const session = this.guestSessions.get(seat);
    if (!session || session !== originatingSession || this.seatTokens.get(seat) !== draftToken) return;

    const priorGraceDeadline = this.disconnectedSeats.get(seat)?.deadlineAt;
    const priorReconnectDeadline = this.reconnectDeadlines.get(seat);
    const priorWorkspace = this.perSeatWorkspaceSnapshots.get(seat);
    const priorToken = this.seatTokens.get(seat);
    const priorName = this.seatNames.get(seat);
    const wasKicked = this.kickedTokens.has(draftToken);
    const wasExpired = this.expiredDisconnectedSeats.has(seat);
    let sessionEnded = false;
    let sessionEndedAt: number | null = null;
    const stopWatchingSession = session.onDisconnect(() => {
      sessionEnded = true;
      sessionEndedAt = Date.now();
    });

    let attemptedDisconnect = false;
    try {
      this.guestSessions.delete(seat);
      this.clearReconnectGrace(seat);
      this.reconnectDeadlines.delete(seat);
      this.perSeatWorkspaceSnapshots.delete(seat);

      if (!this.draftStarted) {
        this.seatTokens.delete(seat);
        this.seatNames.delete(seat);
      } else {
        this.kickedTokens.add(draftToken);
        this.seatTokens.delete(seat);
        this.expiredDisconnectedSeats.add(seat);
        // A rejected adapter call can still have changed the engine. Treat the
        // attempted transition as needing compensation either way.
        attemptedDisconnect = true;
        await this.adapter.setSeatConnected(seat, false);
      }

      // Leave is a compensating transaction: unlike a reducer command, its
      // failed snapshot must never be retained and replayed after this branch
      // restores the participant's recovery capability.
      await this.persistSessionStrict({ retainFailedDraftSnapshot: false });
    } catch (error) {
      // A failed leave transition leaves the prior durable session
      // authoritative. Restore its complete in-memory counterpart before
      // returning without an acknowledgement, so the existing recovery
      // capability stays usable.
      if (!sessionEnded) this.guestSessions.set(seat, session);
      const reconnectDeadline = priorGraceDeadline
        ?? priorReconnectDeadline
        ?? (sessionEndedAt === null ? undefined : sessionEndedAt + this.gracePeriodMs);
      if (sessionEnded && reconnectDeadline !== undefined) {
        this.scheduleReconnectGrace(seat, reconnectDeadline);
      } else if (priorGraceDeadline !== undefined) {
        this.scheduleReconnectGrace(seat, priorGraceDeadline);
      } else if (priorReconnectDeadline !== undefined) {
        this.reconnectDeadlines.set(seat, priorReconnectDeadline);
      }
      if (priorWorkspace !== undefined) this.perSeatWorkspaceSnapshots.set(seat, priorWorkspace);
      if (priorToken !== undefined) this.seatTokens.set(seat, priorToken);
      if (priorName !== undefined) this.seatNames.set(seat, priorName);
      if (wasKicked) this.kickedTokens.add(draftToken);
      else this.kickedTokens.delete(draftToken);
      if (wasExpired) this.expiredDisconnectedSeats.add(seat);
      else this.expiredDisconnectedSeats.delete(seat);
      if (this.draftStarted && attemptedDisconnect && !sessionEnded) {
        try {
          await this.adapter.setSeatConnected(seat, true);
        } catch (rollbackError) {
          // The durable state and every local recovery record have already
          // been restored. Surface the adapter failure without sacrificing the
          // participant's capability by letting rollback abort halfway through.
          const message = rollbackError instanceof Error ? rollbackError.message : String(rollbackError);
          console.error("[P2PDraftHost] leave rollback connectivity failed:", rollbackError);
          this.emit({ type: "error", message: `leave rollback connectivity failed: ${message}` });
        }
      }
      if (sessionEnded) {
        if (this.draftStarted) {
          try {
            // A connection that closed during the failed leave is still gone.
            // Keep the engine paused/disconnected for its recovered grace
            // window instead of compensating it back to a live seat.
            await this.adapter.setSeatConnected(seat, false);
          } catch (disconnectError) {
            this.reportDetachedMutationFailure("leave disconnect recovery", disconnectError);
          }
        }
        try {
          await this.persistSessionStrict({ retainFailedDraftSnapshot: false });
        } catch (persistError) {
          this.reportDetachedMutationFailure("leave disconnect recovery", persistError);
        }
        // A second persistence failure cannot make this lost connection look
        // live. Pause/synchronize from the restored local state regardless of
        // whether its recovery snapshot made it to storage.
        if (this.draftStarted) this.reconcileEffectivePause();
        else this.syncLobbyToGuests();
      }
      throw error;
    } finally {
      stopWatchingSession();
    }
    try {
      await session.send({
        type: "draft_leave_ack",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        draftToken,
      });
    } catch (error) {
      // The leave is already durable. A failed notification cannot skip
      // terminal cleanup or hide the departure from the remaining seats.
      console.warn("[P2PDraftHost] leave acknowledgement failed:", error);
    }
    session.close("Participant left draft");

    if (this.draftStarted) {
      await this.broadcastViews();
      this.reconcileEffectivePause();
    } else {
      this.syncLobbyToGuests();
    }
    this.emit({ type: "seatDisconnected", seatIndex: seat });
  }

  private handleGuestDisconnect(seat: number): void {
    if (!this.guestSessions.has(seat)) return;
    if (this.disconnectedSeats.has(seat)) return;

    this.guestSessions.delete(seat);

    this.scheduleReconnectGrace(
      seat,
      this.reconnectDeadlines.get(seat) ?? Date.now() + this.gracePeriodMs,
    );
    // The socket callback is synchronous, but all externally visible state
    // follows the one durable queue: connected bitmap → snapshot → views/pause.
    this.runDetachedMutation("guest disconnect", async () => {
      if (this.draftStarted) await this.adapter.setSeatConnected(seat, false);
      await this.persistSessionStrict();
      if (this.draftStarted) {
        await this.broadcastViews();
        this.reconcileEffectivePause();
      } else {
        this.syncLobbyToGuests();
      }
      this.emit({ type: "seatDisconnected", seatIndex: seat });
    });
  }

  private scheduleReconnectGrace(seat: number, deadlineAt: number): void {
    this.reconnectDeadlines.set(seat, deadlineAt);
    const remainingMs = Math.max(0, deadlineAt - Date.now());
    const timer = setTimeout(() => {
      this.runDetachedMutation("reconnect grace expiry", () => this.expireReconnectGrace(seat));
    }, remainingMs);
    this.disconnectedSeats.set(seat, { deadlineAt, timer });
  }

  private clearReconnectGrace(seat: number): void {
    const grace = this.disconnectedSeats.get(seat);
    if (grace?.timer !== null && grace?.timer !== undefined) clearTimeout(grace.timer);
    this.disconnectedSeats.delete(seat);
  }

  /**
   * Only this derived state controls visibility and timers.  Clearing a
   * reconnect grace must never accidentally cancel a host's explicit pause.
   */
  private reconcileEffectivePause(): void {
    const shouldPause = this.manualPause || this.disconnectedSeats.size > 0 || this.expiredDisconnectedSeats.size > 0;
    if (shouldPause === this.paused) return;
    this.paused = shouldPause;
    if (shouldPause) {
      this.freezeActiveTimer();
      const reason = this.manualPause
        ? DraftPauseReason.PausedByHost
        : DraftPauseReason.PlayerDisconnected;
      this.broadcastToGuests({ type: "draft_paused", reason });
      this.emit({ type: "draftPaused", reason });
      return;
    }
    this.broadcastToGuests({ type: "draft_resumed" });
    this.emit({ type: "draftResumed" });
    if (this.resumeFrozenTimer()) return;
    if (this.draftStarted && this.podPolicy === "Competitive") {
      void (async () => {
        try {
          const view = await this.adapter.getViewForSeat(0);
          if (view.status === "Drafting") this.startPickTimer(view.pick_number);
        } catch { /* A later authoritative operation will retry timer setup. */ }
      })();
    }
  }

  /** One durable terminal transition for live and recovered reconnect grace. */
  private async expireReconnectGrace(seat: number): Promise<void> {
    // Grace expiry remains an effective pause: a missing player cannot
    // silently resume the pod just because their reconnect window ended.
    if (!this.disconnectedSeats.has(seat)) return;
    this.clearReconnectGrace(seat);
    this.reconnectDeadlines.delete(seat);
    if (!this.draftStarted) {
      this.seatTokens.delete(seat);
      this.seatNames.delete(seat);
      this.perSeatWorkspaceSnapshots.delete(seat);
      await this.persistSessionStrict();
      this.syncLobbyToGuests();
      this.emit({ type: "seatDisconnected", seatIndex: seat });
      return;
    }
    await this.adapter.setSeatConnected(seat, false);
    this.expiredDisconnectedSeats.add(seat);
    await this.persistSessionStrict();
    this.broadcastToGuests({
      type: "draft_paused",
      reason: DraftPauseReason.DisconnectGraceExpired,
    });
    this.emit({ type: "seatKicked", seatIndex: seat, reason: DraftPauseReason.DisconnectGraceExpired });
    this.emit({ type: "draftPaused", reason: DraftPauseReason.DisconnectGraceExpired });
    this.reconcileEffectivePause();
  }

  // ── Timer management ─────────────────────────────────────────────────

  private clearActiveTimer(): void {
    if (this.timerInterval !== null) {
      clearInterval(this.timerInterval);
      this.timerInterval = null;
    }
    this.timerContext = null;
    this.timerTargetMatchId = null;
  }

  private freezeActiveTimer(): void {
    if (this.timerContext) {
      this.frozenTimer = {
        context: this.timerContext,
        remainingMs: Math.max(0, this.timerEndAt - Date.now()),
        matchId: this.timerTargetMatchId,
      };
    }
    this.clearActiveTimer();
  }

  private resumeFrozenTimer(): boolean {
    const timer = this.frozenTimer;
    this.frozenTimer = null;
    if (!timer || timer.remainingMs <= 0) return false;
    switch (timer.context) {
      case "pick": this.startPickTimer(undefined, timer.remainingMs); return true;
      case "sideboard": if (timer.matchId) { this.startSideboardTimer(timer.matchId, timer.remainingMs); return true; } return false;
      case "playdraw": if (timer.matchId) { this.startPlayDrawTimer(timer.matchId, timer.remainingMs); return true; } return false;
    }
  }

  private startPickTimer(pickNumber?: number, durationOverride?: number): void {
    this.clearActiveTimer();
    if (this.podPolicy !== "Competitive") return;
    this.timerContext = "pick";
    const duration = durationOverride ?? pickTimerDurationMs(pickNumber ?? 0);
    this.timerRemainingMs = duration;
    this.timerEndAt = Date.now() + duration;
    this.timerInterval = setInterval(() => {
      this.onPickTimerTick();
    }, 1_000);
  }

  private onPickTimerTick(): void {
    this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
    this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
    if (this.timerRemainingMs <= 0) {
      this.clearActiveTimer();
      this.emit({ type: "timerExpired" });
      this.runDetachedMutation("pick timer expiry", () => this.autoPickAllPending());
    }
  }

  private startSideboardTimer(matchId: string, durationOverride?: number): void {
    this.clearActiveTimer();
    this.timerContext = "sideboard";
    const duration = durationOverride ?? 60_000;
    this.timerTargetMatchId = matchId;
    this.timerRemainingMs = duration;
    this.timerEndAt = Date.now() + duration;
    this.timerInterval = setInterval(() => {
      this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
      this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
      if (this.timerRemainingMs <= 0) {
        this.clearActiveTimer();
        this.runDetachedMutation("sideboard timer expiry", () => this.autoSubmitSideboards(matchId));
      }
    }, 1_000);
  }

  private startPlayDrawTimer(matchId: string, durationOverride?: number): void {
    this.clearActiveTimer();
    this.timerContext = "playdraw";
    const duration = durationOverride ?? 10_000;
    this.timerTargetMatchId = matchId;
    this.timerRemainingMs = duration;
    this.timerEndAt = Date.now() + duration;
    this.timerInterval = setInterval(() => {
      this.timerRemainingMs = Math.max(0, this.timerEndAt - Date.now());
      this.broadcastToGuests({ type: "draft_timer_sync", remainingMs: this.timerRemainingMs });
      if (this.timerRemainingMs <= 0) {
        this.clearActiveTimer();
        this.runDetachedMutation("play-draw timer expiry", () => this.autoChoosePlayDraw(matchId));
      }
    }, 1_000);
  }

  private async autoPickAllPending(): Promise<void> {
    // For each seat that still has a current_pack (hasn't picked), auto-pick
    // a random card (D-02). Skip seats already in `picksThisRound` — they've
    // already submitted this round and the engine would reject the duplicate
    // with `SeatAlreadyPickedThisRound`, swallowing the error and stranding
    // the timer at zero.
    //
    // Pass `resolveBots: false` to `handlePick` so the per-pick bot-pick
    // resolution and view broadcast are suppressed during the sweep. Otherwise
    // an N-seat sweep produces N redundant broadcasts (and N redundant bot
    // resolution sweeps). After the loop we resolve bots once and broadcast
    // once — except when the round naturally completed via `allPicksSubmitted`
    // inside the last `handlePick`, which already broadcast.
    let anyPicked = false;
    for (let seat = 0; seat < this.activePodSize; seat++) {
      if (this.picksThisRound.has(seat)) continue;
      try {
        const view = await this.adapter.getViewForSeat(seat);
        if (view.current_pack && view.current_pack.length > 0) {
          // CR 903.13b: the sweep owes one whole pick step, not one card.
          await this.handlePick(
            seat,
            randomDistinctCards(view.current_pack, view.required_pick_count),
            false,
          );
          anyPicked = true;
        }
      } catch (err) {
        console.error(`[P2PDraftHost] auto-pick failed for seat ${seat}:`, err);
      }
    }
    if (anyPicked) {
      await this.resolveBotPicks({ emit: true, persist: true });
      const allPicked = await this.adapter.allPicksSubmitted();
      if (!allPicked) {
        await this.broadcastViews();
      }
    }
  }

  private async resolveBotPicks(options: PickOptions = { emit: true, persist: true }): Promise<void> {
    const hostView = await this.adapter.getViewForSeat(0);
    if (hostView.status !== "Drafting") return;

    for (const seat of hostView.seats) {
      if (!seat.is_bot) continue;
      const view = await this.adapter.getViewForSeat(seat.seat_index);
      const pack = view.current_pack;
      if (!pack || pack.length === 0) continue;

      // CR 903.13b: a bot owes one whole pick step, not one card. The count is
      // the engine's; a one-id pick into a Commander pod is refused by
      // `apply_pick_inner` with `WrongPickCardCount`, and `resolveBotPicks` has
      // no try/catch, so it would strand the round.
      await this.applyPick(
        seat.seat_index,
        randomDistinctCards(pack, view.required_pick_count),
        { acknowledge: false, emit: options.emit, persist: options.persist, resolveBots: false },
      );
    }
  }

  private isBotSeat(seat: number): boolean {
    return this.seatNames.get(seat) === undefined && !this.guestSessions.has(seat);
  }

  private botNameForSeat(seat: number, seed: number): string {
    return assignAvatarForSeat(this.podSize, seat, seed)?.name ?? `Seat ${seat + 1}`;
  }

  // ── Match coordination ────────────────────────────────────────────────

  /**
   * Generate the next round's pairings and dispatch match start messages.
   * The engine decides which round that is; we read it back off the view.
   * Called after all decks are submitted or after round advancement.
   */
  async generatePairings(): Promise<void> {
    return this.enqueueAuthoritativeMutation(() => this.generatePairingsInner());
  }

  private async generatePairingsInner(): Promise<void> {
    try {
      const view = await this.adapter.generatePairings();
      await this.persistSessionStrict();
      // The engine owns the round. Read it back; never compute it here.
      const round = view.current_round;
      const launchablePairings = view.pairings.filter((pairing) =>
        pairing.round === round &&
        (pairing.status === "Pending" || pairing.status === "InProgress")
      );

      for (const pairing of launchablePairings) {
        if (
          this.isBotSeatFromView(view, pairing.seat_a) &&
          this.isBotSeatFromView(view, pairing.seat_b)
        ) {
          await this.dispatchMatchLaunch(pairing, view);
        }
      }

      const postBotView = await this.adapter.getViewForSeat(0);
      for (const pairing of postBotView.pairings) {
        if (pairing.round !== round) continue;
        if (pairing.status !== "Pending" && pairing.status !== "InProgress") continue;
        if (
          this.isBotSeatFromView(postBotView, pairing.seat_a) &&
          this.isBotSeatFromView(postBotView, pairing.seat_b)
        ) {
          continue;
        }

        await this.dispatchMatchLaunch(pairing, postBotView);
      }

      const latestView = await this.adapter.getViewForSeat(0);

      // Launch records may have changed while dispatching. Fence that final
      // snapshot before either broadcasts or the host UI observe it.
      await this.persistSessionStrict();
      await this.broadcastViews();
      this.emit({ type: "pairingsGenerated", round, pairings: latestView.pairings });
      this.emit({ type: "viewUpdated", view: latestView });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[P2PDraftHost] generatePairings failed:`, message);
      this.emit({ type: "error", message: `Failed to generate pairings: ${message}` });
    }
  }

  private matchBindingFor(pairing: PairingView, matchAuthoritySeat: number): DraftMatchBinding {
    const existing = this.matchBindings.get(pairing.match_id);
    if (existing && existing.round === pairing.round) return existing;

    const binding: DraftMatchBinding = {
      podId: this.draftCode,
      matchId: pairing.match_id,
      round: pairing.round,
      sessionKey: crypto.randomUUID(),
      lease: crypto.randomUUID(),
      nonce: crypto.randomUUID(),
      revision: 0,
      matchAuthoritySeat,
    };
    this.matchBindings.set(pairing.match_id, binding);
    return binding;
  }

  private async acceptMatchSettlement(
    submittingSeat: number,
    settlement: DraftMatchSettlement,
  ): Promise<void> {
    const binding = this.matchBindings.get(settlement.binding.matchId);
    if (!binding || !this.sameBinding(binding, settlement.binding)) {
      this.guestSessions.get(submittingSeat)?.send({ type: "draft_error", reason: "Invalid match binding" });
      return;
    }
    const view = await this.adapter.getViewForSeat(0);
    const pairing = view.pairings.find(
      (candidate) => candidate.match_id === binding.matchId && candidate.round === binding.round,
    );
    if (
      !pairing ||
      view.current_round !== binding.round ||
      submittingSeat !== binding.matchAuthoritySeat ||
      (settlement.winnerSeat !== null &&
        settlement.winnerSeat !== pairing.seat_a &&
        settlement.winnerSeat !== pairing.seat_b)
    ) {
      this.guestSessions.get(submittingSeat)?.send({ type: "draft_error", reason: "Unauthorized match settlement" });
      return;
    }

    const receipt = this.settlementReceipts.get(binding.matchId);
    if (receipt) {
      if (receipt.receiptId === settlement.receiptId) {
        void this.sendSettlementAck(submittingSeat, binding.matchId, receipt);
      } else {
        this.sendToSeat(submittingSeat, {
          type: "draft_error",
          reason: "Match already settled",
        });
      }
      return;
    }

    // Persist the intent before invoking the draft reducer. A recovered pod
    // can retry this record without applying a second result.
    this.settlementOutbox.set(settlement.receiptId, settlement);
    await this.persistSessionStrict();
    await this.reportMatchResult(binding.matchId, settlement.winnerSeat);
    const accepted = { receiptId: settlement.receiptId, revision: binding.revision };
    this.settlementReceipts.set(binding.matchId, accepted);
    this.settlementOutbox.delete(settlement.receiptId);
    await this.persistSessionStrict();
    void this.sendSettlementAck(submittingSeat, binding.matchId, accepted);
  }

  private sameBinding(left: DraftMatchBinding, right: DraftMatchBinding): boolean {
    return left.podId === right.podId
      && left.matchId === right.matchId
      && left.round === right.round
      && left.sessionKey === right.sessionKey
      && left.lease === right.lease
      && left.nonce === right.nonce
      && left.revision === right.revision
      && left.matchAuthoritySeat === right.matchAuthoritySeat;
  }

  private async sendSettlementAck(
    seat: number,
    matchId: string,
    receipt: { receiptId: string; revision: number },
  ): Promise<void> {
    const message: DraftP2PMessage = {
      type: "draft_match_settlement_ack",
      matchId,
      receiptId: receipt.receiptId,
      revision: receipt.revision,
    };
    if (seat === 0) return;
    try {
      await this.guestSessions.get(seat)?.send(message);
    } catch (error) {
      // The receipt is already durable; an exact retry can acknowledge it
      // without applying the result to the reducer again.
      console.warn("[P2PDraftHost] settlement acknowledgement failed:", error);
    }
  }

  /**
   * The N-seat deck payload for a completed Commander pod (CR 903.13a: "a draft
   * ... followed by a multiplayer game").
   *
   * A different SHAPE from `dispatchMatchLaunch`'s pairwise assembly — N seats
   * in game-player order rather than two — and it is the only place the
   * seat -> game-player mapping is defined. It lives here rather than in the
   * store because the three per-seat primitives it composes and
   * `exportDraftSession` are all private on this class, and because a
   * game-shaped ordering rule does not belong in the display layer.
   *
   * The local seat becomes game player 0; the remaining seats, in ascending
   * seat order, become game player 1 (`opponent`) and then 2..N-1 (`ai_decks`).
   * Throws when the local seat has no submitted deck — the existing
   * `submittedDeckForSeat` throw, not a new error path.
   */
  async podCommanderDeckPayload(
    view: DraftPlayerView,
    localSeat: number,
  ): Promise<DraftMatchDeckPayload> {
    const session = await this.exportDraftSession();
    const deckForSeat = async (seat: number): Promise<DraftDeckPayload> =>
      this.isBotSeatFromView(view, seat)
        ? this.botDeckForSeat(session, seat)
        : this.submittedDeckForSeat(session, seat);

    const player = await deckForSeat(localSeat);
    const others = view.seats
      .map((s) => s.seat_index)
      .filter((seat) => seat !== localSeat)
      .sort((a, b) => a - b);

    const decks: DraftDeckPayload[] = [];
    for (const seat of others) {
      decks.push(await deckForSeat(seat));
    }

    const [opponent, ...aiDecks] = decks;
    return {
      player,
      opponent,
      ai_decks: aiDecks,
      draft_set_codes: view.draft_set_codes,
    };
  }

  private async dispatchMatchLaunch(pairing: PairingView, view: DraftPlayerView): Promise<void> {
    const seatA = pairing.seat_a;
    const seatB = pairing.seat_b;
    const seatAIsBot = this.isBotSeatFromView(view, seatA);
    const seatBIsBot = this.isBotSeatFromView(view, seatB);
    if (seatAIsBot && seatBIsBot) {
      await this.reportMatchResult(pairing.match_id, Math.min(seatA, seatB));
      return;
    }

    const session = await this.exportDraftSession();

    if (seatAIsBot || seatBIsBot) {
      const humanSeat = seatAIsBot ? seatB : seatA;
      const binding = this.matchBindingFor(pairing, humanSeat);
      const botSeat = seatAIsBot ? seatA : seatB;
      const botName = seatAIsBot ? pairing.name_a : pairing.name_b;
      const humanDeck = this.submittedDeckForSeat(session, humanSeat);
      const botDeck = await this.botDeckForSeat(session, botSeat);
      const deckPayload: DraftMatchDeckPayload = {
        player: humanDeck,
        opponent: botDeck,
        ai_decks: [],
      };

      await this.sendMatchLaunch(humanSeat, {
          type: "Bot",
          matchId: pairing.match_id,
          round: pairing.round,
          localSeat: humanSeat,
          botSeat,
          botName,
          deckPayload,
          matchConfig: matchConfigForView(view),
          binding,
      });
      return;
    }

    const matchHostSeat = Math.min(seatA, seatB);
    const binding = this.matchBindingFor(pairing, matchHostSeat);
    const guestSeat = matchHostSeat === seatA ? seatB : seatA;
    const matchRoomCode = `${this.draftCode ?? "draft"}-${pairing.match_id}`;
    const hostDeck = this.submittedDeckForSeat(session, matchHostSeat);
    const guestDeck = this.submittedDeckForSeat(session, guestSeat);
    const hostOpponentName = matchHostSeat === seatA ? pairing.name_b : pairing.name_a;
    const guestOpponentName = matchHostSeat === seatA ? pairing.name_a : pairing.name_b;
    const deckPayload: DraftMatchDeckPayload = {
      player: hostDeck,
      opponent: guestDeck,
      ai_decks: [],
    };

    await this.sendMatchLaunch(matchHostSeat, {
        type: "HumanHost",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: matchHostSeat,
        opponentSeat: guestSeat,
        opponentName: hostOpponentName,
        matchHostPeerId: matchRoomCode,
        deckPayload,
        matchConfig: matchConfigForView(view),
        binding,
    });
    await this.sendMatchLaunch(guestSeat, {
        type: "HumanGuest",
        matchId: pairing.match_id,
        matchRoomCode,
        round: pairing.round,
        localSeat: guestSeat,
        opponentSeat: matchHostSeat,
        opponentName: guestOpponentName,
        matchHostPeerId: matchRoomCode,
        localDeck: guestDeck,
        matchConfig: matchConfigForView(view),
        binding,
    });
  }

  private async sendMatchLaunch(seat: number, launch: DraftMatchLaunch): Promise<void> {
    this.rememberMatchDecks(launch);
    let launches = this.matchLaunches.get(launch.matchId);
    if (!launches) {
      launches = new Map();
      this.matchLaunches.set(launch.matchId, launches);
    }
    launches.set(seat, launch);
    let digests = this.launchDigests.get(launch.matchId);
    if (!digests) {
      digests = new Map();
      this.launchDigests.set(launch.matchId, digests);
    }
    digests.set(seat, draftIntergameDigest(launch));
    await this.persistSessionStrict();
    this.sendToSeat(seat, { type: "draft_match_start", launch });
  }

  private rememberMatchDecks(launch: DraftMatchLaunch): void {
    let decks = this.matchDecks.get(launch.matchId);
    if (!decks) {
      decks = new Map();
      this.matchDecks.set(launch.matchId, decks);
    }
    switch (launch.type) {
      case "HumanHost":
        decks.set(launch.localSeat, launch.deckPayload.player);
        decks.set(launch.opponentSeat, launch.deckPayload.opponent);
        break;
      case "HumanGuest":
        decks.set(launch.localSeat, launch.localDeck);
        break;
      case "Bot":
        decks.set(launch.localSeat, launch.deckPayload.player);
        decks.set(launch.botSeat, launch.deckPayload.opponent);
        break;
    }
  }

  private async dispatchMatchLaunchesForSeat(view: DraftPlayerView, seat: number): Promise<void> {
    for (const pairing of view.pairings) {
      if (pairing.round !== view.current_round) continue;
      if (pairing.status !== "Pending" && pairing.status !== "InProgress") continue;
      if (pairing.seat_a !== seat && pairing.seat_b !== seat) continue;

      await this.dispatchMatchLaunch(pairing, view);
    }
  }

  private isBotSeatFromView(view: DraftPlayerView, seat: number): boolean {
    return view.seats.find((s) => s.seat_index === seat)?.is_bot ?? this.isBotSeat(seat);
  }

  private async exportDraftSession(): Promise<ExportedDraftSession> {
    const sessionJson = await this.adapter.exportSession();
    return JSON.parse(sessionJson) as ExportedDraftSession;
  }

  private submittedDeckForSeat(session: ExportedDraftSession, seat: number): DraftDeckPayload {
    const submitted = Object.values(session.submitted_decks ?? {}).find(
      (deck) => deck.seat === seat,
    );
    if (!submitted) {
      throw new Error(`Seat ${seat} has no submitted deck`);
    }
    return deckPayload(
      submitted.main_deck,
      sideboardFromPool(session, seat, submitted.main_deck),
      submitted.commanders ?? [],
    );
  }

  private async botDeckForSeat(
    session: ExportedDraftSession,
    botSeat: number,
  ): Promise<DraftDeckPayload> {
    const suggested = await this.adapter.getBotDeck(botSeat);
    const mainDeck = [
      ...suggested.main_deck,
      ...Object.entries(suggested.lands).flatMap(([name, count]) =>
        Array<string>(count).fill(name),
      ),
    ];
    // CR 903.3: the designation is a member of `suggested.main_deck` (the
    // engine guarantees it), so carrying it here adds no name and loses none.
    return deckPayload(
      mainDeck,
      sideboardFromPool(session, botSeat, suggested.main_deck),
      suggested.commander,
    );
  }


  /**
   * Report a match result. Called when a guest sends draft_match_result.
   * T-57-06: validates matchId exists in current round pairings.
   */
  async reportMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    try {
      const view = await this.adapter.reportMatchResult(matchId, winnerSeat);
      await this.persistSessionStrict();
      this.emit({ type: "matchResultReceived", matchId, winnerSeat });

      // Broadcast updated views with new standings
      await this.broadcastViews();
      this.emit({ type: "viewUpdated", view });

      // Check if the reducer auto-advanced (Competitive mode)
      if (view.status === "Complete") {
        void this.cleanupServerBackup();
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[P2PDraftHost] reportMatchResult failed:`, message);
      throw err;
    }
  }

  /** Seat 0 uses the same authenticated settlement gate as remote match hosts. */
  async submitHostMatchSettlement(settlement: DraftMatchSettlement): Promise<void> {
    await this.enqueueAuthoritativeMutation(() => this.acceptMatchSettlement(0, settlement));
  }

  /**
   * Advance to the next round (Casual mode, host-only).
   * T-57-07: only callable from host UI; guests sending draft_request_advance are ignored.
   */
  async advanceRound(): Promise<void> {
    return this.enqueueAuthoritativeMutation(() => this.advanceRoundInner());
  }

  private async advanceRoundInner(): Promise<void> {
    try {
      await this.adapter.advanceRound();
      await this.persistSessionStrict();
      this.emit({ type: "roundAdvanced" });
      await this.generatePairingsInner();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to advance round: ${message}` });
    }
  }

  /**
   * Replace a disconnected player with a bot (Casual mode, host-only).
   */
  async replaceSeatWithBot(seat: number): Promise<void> {
    return this.enqueueAuthoritativeMutation(() => this.replaceSeatWithBotInner(seat));
  }

  private async replaceSeatWithBotInner(seat: number): Promise<void> {
    try {
      const seed = this.draftSeed ?? hashStringToSeed(this.draftCode || this.roomCode || "draft");
      await this.adapter.replaceSeatWithBot(seat, this.botNameForSeat(seat, seed));
      const grace = this.disconnectedSeats.get(seat);
      if (grace) this.clearReconnectGrace(seat);
      this.reconnectDeadlines.delete(seat);
      this.expiredDisconnectedSeats.delete(seat);
      this.seatTokens.delete(seat);
      this.seatNames.delete(seat);
      await this.persistSessionStrict();
      await this.broadcastViews();
      this.reconcileEffectivePause();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.emit({ type: "error", message: `Failed to replace seat ${seat}: ${message}` });
    }
  }

  /**
   * Override a match result (Casual mode, host-only).
   */
  async overrideMatchResult(matchId: string, winnerSeat: number | null): Promise<void> {
    await this.enqueueAuthoritativeMutation(() => this.reportMatchResult(matchId, winnerSeat));
  }

  // ── Bo3 Between-Games Orchestration ────────────────────────────────────

  /**
   * Orchestrates the between-games flow for a Bo3 match.
   * Called when the match adapter detects BetweenGamesSideboard waiting state.
   */
  handleMatchBetweenGames(
    matchId: string,
    gameNumber: number,
    score: MatchScore,
    loserSeat: number | null,
    seatA: number,
    seatB: number,
  ): void {
    this.runDetachedMutation("between-games transition", () => this.handleMatchBetweenGamesDurably(
      matchId, gameNumber, score, loserSeat, seatA, seatB,
    ));
  }

  private async handleMatchBetweenGamesDurably(
    matchId: string,
    gameNumber: number,
    score: MatchScore,
    loserSeat: number | null,
    seatA: number,
    seatB: number,
  ): Promise<void> {
    const decks = this.matchDecks.get(matchId);
    this.bo3State.set(matchId, {
      seatA, seatB,
      submittedA: false, submittedB: false,
      loserSeat, gameNumber, score,
      decks: [seatA, seatB].flatMap((seat) => {
        const deck = decks?.get(seat);
        return deck ? [{ seat, ...deckSubmission(deck) }] : [];
      }),
    });

    const timerMs = this.podPolicy === "Competitive" ? 60_000 : 0;

    await this.persistSessionStrict();

    // Send sideboard prompt to both pairing players via draft pod channel
    const prompt: DraftP2PMessage = {
      type: "draft_bo3_sideboard_prompt",
      matchId, gameNumber, score, loserSeat, timerMs,
    };
    this.sendToSeat(seatA, prompt);
    this.sendToSeat(seatB, prompt);

    // Broadcast live score to all guests for standings display
    this.broadcastToGuests({
      type: "draft_bo3_score_update",
      matchId,
      scoreA: score.p0_wins,
      scoreB: score.p1_wins,
    });

    if (timerMs > 0) {
      this.startSideboardTimer(matchId);
    }

    this.emit({ type: "bo3SideboardPromptSent", matchId });
  }

  private async handleGuestBetweenGames(
    seat: number,
    message: Extract<DraftP2PMessage, { type: "draft_bo3_between_games" }>,
  ): Promise<void> {
    const binding = this.matchBindings.get(message.matchId);
    const view = await this.adapter.getViewForSeat(0);
    const pairing = view.pairings.find(
      (candidate) => candidate.match_id === message.matchId && candidate.round === binding?.round,
    );
    if (
      !binding
      || binding.matchAuthoritySeat !== seat
      || view.current_round !== binding.round
      || !pairing
      || (pairing.status !== "Pending" && pairing.status !== "InProgress")
    ) {
      this.guestSessions.get(seat)?.send({ type: "draft_error", reason: "Unauthorized between-games report" });
      return;
    }
    if (this.bo3State.get(message.matchId)?.gameNumber === message.gameNumber) return;

    await this.handleMatchBetweenGamesDurably(
      message.matchId,
      message.gameNumber,
      message.score,
      message.loserSeat,
      pairing.seat_a,
      pairing.seat_b,
    );
  }

  /** The sole command ingress for host UI and authenticated guest sessions. */
  submitAuthorized(seat: number, command: DraftIntergameCommand): void {
    this.runDetachedMutation("authorized intergame command", () => this.submitAuthorizedDurably(seat, command));
  }

  private async submitAuthorizedDurably(seat: number, command: DraftIntergameCommand): Promise<void> {
    if (command.status === "Receipted" && command.receiptId) {
      await this.receiptIntergameCommand(seat, commandAcknowledgement(command), command.receiptId);
      return;
    }
    await this.holdIntergameCommand(seat, command);
  }

  private async holdIntergameCommand(seat: number, command: DraftIntergameCommand): Promise<void> {
    const state = this.bo3State.get(command.matchId);
    const launchDigest = this.launchDigests.get(command.matchId)?.get(seat);
    if (!state
      || command.status !== "Pending"
      || command.seat !== seat
      || command.gameNumber !== state.gameNumber
      || !launchDigest
      || command.launchDigest !== launchDigest
      || draftIntergameDigest(command.launchPayload) !== command.launchDigest
      || command.payloadDigest !== draftIntergameDigest(command.payload)
      || this.intergameCommands.snapshot().some((candidate) => candidate.commandId === command.commandId)) {
      return;
    }
    if (command.payload.type === "SubmitSideboard") {
      const deck = this.matchDecks.get(command.matchId)?.get(seat);
      if (
        (seat !== state.seatA && seat !== state.seatB)
        || !deck
        || !preservesDeckPool(deck, command.payload.main, command.payload.sideboard)
      ) {
        this.sendToSeat(seat, { type: "draft_error", reason: "Invalid sideboard submission" });
        return;
      }
    } else if (state.loserSeat !== seat) {
      return;
    }

    const held = this.intergameCommands.hold({
      commandId: command.commandId,
      matchId: command.matchId,
      gameNumber: command.gameNumber,
      seat,
      payload: command.payload,
      launchPayload: command.launchPayload,
      launchDigest: command.launchDigest,
    });
    switch (held.payload.type) {
      case "SubmitSideboard":
        if (seat === state.seatA) state.submittedA = true;
        else state.submittedB = true;
        await this.persistSessionStrict();
        if (state.submittedA && state.submittedB) {
          this.clearActiveTimer();
          for (const pending of this.intergameCommands.snapshot()) {
            if (pending.matchId === held.matchId
              && pending.gameNumber === held.gameNumber
              && pending.status === "Pending"
              && pending.payload.type === "SubmitSideboard") {
              await this.authorizeIntergameCommand(pending);
            }
          }
          this.emit({ type: "bo3BothSideboardsSubmitted", matchId: held.matchId });
        }
        break;
      case "ChoosePlayDraw":
        await this.authorizeIntergameCommand(held);
        break;
    }
  }

  private async authorizeIntergameCommand(command: DraftIntergameCommand): Promise<void> {
    const acknowledgement = commandAcknowledgement(command);
    const authorized = this.intergameCommands.authorize(command.commandId, acknowledgement);
    if (!authorized) return;
    const permit = this.intergameCommands.begin(command.commandId, acknowledgement);
    if (!permit) return;
    // The controller, not a caller supplied flag, owns the Executing state.
    // This deliberate no-op consumption proves the issuer created the permit;
    // the participant performs the same pre-execution check with its own issuer.
    void permit;
    await this.persistSessionStrict();
    this.sendToSeat(command.seat, {
      type: "draft_bo3_intergame_authorized",
      command: authorized,
      acknowledgement,
    });
  }

  private async receiptIntergameCommand(
    seat: number,
    acknowledgement: DraftIntergameCommandAck,
    receiptId: string,
  ): Promise<void> {
    const command = this.intergameCommands.snapshot().find(
      (candidate) => candidate.commandId === acknowledgement.commandId,
    );
    if (!command || command.seat !== seat || !matchesCommandAcknowledgement(command, acknowledgement)) return;
    const receipted = this.intergameCommands.receipt(command.commandId, acknowledgement, receiptId);
    if (!receipted) return;
    switch (receipted.payload.type) {
      case "SubmitSideboard": {
        const state = this.bo3State.get(receipted.matchId);
        const deck = state?.decks.find((candidate) => candidate.seat === seat);
        if (deck) {
          deck.main = receipted.payload.main;
          deck.sideboard = receipted.payload.sideboard;
        }
        const complete = state && [state.seatA, state.seatB].every((participant) =>
          this.intergameCommands.snapshot().some((candidate) =>
            candidate.matchId === receipted.matchId
              && candidate.gameNumber === receipted.gameNumber
              && candidate.seat === participant
              && candidate.payload.type === "SubmitSideboard"
              && candidate.status === "Receipted"),
        );
        await this.persistSessionStrict();
        if (complete && state) await this.transitionToPlayDraw(receipted.matchId, state);
        break;
      }
      case "ChoosePlayDraw":
        await this.persistSessionStrict();
        await this.resolvePlayDrawChoice(receipted.matchId, receipted.payload.playFirst);
        break;
    }
  }

  private async autoSubmitSideboards(matchId: string): Promise<void> {
    const state = this.bo3State.get(matchId);
    if (!state) return;
    const participants = [state.seatA, state.seatB];
    const submitted = new Set([
      ...(state.submittedA ? [state.seatA] : []),
      ...(state.submittedB ? [state.seatB] : []),
    ]);
    for (const seat of participants) {
      if (submitted.has(seat)) continue;
      const deck = state.decks.find((candidate) => candidate.seat === seat);
      if (!deck) {
        this.emit({ type: "error", message: "Sideboard timer expired without a registered deck" });
        continue;
      }
      await this.submitDefaultIntergameCommand(matchId, state, seat, {
        type: "SubmitSideboard",
        main: deck.main,
        sideboard: deck.sideboard,
      });
    }
  }

  private async autoChoosePlayDraw(matchId: string): Promise<void> {
    const state = this.bo3State.get(matchId);
    if (!state || state.loserSeat === null) return;
    await this.submitDefaultIntergameCommand(matchId, state, state.loserSeat, {
      type: "ChoosePlayDraw",
      playFirst: true,
    });
  }

  /** Timeout defaults enter the same signed launch/ledger path as a player
   * submission, so they cannot bypass authorization or the execution receipt. */
  private async submitDefaultIntergameCommand(
    matchId: string,
    state: Bo3MatchState,
    seat: number,
    payload: DraftIntergameCommand["payload"],
  ): Promise<void> {
    const launch = this.matchLaunches.get(matchId)?.get(seat);
    const launchDigest = this.launchDigests.get(matchId)?.get(seat);
    if (!launch || !launchDigest) {
      this.emit({ type: "error", message: "Intergame timeout lacks launch authority" });
      return;
    }
    await this.holdIntergameCommand(seat, {
      commandId: crypto.randomUUID(),
      matchId,
      gameNumber: state.gameNumber,
      seat,
      payload,
      launchPayload: launch,
      launchDigest,
      payloadDigest: draftIntergameDigest(payload),
      status: "Pending",
    });
  }

  private async transitionToPlayDraw(matchId: string, state: Bo3MatchState): Promise<void> {
    if (state.loserSeat !== null) {
      const timerMs = this.podPolicy === "Competitive" ? 10_000 : 0;
      const prompt: DraftP2PMessage = {
        type: "draft_bo3_play_draw_prompt",
        matchId,
        gameNumber: state.gameNumber,
        score: state.score,
        timerMs,
      };
      this.sendToSeat(state.loserSeat, prompt);
      if (timerMs > 0) this.startPlayDrawTimer(matchId);
    } else {
      // Draw — keep previous first player. Signal game start immediately.
      await this.resolvePlayDrawChoice(matchId, true);
    }
  }

  private async resolvePlayDrawChoice(matchId: string, playFirst: boolean): Promise<void> {
    this.clearActiveTimer();
    const state = this.bo3State.get(matchId);
    if (!state) return;

    const firstPlayerSeat = playFirst
      ? (state.loserSeat ?? state.seatA)
      : (state.loserSeat === state.seatA ? state.seatB : state.seatA);

    this.bo3State.delete(matchId);
    await this.persistSessionStrict();
    const msg: DraftP2PMessage = {
      type: "draft_bo3_game_start",
      matchId,
      gameNumber: state.gameNumber,
      firstPlayerSeat,
    };
    this.sendToSeat(state.seatA, msg);
    this.sendToSeat(state.seatB, msg);

    this.emit({ type: "bo3GameStarted", matchId, gameNumber: state.gameNumber });
  }

  private sendToSeat(seat: number, msg: DraftP2PMessage): void {
    if (seat === 0) {
      // Host is seat 0 — emit event directly instead of sending over network
      switch (msg.type) {
        case "draft_match_start":
          this.emit({ type: "matchStart", launch: msg.launch });
          break;
        case "draft_bo3_sideboard_prompt":
          this.emit({
            type: "bo3SideboardPrompt",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            score: msg.score,
            loserSeat: msg.loserSeat,
            timerMs: msg.timerMs,
          });
          break;
        case "draft_bo3_play_draw_prompt":
          this.emit({
            type: "bo3ChoosePlayDraw",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            score: msg.score,
            timerMs: msg.timerMs,
          });
          break;
        case "draft_bo3_game_start":
          this.emit({
            type: "bo3GameStart",
            matchId: msg.matchId,
            gameNumber: msg.gameNumber,
            firstPlayerSeat: msg.firstPlayerSeat,
          });
          break;
        case "draft_bo3_intergame_authorized":
          this.emit({
            type: "bo3AuthorizedCommand",
            command: msg.command,
            acknowledgement: msg.acknowledgement,
          });
          break;
        default:
          break;
      }
      return;
    }
    const session = this.guestSessions.get(seat);
    if (session && !this.disconnectedSeats.has(seat)) {
      session.send(msg);
    }
  }

  // ── Host controls ──────────────────────────────────────────────────

  kickPlayer(seat: number, reason: string = "Kicked by host"): void {
    this.runDetachedMutation("kick player", () => this.kickPlayerDurably(seat, reason));
  }

  private async kickPlayerDurably(seat: number, reason: string): Promise<void> {
    const token = this.seatTokens.get(seat);
    if (token) this.kickedTokens.add(token);

    const session = this.guestSessions.get(seat);
    if (session) this.guestSessions.delete(seat);

    // Cancel grace timer if active
    const grace = this.disconnectedSeats.get(seat);
    if (grace) this.clearReconnectGrace(seat);
    this.reconnectDeadlines.delete(seat);
    this.expiredDisconnectedSeats.delete(seat);

    await this.persistSessionStrict();
    if (session) {
      session.send({ type: "draft_kicked", reason });
      session.close("Kicked");
    }
    this.emit({ type: "seatKicked", seatIndex: seat, reason });
    this.syncLobbyToGuests();
    this.reconcileEffectivePause();
  }

  requestPause(): void {
    void this.enqueueAuthoritativeMutation(async () => {
      if (this.manualPause) return;
      this.manualPause = true;
      await this.persistSessionStrict();
      this.reconcileEffectivePause();
    }).catch((error: unknown) => console.error("[P2PDraftHost] pause persistence failed:", error));
  }

  requestResume(): void {
    void this.enqueueAuthoritativeMutation(async () => {
      if (!this.manualPause) return;
      this.manualPause = false;
      await this.persistSessionStrict();
      this.reconcileEffectivePause();
    }).catch((error: unknown) => console.error("[P2PDraftHost] resume persistence failed:", error));
  }

  // ── Persistence (P2P-05) ──────────────────────────────────────────

  private persistSession(): void {
    if (!this.persistenceId || this.persistenceClosed) return;
    // Lobby snapshots contain no asynchronous engine export, so capture them
    // at mutation time. A later admission must not leak into an earlier queued
    // snapshot if its own strict write fails.
    const snapshot = this.draftStarted ? undefined : this.buildPersistedSnapshot(null);
    void this.enqueuePersistSession(snapshot).catch(() => {});
  }

  /**
   * Callers await this fence before making a recovery capability externally
   * visible. A caller that fully compensates its mutation can opt out of
   * retaining its failed engine snapshot for replay.
   */
  private persistSessionStrict(
    options: { retainFailedDraftSnapshot?: boolean } = {},
  ): Promise<void> {
    if (!this.persistenceId || this.persistenceClosed) return Promise.resolve();
    return this.enqueuePersistSession(
      this.draftStarted ? undefined : this.buildPersistedSnapshot(null),
      options.retainFailedDraftSnapshot,
    );
  }

  /**
   * Serializes snapshots while retaining a live queue after a failed write.
   * Fire-and-forget mutations report errors through `persistSession`; callers
   * that await the returned task may roll their mutation back on failure.
   */
  private enqueuePersistSession(
    snapshotAtMutation?: PersistedDraftHostSession,
    retainFailedDraftSnapshot = true,
  ): Promise<void> {
    if (!this.persistenceId || this.persistenceClosed) return Promise.resolve();
    const persist = this.persistQueue.then(async () => {
      if (this.persistenceClosed) return;
      // A failed deck/pick snapshot is immutable evidence of an already-run
      // reducer. Retry it before capturing newer state; never retry by
      // applying the command again.
      if (this.pendingDraftSnapshot) {
        await saveDraftHostSession(this.persistenceId!, this.pendingDraftSnapshot);
        this.pendingDraftSnapshot = null;
      }
      const snapshot = snapshotAtMutation ?? this.buildPersistedSnapshot(
        this.draftStarted ? await this.adapter.exportSession() : null,
      );
      if (this.persistenceClosed) return;

      try {
        await saveDraftHostSession(this.persistenceId!, snapshot);
      } catch (error) {
        // Admission has its own transactional rollback.  Only an engine-backed
        // snapshot represents a reducer result that must be replayed exactly.
        if (this.draftStarted && retainFailedDraftSnapshot) this.pendingDraftSnapshot = snapshot;
        throw error;
      }

      // Server backup upload (D-08, T-60-11: rate-limited to every N picks)
      this.picksSinceLastBackup++;
      if (this.backupEndpoint && this.picksSinceLastBackup >= P2PDraftHost.BACKUP_INTERVAL_PICKS) {
        this.picksSinceLastBackup = 0;
        void this.uploadBackupSnapshot(snapshot);
      }
    });
    this.persistQueue = persist.catch((err) => {
      console.warn("[P2PDraftHost] persist failed:", err);
    });
    return persist;
  }

  private buildPersistedSnapshot(draftSessionJson: string | null): PersistedDraftHostSession {
    return {
      persistenceId: this.persistenceId!,
      roomCode: this.roomCode!,
      kind: this.kind,
      podSize: this.podSize,
      hostDisplayName: this.hostDisplayName,
      tournamentFormat: this.tournamentFormat,
      podPolicy: this.podPolicy,
      seatTokens: Object.fromEntries(this.seatTokens),
      seatNames: Object.fromEntries(this.seatNames),
      kickedTokens: [...this.kickedTokens],
      reconnectDeadlines: Object.fromEntries(this.reconnectDeadlines),
      expiredDisconnectedSeats: [...this.expiredDisconnectedSeats],
      draftStarted: this.draftStarted,
      manualPause: this.manualPause,
      draftCode: this.draftCode,
      draftSessionJson,
      poolInput: this.poolInput,
      matchBindings: [...this.matchBindings.values()],
      settlementOutbox: [...this.settlementOutbox.values()],
      settlementReceipts: [...this.settlementReceipts.entries()].map(
        ([matchId, receipt]) => ({ matchId, ...receipt }),
      ),
      intergameCommands: this.intergameCommands.snapshot(),
      bo3State: [...this.bo3State.entries()].map(([matchId, state]) => ({ matchId, ...state })),
      launchDigests: [...this.launchDigests.entries()].flatMap(([matchId, digests]) =>
        [...digests.entries()].map(([seat, digest]) => ({ matchId, seat, digest })),
      ),
      matchLaunches: [...this.matchLaunches.entries()].flatMap(([matchId, launches]) =>
        [...launches.entries()].map(([seat, launch]) => ({ matchId, seat, launch })),
      ),
      deckSubmissionReceipts: [...this.deckSubmissionReceipts.entries()].map(
        ([submissionId, receipt]) => ({ submissionId, ...receipt }),
      ),
      perSeatWorkspaceSnapshots: Object.fromEntries(this.perSeatWorkspaceSnapshots),
    };
  }

  /**
   * Upload a backup snapshot to the phase-server (best-effort, D-08).
   * Failures are silently logged — P2P works without server backup.
   */
  private async uploadBackupSnapshot(snapshot: PersistedDraftHostSession): Promise<void> {
    if (!this.backupEndpoint || !this.draftCode) return;
    try {
      const publicSnapshot = redactChaosAssignmentsFromPublicBackup(snapshot);
      await fetch(`${this.backupEndpoint}/p2p-draft-backup`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          draft_code: this.draftCode,
          host_peer_id: this.hostPeer.id,
          snapshot_json: JSON.stringify(publicSnapshot),
        }),
      });
    } catch (err) {
      console.warn("[P2PDraftHost] server backup upload failed:", err);
    }
  }

  /**
   * Delete the server backup on clean draft completion (best-effort).
   */
  private async cleanupServerBackup(): Promise<void> {
    if (!this.backupEndpoint || !this.draftCode) return;
    try {
      const params = new URLSearchParams({ host_peer_id: this.hostPeer.id });
      await fetch(
        `${this.backupEndpoint}/p2p-draft-backup/${this.draftCode}?${params}`,
        { method: "DELETE" },
      );
    } catch {
      // Best-effort cleanup
    }
  }

  /**
   * Restore host state from a persisted snapshot.
   * Called before `initialize()` to rehydrate a crashed host.
   */
  async restoreFromPersisted(session: PersistedDraftHostSession): Promise<DraftPlayerView | null> {
    this.perSeatWorkspaceSnapshots = new Map(
      Object.entries(session.perSeatWorkspaceSnapshots ?? {}).map(([seat, state]) => [
        Number(seat),
        state,
      ]),
    );
    for (const [seatStr, token] of Object.entries(session.seatTokens)) {
      this.seatTokens.set(Number(seatStr), token);
    }
    for (const [seatStr, name] of Object.entries(session.seatNames)) {
      this.seatNames.set(Number(seatStr), name);
    }
    for (const token of session.kickedTokens) {
      this.kickedTokens.add(token);
    }
    for (const seat of session.expiredDisconnectedSeats ?? []) {
      this.expiredDisconnectedSeats.add(seat);
    }
    this.draftStarted = session.draftStarted;
    this.manualPause = session.manualPause ?? false;
    this.draftCode = session.draftCode;
    this.draftSeed = hashStringToSeed(session.draftCode || this.roomCode || "draft");
    for (const binding of session.matchBindings ?? []) {
      this.matchBindings.set(binding.matchId, binding);
    }
    for (const settlement of session.settlementOutbox ?? []) {
      this.settlementOutbox.set(settlement.receiptId, settlement);
    }
    for (const receipt of session.settlementReceipts ?? []) {
      this.settlementReceipts.set(receipt.matchId, {
        receiptId: receipt.receiptId,
        revision: receipt.revision,
      });
    }
    for (const receipt of session.deckSubmissionReceipts ?? []) {
      this.deckSubmissionReceipts.set(receipt.submissionId, {
        seat: receipt.seat,
        payloadFingerprint: receipt.payloadFingerprint,
      });
    }
    this.intergameCommands = new IntergameCommandController(session.intergameCommands ?? []);
    this.intergameCommands.recover();
    for (const state of session.bo3State ?? []) {
      const { matchId, ...rest } = state;
      this.bo3State.set(matchId, { ...rest, decks: rest.decks ?? [] });
    }
    for (const { matchId, seat, digest } of session.launchDigests ?? []) {
      let digests = this.launchDigests.get(matchId);
      if (!digests) {
        digests = new Map();
        this.launchDigests.set(matchId, digests);
      }
      digests.set(seat, digest);
    }
    for (const { matchId, seat, launch } of session.matchLaunches ?? []) {
      const binding = launch.binding ?? this.matchBindings.get(matchId);
      if (!binding || binding.matchId !== matchId) continue;
      const recoveredLaunch = launch.binding ? launch : { ...launch, binding } as DraftMatchLaunch;
      let launches = this.matchLaunches.get(matchId);
      if (!launches) {
        launches = new Map();
        this.matchLaunches.set(matchId, launches);
      }
      launches.set(seat, recoveredLaunch);
      this.rememberMatchDecks(recoveredLaunch);
    }

    const recoveryStateChanged = this.armRecoveredGuestGrace(session.reconnectDeadlines);
    if (recoveryStateChanged) {
      // Persist the recovery transition from the original engine snapshot
      // before importing it. A second host crash during import therefore sees
      // this same absolute deadline rather than granting a fresh window.
      await this.enqueuePersistSession(this.buildPersistedSnapshot(session.draftSessionJson));
    }

    if (session.draftSessionJson) {
      const view = await this.adapter.importSession(session.draftSessionJson, 2);
      for (const seat of this.expiredDisconnectedSeats) {
        await this.adapter.setSeatConnected(seat, false);
      }
      if (this.reconcileRetainedWorkspace(0, view.pool).changed) this.persistSession();
      await this.recoverSettlementOutbox(view);
      if (this.rotateLegacyBotMatchAuthorities(view)) {
        await this.persistSessionStrict();
      }

      this.reconcileEffectivePause();

      if (view.status === "MatchInProgress") {
        await this.dispatchMatchLaunchesForSeat(view, 0);
      } else if (view.status === "Pairing") {
        // Two engine sites write `Pairing`: `apply_submit_deck` opens the
        // round-0 window once all decks are in, and `apply_advance_round` opens
        // each later one. Neither has generated the pairings the window exists
        // to produce — `apply_generate_pairings` is what generates them, and it
        // immediately leaves for `MatchInProgress`. So `Pairing` always means
        // "not generated yet".
        // `view.pairings` still holds the *previous* round's pairings here
        // (`compute_pairing_views` filters on `current_round`, which
        // `AdvanceRound` deliberately does not bump), so testing it for
        // emptiness made this branch dead for every round after the first.
        //
        // Widening it cannot generate a round twice: generating sets status to
        // `MatchInProgress`, so this branch cannot fire again for the same
        // round; and `AdvanceRound` requires `RoundComplete`, which the final
        // round never enters (it transitions straight to `Complete`), so there
        // is no round past the last one for this branch to invent.
        await this.generatePairingsInner();
        return this.adapter.getViewForSeat(0);
      }

      return view;
    }

    return null;
  }

  /**
   * A recovered host is the first observer that knows its peers are gone. It
   * persists that absolute deadline before serving recovery, so another host
   * recovery cannot reset the grace clock. Snapshots from before this field
   * existed fail closed rather than guessing how much grace remained.
   */
  private armRecoveredGuestGrace(reconnectDeadlines: Record<number, number> | undefined): boolean {
    let changed = false;
    const now = Date.now();
    for (const seat of [...this.seatTokens.keys()]) {
      if (seat === 0 || this.expiredDisconnectedSeats.has(seat)) continue;
      if (reconnectDeadlines === undefined) {
        if (this.draftStarted) {
          this.expiredDisconnectedSeats.add(seat);
        } else {
          this.seatTokens.delete(seat);
          this.seatNames.delete(seat);
          this.perSeatWorkspaceSnapshots.delete(seat);
        }
        changed = true;
        continue;
      }

      const deadlineAt = reconnectDeadlines[seat] ?? now + this.gracePeriodMs;
      if (deadlineAt <= now) {
        this.reconnectDeadlines.delete(seat);
        if (this.draftStarted) {
          this.expiredDisconnectedSeats.add(seat);
        } else {
          this.seatTokens.delete(seat);
          this.seatNames.delete(seat);
          this.perSeatWorkspaceSnapshots.delete(seat);
        }
        changed = true;
        continue;
      }

      this.scheduleReconnectGrace(seat, deadlineAt);
      if (reconnectDeadlines[seat] === undefined) changed = true;
    }
    return changed;
  }

  /**
   * Old snapshots selected the lowest numbered seat as the settlement
   * authority, including bot seats. A bot cannot return a participant
   * settlement, so repair only the active human-versus-bot binding after the
   * restored engine view identifies its human participant. Completed and
   * future-round bindings intentionally retain their historical capability.
   */
  private rotateLegacyBotMatchAuthorities(view: DraftPlayerView): boolean {
    let changed = false;
    for (const pairing of view.pairings ?? []) {
      if (pairing.round !== view.current_round || pairing.status !== "InProgress") continue;

      const seatAIsBot = this.isBotSeatFromView(view, pairing.seat_a);
      const seatBIsBot = this.isBotSeatFromView(view, pairing.seat_b);
      if (seatAIsBot === seatBIsBot) continue;

      const binding = this.matchBindings.get(pairing.match_id);
      if (!binding || binding.round !== pairing.round) continue;

      const humanSeat = seatAIsBot ? pairing.seat_b : pairing.seat_a;
      const botSeat = seatAIsBot ? pairing.seat_a : pairing.seat_b;
      if (binding.matchAuthoritySeat !== botSeat) continue;

      this.matchBindings.set(pairing.match_id, { ...binding, matchAuthoritySeat: humanSeat });
      changed = true;
    }
    return changed;
  }

  /** Replays only write-ahead settlements that the restored draft still lacks. */
  private async recoverSettlementOutbox(view: DraftPlayerView): Promise<void> {
    for (const settlement of [...this.settlementOutbox.values()]) {
      const binding = this.matchBindings.get(settlement.binding.matchId);
      const pairing = view.pairings.find(
        (candidate) => candidate.match_id === settlement.binding.matchId,
      );
      if (!binding || !this.sameBinding(binding, settlement.binding) || !pairing) continue;
      if (pairing.status === "Pending" || pairing.status === "InProgress") {
        await this.reportMatchResult(settlement.binding.matchId, settlement.winnerSeat);
      }
      this.settlementReceipts.set(settlement.binding.matchId, {
        receiptId: settlement.receiptId,
        revision: settlement.binding.revision,
      });
      this.settlementOutbox.delete(settlement.receiptId);
    }
    this.persistSession();
  }

  // ── Cleanup ────────────────────────────────────────────────────────

  async dispose(): Promise<void> {
    // Closing this synchronously is a write fence: `persistSession` continuations
    // may already be queued, but none may snapshot or save after their host loses
    // ownership to a newer recovery using the same persistence ID.
    this.persistenceClosed = true;
    this.clearActiveTimer();
    if (this.hostConnectionUnsub) this.hostConnectionUnsub();
    for (const { timer } of this.disconnectedSeats.values()) {
      if (timer !== null) clearTimeout(timer);
    }
    this.disconnectedSeats.clear();
    this.reconnectDeadlines.clear();
    this.bo3State.clear();
    this.matchDecks.clear();
    this.matchLaunches.clear();
    for (const session of this.guestSessions.values()) {
      session.close();
    }
    this.guestSessions.clear();
    this.listeners = [];
    await this.persistQueue;
  }

  async terminateDraft(): Promise<void> {
    // Fence queued non-terminal saves before awaiting guest notifications.
    this.persistenceClosed = true;
    for (const session of this.guestSessions.values()) {
      try {
        await session.send({ type: "draft_host_left", reason: "Host left the draft" });
      } catch (error) {
        // A disconnected guest cannot prevent notification of the remaining
        // guests or the terminal cleanup of the host's durable session.
        console.warn("[P2PDraftHost] termination notification failed:", error);
      }
    }
    await this.persistQueue;
    if (this.persistenceId) {
      await clearDraftHostSession(this.persistenceId);
    }
    void this.cleanupServerBackup();
    await this.dispose();
    try {
      this.hostPeer.destroy();
    } catch { /* best-effort */ }
  }

  // ── Helpers ────────────────────────────────────────────────────────

  private firstOpenSeat(): number | null {
    for (let i = 1; i < this.podSize; i++) {
      if (!this.seatTokens.has(i)) return i;
    }
    return null;
  }

  private occupiedSeatCount(): number {
    // Host (seat 0) + connected guests
    return 1 + this.seatTokens.size - (this.seatTokens.has(0) ? 0 : 0);
  }

  private buildSeatPublicViews(): SeatPublicView[] {
    const seats: SeatPublicView[] = [];
    for (let i = 0; i < this.podSize; i++) {
      seats.push({
        seat_index: i,
        display_name: this.seatNames.get(i) ?? "",
        is_bot: false,
        connected: i === 0 || this.guestSessions.has(i),
        has_submitted_deck: false,
        pick_status: "NotDrafting",
        active_pack_count: 0,
        face_up_draft_cards: [],
      });
    }
    return seats;
  }

  private buildLobbyView(): DraftPlayerView {
    // A null procedure means `buildLobbyView` ran before `initialize()`, which
    // is a programming error. Throw rather than default: a silent 40-card
    // fallback would advertise the CR 100.2b limited floor for a CR 903.13f(1)
    // 60-card format — exactly the class of defect the procedure read exists to
    // prevent.
    if (!this.procedure) {
      throw new Error("P2PDraftHost.buildLobbyView called before initialize()");
    }
    return {
      status: "Lobby",
      kind: this.kind,
      launch_capability: this.procedure.launch_capability,
      commanders_required: this.procedure.commanders_required,
      current_pack_number: 0,
      pick_number: 0,
      pass_direction: "Left",
      current_pack: null,
      // 0, not `procedure.cards_per_pick`: this mirrors exactly what
      // `filter_for_player` publishes for a seat with no pending pack. A
      // placeholder that disagrees with the real view is the [G6] defect class
      // this run has already paid for once.
      required_pick_count: 0,
      pick_selection_mode: this.procedure?.pick_selection_mode ?? "Direct",
      pool: [],
      draft_effects: [],
      pool_groups: EMPTY_DRAFT_POOL_GROUPS,
      seats: this.buildSeatPublicViews(),
      // NOT a `DraftProcedure` axis — the struct carries no pack-size field, so
      // this is a POOL property (`set_cards_per_pack` for a set pool,
      // `settings.cards_per_pack` for a cube), wrong for all five kinds equally
      // rather than wrong for CommanderDraft. This is a pre-draft placeholder
      // view that the real session view replaces once the draft starts; not a
      // missed kind-derived hardcode.
      cards_per_pack: LOBBY_PLACEHOLDER_CARDS_PER_PACK,
      // Mirrors what `filter_for_player` publishes for a session that has
      // opened nothing: `pack_size_sequence()` falls back to the uniform
      // `cards_per_pack` for every pack, and the lobby's own `cards_per_pack`
      // above is that uniform value. The length is read from the procedure
      // rather than hardcoded, so a kind whose pack count differs does not get
      // a 3-element array. A multi-set draft's real per-pack sizes replace
      // these the moment the draft starts.
      pack_sizes: Array<number>(this.procedure.packs_per_player).fill(
        LOBBY_PLACEHOLDER_CARDS_PER_PACK,
      ),
      // Empty strings, not fabricated codes: the lobby host has no
      // `DraftSource`, so there is no engine answer for which set fills each
      // booster. `""` is the one value no reachable producer publishes — every
      // path that fills `pack_set_code_sequence` names a real set or cube id —
      // so it cannot be read as an engine answer.
      pack_set_codes: Array<string>(this.procedure.packs_per_player).fill(""),
      // Zeroes, for the same reason the scalar below is 0: there is no
      // engine-derived step count to publish before a session exists, and 0 is
      // a value no reachable producer emits. Sized from the procedure so the
      // array agrees with `pack_count`.
      pack_pick_steps: Array<number>(this.procedure.packs_per_player).fill(0),
      // 0, not a step count: the lobby host has no session, and its own
      // `cards_per_pack` above is a POOL placeholder rather than a config
      // value — so there is no engine-derived answer to publish here. Deriving
      // one from `procedure.cards_per_pick` is refused: that is a second
      // authority for CR 903.13b's step rule in the display layer, the same
      // defect class as the seat-count literal this commit deletes above. `0`
      // is the one value no reachable producer publishes -- every path that
      // fills `DraftConfig.cards_per_pack` supplies at least 1 (an MTGJSON
      // slot-count sum, or the cube panel's clamped `min={1}`), so steps >= 1
      // -- and it therefore cannot be read as an engine answer. It is wrong
      // for all five kinds equally rather than right for four and wrong for
      // CommanderDraft.
      //
      // Note which precedent applies: `required_pick_count: 0` above justifies
      // its `0` as a value production DOES emit (a seat with no pending pack).
      // That ground does not transfer — there is no production state that
      // yields 0 steps. The ground here is `cards_per_pack`'s: an acknowledged
      // placeholder, wrong uniformly rather than kind-selectively.
      pick_steps_per_pack: 0,
      pack_count: this.procedure.packs_per_player,
      min_deck_size: this.procedure.min_deck_size,
      addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
      timer_remaining_ms: null,
      standings: [],
      current_round: 0,
      next_pairing_round: 1,
      tournament_format: "Swiss",
      pod_policy: "Competitive",
      pairings: [],
      match_config: this.procedure.match_config,
    };
  }

  /** Get the host's current view. */
  async getHostView(): Promise<DraftPlayerView> {
    if (!this.draftStarted) return this.buildLobbyView();
    return this.adapter.getViewForSeat(0);
  }

  /** Whether the draft pod is full. */
  get isFull(): boolean {
    return this.firstOpenSeat() === null;
  }

  /** Whether the draft has started. */
  get isStarted(): boolean {
    return this.draftStarted;
  }

  /** Whether the draft is paused. */
  get isPaused(): boolean {
    return this.paused;
  }

  /** The active timer type, if any. */
  get activeTimerContext(): "pick" | "sideboard" | "playdraw" | null {
    return this.timerContext;
  }
}
